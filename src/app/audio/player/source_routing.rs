use super::*;

pub(super) fn recreate_capture_stream(state: &mut LoopState) -> Result<(), String> {
    clear_virtual_mic_queues(&state.queues);

    let Some(core) = state.backend.as_ref().map(|backend| backend.core.clone()) else {
        return Ok(());
    };

    if let Some(backend) = state.backend.as_mut() {
        backend.capture_stream = None;
    }
    if !state.runtime.mic_passthrough {
        return Ok(());
    }

    let target = resolve_capture_target(state);
    let Some(target) = target else {
        warn!("No physical microphone source available for passthrough");
        return Ok(());
    };

    let capture_stream = create_capture_stream(
        core,
        state.queues.clone(),
        state.stream_runtime.clone(),
        &target,
        state.runtime.pipewire_latency_hint(),
    )?;
    if let Some(backend) = state.backend.as_mut() {
        backend.capture_stream = Some(capture_stream);
    }
    Ok(())
}

fn resolve_capture_target(state: &LoopState) -> Option<String> {
    if let Some(source) = state.runtime.mic_source.as_ref() {
        return state
            .sources
            .values()
            .find(|candidate| candidate.node_name == *source && !candidate.is_virtual)
            .map(|candidate| candidate.node_name.clone());
    }

    let default_source = current_default_source_name();
    if let Some(default_source) = default_source {
        if is_physical_source_name(&default_source, &state.sources) {
            return Some(default_source);
        }
    }

    state
        .previous_default_source_name
        .clone()
        .filter(|source_name| is_physical_source_name(source_name, &state.sources))
        .or_else(|| {
            state
                .sources
                .values()
                .find(|candidate| !candidate.is_monitor && !candidate.is_virtual)
                .map(|candidate| candidate.node_name.clone())
        })
}

fn is_physical_source_name(source_name: &str, sources: &HashMap<u32, SourceDescriptor>) -> bool {
    sources.values().any(|candidate| {
        candidate.node_name == source_name && !candidate.is_monitor && !candidate.is_virtual
    })
}

pub(super) fn apply_default_source_mode(state: &mut LoopState) -> Result<(), String> {
    match state.runtime.default_source_mode {
        DefaultSourceMode::Manual => restore_default_source(state),
        DefaultSourceMode::AutoWhileRunning => {
            maybe_claim_default_source(state);
            Ok(())
        }
    }
}

pub(super) fn maybe_claim_default_source(state: &mut LoopState) {
    if state.runtime.default_source_mode != DefaultSourceMode::AutoWhileRunning
        || state.claimed_default
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

    if state.previous_default_source_name.is_none() {
        if let Some(current_default) = current_default_source_name() {
            if current_default != VIRTUAL_SOURCE_NAME
                && is_physical_source_name(&current_default, &state.sources)
            {
                state.previous_default_source_name = Some(current_default);
            }
        }
    }

    if let Err(err) = set_default_source(virtual_source_id) {
        warn!("Failed to claim default source: {}", err);
        return;
    }

    state.claimed_default = true;
}

pub(super) fn restore_default_source(state: &mut LoopState) -> Result<(), String> {
    if !state.claimed_default {
        return Ok(());
    }

    if let Some(previous_name) = state.previous_default_source_name.clone() {
        if let Some(source_id) = resolve_source_id_by_name(&state.sources, &previous_name) {
            set_default_source(source_id)?;
        }
    }

    state.claimed_default = false;
    Ok(())
}

pub(super) fn resolve_source_id_by_name(
    sources: &HashMap<u32, SourceDescriptor>,
    node_name: &str,
) -> Option<u32> {
    sources
        .values()
        .find(|source| source.node_name == node_name)
        .map(|source| source.id)
}

fn set_default_source(source_id: u32) -> Result<(), String> {
    let output = Command::new("wpctl")
        .args(["set-default", &source_id.to_string()])
        .output()
        .map_err(|e| format!("Failed to run wpctl set-default: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn current_default_source_name() -> Option<String> {
    let output = Command::new("wpctl")
        .args(["inspect", "@DEFAULT_SOURCE@"]) 
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_wpctl_node_name(&String::from_utf8_lossy(&output.stdout))
}

pub(super) fn parse_wpctl_node_name(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| parse_wpctl_property_line(line, "node.name"))
}

fn parse_wpctl_property_line(line: &str, property: &str) -> Option<String> {
    let (_, value) = line.split_once(property)?;
    let (_, value) = value.split_once('=')?;
    let value = value.trim();
    let value = value.strip_prefix('"').unwrap_or(value);
    let value = value.strip_suffix('"').unwrap_or(value);
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}
