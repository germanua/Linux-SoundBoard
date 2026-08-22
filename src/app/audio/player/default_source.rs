//! Default-source ownership.
//!
//! We make `linuxsoundboard.virtual_mic` the system default mic, the same way
//! EasyEffects does with `easyeffects_source`. Apps with no explicit device
//! just take the default — no per-stream metadata writes, no fight with
//! stream-restore.
//!
//! API calls only (`wpctl set-default`, `pactl set-default-source`); persistence
//! is WirePlumber's `default-nodes`, same path pavucontrol uses. Shutdown does
//! NOT revert — staying default across engine restarts is the point. Only
//! uninstall restores the old default, in `install-user.sh`.

use std::pin::Pin;
use std::sync::atomic::Ordering;

use log::info;
use pipewire as pw;
use pw::registry::GlobalObject;
use pw::types::ObjectType;

use crate::app_meta::VIRTUAL_SOURCE_NAME;
use crate::config::DefaultSourceMode;

use super::source_routing::spawn_default_source_claim;
use super::EngineError;
use super::LoopState;

const DEFAULT_AUDIO_SOURCE_KEY: &str = "default.audio.source";
/// PipeWire carries the system-wide defaults on subject 0.
const DEFAULT_METADATA_SUBJECT: u32 = 0;
const DEFAULT_AUDIO_SOURCE_TYPE: &str = "Spa:String:JSON";

/// Owning handle for the PipeWire "default" metadata proxy + its listener.
/// Dropping this disconnects everything cleanly.
pub(super) struct DefaultMetadataHandle {
    pub(super) id: u32,
    // The PipeWire metadata proxy. Keeps the listener alive for its lifetime,
    // and lets the engine write the runtime default key itself.
    metadata: pw::metadata::Metadata,
    _listener: Pin<Box<pw::metadata::MetadataListener>>,
}

/// Bind a registry global as the `default` metadata object, if that is what it
/// is — the one carrying `default.audio.source`/`default.audio.sink`. Installs
/// a property listener that re-claims when the default drifts off our mic.
pub(super) fn bind_default_metadata_from_global(
    registry: &pw::registry::RegistryRc,
    global: &GlobalObject<&pw::spa::utils::dict::DictRef>,
    weak_state: std::rc::Weak<std::cell::RefCell<LoopState>>,
) -> Option<DefaultMetadataHandle> {
    if global.type_ != ObjectType::Metadata {
        return None;
    }
    let props = global.props.as_ref()?;
    let name = props.get("metadata.name")?;
    if name != "default" {
        return None;
    }

    let metadata: pw::metadata::Metadata = registry.bind(global).ok()?;

    let global_id = global.id;
    // Pre-allocate the boxed listener so we can pin it for the C ABI.
    let listener = metadata
        .add_listener_local()
        .property(move |subject, key, _type_, value| {
            // Only care about the system-default subject (id=0) +
            // default.audio.source. Other metadata properties are noise here.
            if subject != 0 {
                return 0;
            }
            let Some(key) = key else { return 0 };
            if key != DEFAULT_AUDIO_SOURCE_KEY {
                return 0;
            }

            if let Some(state) = weak_state.upgrade() {
                let mut state = state.borrow_mut();
                handle_default_source_metadata_change(&mut state, value);
            }
            0
        })
        .register();
    let listener = Box::pin(listener);

    Some(DefaultMetadataHandle {
        id: global_id,
        metadata,
        _listener: listener,
    })
}

/// The default source changed — by us, by EasyEffects, or by hand in
/// pavucontrol. `value` is the raw property JSON, `{"name":"…"}`.
pub(super) fn handle_default_source_metadata_change(state: &mut LoopState, value: Option<&str>) {
    let new_name = value.and_then(parse_default_source_name);
    state.default_audio_source_name = new_name.clone();

    if !should_reclaim_default(state.runtime.default_source_mode, new_name.as_deref()) {
        return;
    }

    match new_name.as_deref() {
        Some(name) => {
            info!(
                "Default source changed externally to '{}'; re-asserting '{}'",
                name, VIRTUAL_SOURCE_NAME
            );
            // Remember whatever was here BEFORE us — for uninstall restore.
            if state.previous_default_source_name.is_none() {
                state.previous_default_source_name = Some(name.to_string());
            }
        }
        None => {
            // Nothing to remember for a restore: the system is not pointing at
            // a device the user picked, it is pointing at nothing.
            info!(
                "Default source was cleared; re-asserting '{}'",
                VIRTUAL_SOURCE_NAME
            );
        }
    }

    if reclaim_strategy(new_name.as_deref()) == ReclaimStrategy::RuntimeKey {
        write_runtime_default_source(state);
    }
    // Keep the configured key pointing at us as well, so a later re-derive by
    // WirePlumber lands on the virtual mic rather than a priority pick.
    claim_default_source_if_enabled(state);
}

