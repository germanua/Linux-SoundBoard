//! Owns the system default audio source.

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

/// Owns the default-metadata proxy and listener.
pub(super) struct DefaultMetadataHandle {
    pub(super) id: u32,
    metadata: pw::metadata::Metadata,
    _listener: Pin<Box<pw::metadata::MetadataListener>>,
}

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
            // Watch only subject 0's default.audio.source.
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
            info!(
                "Default source was cleared; re-asserting '{}'",
                VIRTUAL_SOURCE_NAME
            );
        }
    }

    if reclaim_strategy(new_name.as_deref()) == ReclaimStrategy::RuntimeKey {
        write_runtime_default_source(state);
    }
    // Keep WirePlumber's configured default on the virtual mic.
    claim_default_source_if_enabled(state);
}

fn already_holds_default(cached_name: Option<&str>, claimed: bool) -> bool {
    claimed && cached_name == Some(VIRTUAL_SOURCE_NAME)
}

pub(super) fn forget_default_source_belief(state: &mut LoopState, reason: &str) {
    info!("Default metadata {reason}; re-evaluating the default source");
    state.default_audio_source_name = None;
    state.claimed_default = false;
}

/// How to take the default source back.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ReclaimStrategy {
    ConfiguredClaim,
    RuntimeKey,
}

fn reclaim_strategy(new_name: Option<&str>) -> ReclaimStrategy {
    match new_name {
        Some(_) => ReclaimStrategy::ConfiguredClaim,
        None => ReclaimStrategy::RuntimeKey,
    }
}

fn default_source_metadata_value(name: &str) -> String {
    format!(r#"{{"name":"{name}"}}"#)
}

fn should_reclaim_default(mode: DefaultSourceMode, new_name: Option<&str>) -> bool {
    if mode == DefaultSourceMode::Manual {
        return false;
    }
    new_name != Some(VIRTUAL_SOURCE_NAME)
}

fn write_runtime_default_source(state: &LoopState) {
    let Some(handle) = state.default_metadata.as_ref() else {
        // Binding reapplies the claim later.
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

/// Claims or restores the system default for the selected mode.
pub(super) fn apply_default_source_mode(state: &mut LoopState) -> Result<(), EngineError> {
    match state.runtime.default_source_mode {
        DefaultSourceMode::Default => {
            claim_default_source_if_enabled(state);
            Ok(())
        }
        DefaultSourceMode::Manual => {
            // Restore only if we owned the claim.
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
