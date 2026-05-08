use super::*;

const WPCTL_COMMAND_TIMEOUT: Duration = Duration::from_millis(900);
const PACTL_COMMAND_TIMEOUT: Duration = Duration::from_millis(900);
const WPCTL_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(super) fn recreate_capture_stream(state: &mut LoopState) -> Result<(), String> {
    clear_mic_input_queue(&state.queues);
    state.capture_health_miss_ticks = 0;

    if matches!(state.backend, Some(BackendState::PulseAudio(_))) {
        let runtime = state.runtime.clone();
        if let Some(BackendState::PulseAudio(backend)) = state.backend.as_mut() {
            return backend.recreate_capture_stream(&runtime);
        }
    }

    let Some(core) = state.backend.as_ref().and_then(BackendState::pipewire_core) else {
        return Ok(());
    };

    if let Some(BackendState::PipeWire(backend)) = state.backend.as_mut() {
        if let Some(capture_stream) = backend.capture_stream.take() {
            drop(capture_stream);
        }
    }
    state.active_capture_target = None;
    state.capture_node_id = None;
    if !state.runtime.mic_passthrough {
        return Ok(());
    }

    let target = resolve_capture_target(state);
    let Some(target) = target else {
        if let Some(requested) = state.runtime.mic_source.as_deref() {
            info!(
                "Mic passthrough waiting for '{}' to appear in PipeWire graph",
                requested
            );
        } else if state.sources.values().any(|s| !s.is_monitor && !s.is_our_virtual_mic) {
            info!("Mic passthrough: all available sources are monitors or virtual; waiting for a physical microphone");
        } else {
            info!("Mic passthrough: no microphone found — will activate automatically when one is connected");
        }
        return Ok(());
    };

    info!("Connecting mic passthrough capture to {}", target);
    let capture_stream = create_capture_stream(
        core,
        state.queues.clone(),
        state.stream_runtime.clone(),
        &target,
        state.runtime.pipewire_latency_hint(),
    )?;
    state.active_capture_target = Some(target);
    if let Some(BackendState::PipeWire(backend)) = state.backend.as_mut() {
        backend.capture_stream = Some(capture_stream);
    }
    Ok(())
}

pub(super) fn resolve_capture_target(state: &LoopState) -> Option<String> {
    resolve_capture_target_from_default(state, state.previous_default_source_name.clone())
}

pub(super) fn resolve_capture_target_from_default(
    state: &LoopState,
    default_source: Option<String>,
) -> Option<String> {
    if let Some(source) = state.runtime.mic_source.as_ref() {
        return state
            .sources
            .values()
            .find(|candidate| candidate.node_name == *source && capture_source_allowed(candidate))
            .map(|candidate| candidate.node_name.clone());
    }

    if let Some(default_source) = default_source {
        if is_physical_source_name(&default_source, &state.sources) {
            return Some(default_source);
        }
    }

    state
        .previous_default_source_name
        .clone()
        .filter(|source_name| is_physical_source_name(source_name, &state.sources))
        .or_else(|| best_fallback_source_name(&state.sources))
}

fn is_physical_source_name(source_name: &str, sources: &HashMap<u32, SourceDescriptor>) -> bool {
    sources
        .values()
        .any(|candidate| candidate.node_name == source_name && capture_source_allowed(candidate))
}

pub(super) fn best_fallback_source_name(
    sources: &HashMap<u32, SourceDescriptor>,
) -> Option<String> {
    sources
        .values()
        .filter(|candidate| capture_source_allowed(candidate))
        .max_by(|left, right| {
            enhancement_source_score(left)
                .cmp(&enhancement_source_score(right))
                .then_with(|| left.priority_session.cmp(&right.priority_session))
                .then_with(|| left.display_name.cmp(&right.display_name))
                .then_with(|| left.node_name.cmp(&right.node_name))
                .then_with(|| left.id.cmp(&right.id))
        })
        .map(|candidate| candidate.node_name.clone())
}

fn capture_source_allowed(source: &SourceDescriptor) -> bool {
    !source.is_monitor && !source.is_our_virtual_mic
}

