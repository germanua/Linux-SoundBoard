//! Default-source ownership.
//!
//! The soundboard works by making `linuxsoundboard.virtual_mic` the system
//! default microphone (same pattern EasyEffects uses for `easyeffects_source`).
//! Apps that don't have an explicit per-stream device picked just see the
//! default and use it — no per-stream metadata writes, no fight with
//! WirePlumber's stream-restore module.
//!
//! No system config files are written by this module. We only call the
//! PipeWire/Pulse APIs (`wpctl set-default` and `pactl set-default-source`)
//! whose persistence is handled internally by WirePlumber's `default-nodes`
//! module — the same mechanism `pavucontrol` uses when the user picks a
//! default in the GUI.
//!
//! On engine shutdown the default is intentionally NOT reverted. The whole
//! point of the design is that virtual_mic stays the default across engine
//! restarts. Uninstall is the only path that restores the pre-install default
//! (handled by `install-user.sh`).

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

/// Owning handle for the PipeWire "default" metadata proxy + its listener.
/// Dropping this disconnects everything cleanly.
pub(super) struct DefaultMetadataHandle {
    pub(super) id: u32,
    // The PipeWire metadata proxy. Held here so the listener stays alive for
    // its lifetime.
    _metadata: pw::metadata::Metadata,
    _listener: Pin<Box<pw::metadata::MetadataListener>>,
}

/// Try to bind a registry global as the `default` metadata object.
///
/// Returns `Some(handle)` only when this global is the system's "default"
/// metadata (the one carrying `default.audio.source`/`default.audio.sink`).
/// On binding, we install a property-change listener that triggers a re-claim
/// when the default source drifts away from our virtual mic (matching
/// EasyEffects' behaviour).
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
        _metadata: metadata,
        _listener: listener,
    })
}

/// React to the system's default source being changed (by us, by EasyEffects,
/// or by the user in pavucontrol).
///
/// `value` is the raw JSON the metadata property carries — something like
/// `{"name":"easyeffects_source"}`. We extract the `name` field.
pub(super) fn handle_default_source_metadata_change(state: &mut LoopState, value: Option<&str>) {
    let new_name = value.and_then(parse_default_source_name);
    state.default_audio_source_name = new_name.clone();

    // If the user has chosen `Manual`, respect their pick and don't fight.
    if state.runtime.default_source_mode == DefaultSourceMode::Manual {
        return;
    }

    let Some(name) = new_name.as_deref() else {
        return;
    };
    // If virtual_mic is already the default, nothing to do.
    if name == VIRTUAL_SOURCE_NAME {
        return;
    }

    // Something else became the default. Re-assert.
    info!(
        "Default source changed externally to '{}'; re-asserting '{}'",
        name, VIRTUAL_SOURCE_NAME
    );
    // Remember whatever was here BEFORE us — for uninstall restore.
    if state.previous_default_source_name.is_none() {
        state.previous_default_source_name = Some(name.to_string());
    }
    claim_default_source_if_enabled(state);
}

/// Parse the JSON-shaped metadata value into the source's node name.
///
/// PipeWire's `default.audio.source` value is a small JSON object like
/// `{"name":"alsa_input.usb-foo"}`. We avoid pulling a full JSON dependency
/// just for this single field — a minimal hand parse is fine.
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

    // If the current known default is already us, no-op.
    if state.default_audio_source_name.as_deref() == Some(VIRTUAL_SOURCE_NAME)
        && state.claimed_default
    {
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

/// Apply a freshly-changed `DefaultSourceMode` to the running engine.
///
/// `Default` → claim virtual_mic. `Manual` → restore previous default if we
/// had claimed it ourselves; otherwise leave the user's choice alone.
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
}