/// Already ours, so no need to claim. Both halves matter: the cached name only
/// counts for a claim this engine saw confirmed, and the flag only counts while
/// that name still says virtual_mic.
fn already_holds_default(cached_name: Option<&str>, claimed: bool) -> bool {
    claimed && cached_name == Some(VIRTUAL_SOURCE_NAME)
}

/// Forget what we think the default is. Everything we know came from one
/// metadata object's property events; once that object is gone the belief is
/// about nothing, and a fresh one replays no properties — keep it and the claim
/// never fires again.
pub(super) fn forget_default_source_belief(state: &mut LoopState, reason: &str) {
    // Both silent failures this caused were miserable to diagnose precisely
    // because nothing logged that the engine had gone blind to the default.
    info!("Default metadata {reason}; re-evaluating the default source");
    state.default_audio_source_name = None;
    state.claimed_default = false;
}

/// How to take the default source back.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ReclaimStrategy {
    /// Another device holds it. `wpctl`/`pactl` write
    /// `default.configured.audio.source`, and changing it makes WirePlumber
    /// re-derive the runtime key.
    ConfiguredClaim,
    /// Nothing holds it: the runtime key is gone while the configured key can
    /// still name us. Rewriting the configured key with the value it already
    /// has changes nothing, so the runtime key has to be written directly.
    RuntimeKey,
}

fn reclaim_strategy(new_name: Option<&str>) -> ReclaimStrategy {
    match new_name {
        Some(_) => ReclaimStrategy::ConfiguredClaim,
        None => ReclaimStrategy::RuntimeKey,
    }
}

/// The JSON shape PipeWire carries in `default.audio.source`, matching what
/// [`parse_default_source_name`] reads back.
fn default_source_metadata_value(name: &str) -> String {
    format!(r#"{{"name":"{name}"}}"#)
}

/// Should we claim, given the mode and what the system now reports?
///
/// `None` means the property was deleted, not that all is well: with nothing
/// set PipeWire falls back to its highest-ranked node, which isn't us. Reclaim
/// it like any foreign device.
fn should_reclaim_default(mode: DefaultSourceMode, new_name: Option<&str>) -> bool {
    if mode == DefaultSourceMode::Manual {
        return false;
    }
    new_name != Some(VIRTUAL_SOURCE_NAME)
}

/// Write `default.audio.source` through the metadata proxy we already hold.
/// Called from the property callback on the loop thread; the write queues to
/// the server instead of re-entering, and the event it comes back as ends the
/// reclaim.
fn write_runtime_default_source(state: &LoopState) {
    let Some(handle) = state.default_metadata.as_ref() else {
        // No metadata bound yet; the claim below still sets the configured key,
        // and binding re-runs the claim.
        return;
    };
    let value = default_source_metadata_value(VIRTUAL_SOURCE_NAME);
    handle.metadata.set_property(
        DEFAULT_METADATA_SUBJECT,
        DEFAULT_AUDIO_SOURCE_KEY,
        Some(DEFAULT_AUDIO_SOURCE_TYPE),
        Some(&value),
    );
}

/// Node name out of `{"name":"alsa_input.usb-foo"}`. Hand-parsed — one field
/// is not worth a JSON dependency.
fn parse_default_source_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let needle = "\"name\"";
    let idx = trimmed.find(needle)?;
    let after = &trimmed[idx + needle.len()..];
    let colon = after.find(':')?;
    let after = &after[colon + 1..];
    let quote_start = after.find('"')?;
    let after = &after[quote_start + 1..];
    let quote_end = after.find('"')?;
    Some(after[..quote_end].to_string())
}

/// Idempotently claim the default source when we're allowed to. Called on
/// engine start, when virtual_mic first appears in the registry, and when the
/// default metadata changes externally.
pub(super) fn claim_default_source_if_enabled(state: &mut LoopState) {
    if state.runtime.default_source_mode != DefaultSourceMode::Default {
        return;
    }
    if state
        .default_source_command_in_flight
        .load(Ordering::Relaxed)
    {
        return;
    }
    let Some(virtual_source_id) = state
        .sources
        .values()
        .find(|source| source.node_name == VIRTUAL_SOURCE_NAME)
        .map(|source| source.id)
    else {
        // virtual_mic isn't visible yet (engine startup race or PipeWire
        // restart in progress). The registry listener will re-call us when
        // it appears.
        return;
    };

    if already_holds_default(
        state.default_audio_source_name.as_deref(),
        state.claimed_default,
    ) {
        return;
    }

    // Remember whatever the user had before so uninstall can restore it.
    if state.previous_default_source_name.is_none() {
        state.previous_default_source_name = state
            .default_audio_source_name
            .clone()
            .filter(|name| name != VIRTUAL_SOURCE_NAME);
    }

    spawn_default_source_claim(
        state.default_source_command_in_flight.clone(),
        virtual_source_id,
    );
    state.claimed_default = true;
}