fn enhancement_source_score(source: &SourceDescriptor) -> u8 {
    let node_name = source.node_name.to_ascii_lowercase();
    let display_name = source.display_name.to_ascii_lowercase();
    let is_enhancement = [
        "easyeffects",
        "easy effects",
        "noisetorch",
        "noise_torch",
        "rnnoise",
        "noise-suppression",
        "noise_suppression",
    ]
    .iter()
    .any(|needle| node_name.contains(needle) || display_name.contains(needle));

    u8::from(is_enhancement)
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
        || state
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

    if state.previous_default_source_name.is_none() {
        state.previous_default_source_name = best_fallback_source_name(&state.sources);
    }

    spawn_default_source_claim(
        state.default_source_command_in_flight.clone(),
        virtual_source_id,
    );
    state.claimed_default = true;
}

pub(super) fn restore_default_source(state: &mut LoopState) -> Result<(), String> {
    if !state.claimed_default {
        return Ok(());
    }

    if let Some(previous_name) = state.previous_default_source_name.clone() {
        if let Some(source_id) = resolve_source_id_by_name(&state.sources, &previous_name) {
            spawn_default_source_restore(
                state.default_source_command_in_flight.clone(),
                source_id,
                previous_name,
            );
        }
    }

    state.claimed_default = false;
    Ok(())
}

fn spawn_default_source_claim(in_flight: std::sync::Arc<AtomicBool>, virtual_source_id: u32) {
    if in_flight.swap(true, Ordering::Relaxed) {
        return;
    }
    let worker_in_flight = in_flight.clone();
    if thread::Builder::new()
        .name("linux-soundboard-default-source".to_string())
        .spawn(move || {
            if let Err(err) = set_default_source(virtual_source_id) {
                warn!("Failed to claim default source: {}", err);
            }
            if let Err(err) = set_pulse_default_source(VIRTUAL_SOURCE_NAME) {
                warn!("Failed to claim PulseAudio default source: {}", err);
            }
            worker_in_flight.store(false, Ordering::Relaxed);
        })
        .is_err()
    {
        in_flight.store(false, Ordering::Relaxed);
        warn!("Failed to spawn default-source claim worker");
    }
}

fn spawn_default_source_restore(
    in_flight: std::sync::Arc<AtomicBool>,
    source_id: u32,
    source_name: String,
) {
    if in_flight.swap(true, Ordering::Relaxed) {
        return;
    }
    let worker_in_flight = in_flight.clone();
    if thread::Builder::new()
        .name("linux-soundboard-restore-source".to_string())
        .spawn(move || {
            if let Err(err) = set_default_source(source_id) {
                warn!("Failed to restore default source: {}", err);
            }
            if let Err(err) = set_pulse_default_source(&source_name) {
                warn!("Failed to restore PulseAudio default source: {}", err);
            }
            worker_in_flight.store(false, Ordering::Relaxed);
        })
        .is_err()
    {
        in_flight.store(false, Ordering::Relaxed);
        warn!("Failed to spawn default-source restore worker");
    }
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
    let source_id = source_id.to_string();
    let output = run_wpctl_with_timeout(["set-default", source_id.as_str()])?;
    if output.status.success() {
        Ok(())
    } else {
        let detail = command_output_detail(&output);
        if detail.is_empty() {
            Err("wpctl set-default failed without stderr output".to_string())
        } else {
            Err(detail)
        }
    }
}

fn set_pulse_default_source(source_name: &str) -> Result<(), String> {
    let output = run_pactl_with_timeout(["set-default-source", source_name])?;
    if output.status.success() {
        Ok(())
    } else {
        let detail = command_output_detail(&output);
        if detail.is_empty() {
            Err("pactl set-default-source failed without stderr output".to_string())
        } else {
            Err(detail)
        }
    }
}

fn run_wpctl_with_timeout<const N: usize>(args: [&str; N]) -> Result<std::process::Output, String> {
    run_command_with_timeout("wpctl", &args, WPCTL_COMMAND_TIMEOUT)
}

fn run_pactl_with_timeout<const N: usize>(args: [&str; N]) -> Result<std::process::Output, String> {
    run_command_with_timeout("pactl", &args, PACTL_COMMAND_TIMEOUT)
}

fn run_command_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run {} {}: {e}", program, args.join(" ")))?;

    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|e| format!("Failed to collect {} output: {e}", program));
            }
            Ok(None) => {
                if started_at.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "{} {} timed out after {} ms",
                        program,
                        args.join(" "),
                        timeout.as_millis()
                    ));
                }
                thread::sleep(WPCTL_POLL_INTERVAL);
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Failed while waiting for {}: {e}", program));
            }
        }
    }
}

fn command_output_detail(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }

    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[cfg(test)]
pub(super) fn parse_wpctl_node_name(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| parse_wpctl_property_line(line, "node.name"))
}

#[cfg(test)]
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