/// `Default` claims virtual_mic; `Manual` restores the previous default if we
/// were the ones holding it, otherwise leaves the user's choice alone.
pub(super) fn apply_default_source_mode(state: &mut LoopState) -> Result<(), EngineError> {
    match state.runtime.default_source_mode {
        DefaultSourceMode::Default => {
            claim_default_source_if_enabled(state);
            Ok(())
        }
        DefaultSourceMode::Manual => {
            // Drop our claim if we had one. Restore handled by
            // source_routing::restore_default_source.
            super::source_routing::restore_default_source(state)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_audio_source_value() {
        let raw = r#"{"name":"easyeffects_source"}"#;
        assert_eq!(
            parse_default_source_name(raw).as_deref(),
            Some("easyeffects_source")
        );
    }

    #[test]
    fn parses_with_whitespace_and_extra_fields() {
        let raw = r#"   { "name" :  "alsa_input.usb-x" , "extra": 1 }  "#;
        assert_eq!(
            parse_default_source_name(raw).as_deref(),
            Some("alsa_input.usb-x")
        );
    }

    #[test]
    fn parse_default_audio_source_returns_none_on_malformed() {
        assert_eq!(parse_default_source_name("not json"), None);
        assert_eq!(parse_default_source_name("{}"), None);
        assert_eq!(parse_default_source_name(r#"{"name":"#), None);
    }

    #[test]
    fn a_cleared_default_is_reclaimed() {
        // PipeWire deletes the property instead of reassigning it when the
        // node disappears — every engine restart, since the mic is recreated.
        // Treating that as "nothing to do" left us silently not the default.
        assert!(should_reclaim_default(DefaultSourceMode::Default, None));
    }

    #[test]
    fn a_foreign_default_is_reclaimed() {
        assert!(should_reclaim_default(
            DefaultSourceMode::Default,
            Some("alsa_input.pci-0000_12_00.6.analog-stereo")
        ));
    }

    #[test]
    fn our_own_default_is_left_alone() {
        assert!(!should_reclaim_default(
            DefaultSourceMode::Default,
            Some(VIRTUAL_SOURCE_NAME)
        ));
    }

    #[test]
    fn a_confirmed_claim_is_not_repeated() {
        assert!(already_holds_default(Some(VIRTUAL_SOURCE_NAME), true));
    }

    #[test]
    fn a_forgotten_belief_claims_again() {
        // PipeWire can swap the whole metadata object rather than touch a
        // property. A fresh one fires no events, so trusting the old belief
        // means sitting silent while the system has no default at all.
        assert!(!already_holds_default(None, false));
    }

    #[test]
    fn a_claim_we_never_saw_confirmed_is_not_trusted() {
        assert!(!already_holds_default(Some(VIRTUAL_SOURCE_NAME), false));
    }

    #[test]
    fn another_device_holding_it_is_not_us() {
        assert!(!already_holds_default(
            Some("alsa_input.pci-0000_12_00.6.analog-stereo"),
            true
        ));
    }

    #[test]
    fn a_cleared_default_writes_the_runtime_key() {
        // wpctl and pactl both write default.configured.audio.source. If that
        // already names us and only the runtime key is gone, rewriting it is a
        // no-op, WirePlumber never re-derives, and we stay on the fallback.
        assert_eq!(reclaim_strategy(None), ReclaimStrategy::RuntimeKey);
    }

    #[test]
    fn a_foreign_default_uses_the_configured_claim() {
        assert_eq!(
            reclaim_strategy(Some("alsa_input.pci-0000_12_00.6.analog-stereo")),
            ReclaimStrategy::ConfiguredClaim
        );
    }

    #[test]
    fn the_runtime_key_we_write_is_the_shape_we_read() {
        let written = default_source_metadata_value(VIRTUAL_SOURCE_NAME);
        assert_eq!(
            parse_default_source_name(&written).as_deref(),
            Some(VIRTUAL_SOURCE_NAME)
        );
    }

    #[test]
    fn manual_mode_never_reclaims() {
        assert!(!should_reclaim_default(DefaultSourceMode::Manual, None));
        assert!(!should_reclaim_default(
            DefaultSourceMode::Manual,
            Some("alsa_input.pci-0000_12_00.6.analog-stereo")
        ));
    }
}
