use super::*;

#[cfg(not(test))]
const WPCTL_COMMAND_TIMEOUT: Duration = Duration::from_millis(900);
#[cfg(not(test))]
const PACTL_COMMAND_TIMEOUT: Duration = Duration::from_millis(900);
#[cfg(not(test))]
const WPCTL_POLL_INTERVAL: Duration = Duration::from_millis(10);

// Patterns identifying virtual upstream-mic processors that Soundboard prefers
// over a raw physical mic. Matched as case-insensitive substrings against
// node.name and node.description. When the user explicitly selects one of these
// as `mic_source`, Soundboard refuses to silently fall back to the raw mic if
// the processor is offline (research §9.4) — the user picked it for a reason.
const ENHANCEMENT_SOURCE_PATTERNS: &[&str] = &[
    "easyeffects",
    "easy effects",
    "noisetorch",
    "noise_torch",
    "rnnoise",
    "noise-suppression",
    "noise_suppression",
];

pub(super) fn recreate_capture_stream(state: &mut LoopState) -> Result<(), EngineError> {
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
            if name_looks_like_enhancement_source(requested) {
                // The user explicitly selected an enhancement source (EasyEffects,
                // NoiseTorch, RNNoise, …). Refusing the silent fallback to the raw
                // mic is intentional: they picked it for a reason. Surface this
                // loudly so they know to start the upstream processor.
                warn!(
                    "Selected mic source '{}' is currently absent. Soundboard will NOT \
                     fall back to the raw microphone — start the upstream processor \
                     (EasyEffects, NoiseTorch, …) or pick a different source in settings.",
                    requested
                );
            } else {
                info!(
                    "Mic passthrough waiting for '{}' to appear in PipeWire graph",
                    requested
                );
            }
        } else if state
            .sources
            .values()
            .any(|s| !s.is_monitor && !s.is_our_virtual_mic)
        {
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
    // Explicit user selection always wins.
    if let Some(source) = state.runtime.mic_source.as_ref() {
        return state
            .sources
            .values()
            .find(|candidate| candidate.node_name == *source && upstream_source_allowed(candidate))
            .map(|candidate| candidate.node_name.clone());
    }

    // When no explicit source is set, let best_upstream_mic_source_name pick —
    // it ranks enhancement processors (EasyEffects, NoiseTorch, RNNoise, …) above
    // raw physical mics via enhancement_source_score. Checking default_source /
    // previous_default_source_name first would return the raw mic even when an
    // enhancement source is available, bypassing the user's audio processing chain.
    if let Some(best) = best_upstream_mic_source_name(&state.sources) {
        return Some(best);
    }

    // No sources registered yet — fall back to whatever PipeWire reports as the
    // current default, or the default we recorded before claiming it ourselves.
    if let Some(default_source) = default_source {
        if is_upstream_mic_source(&default_source, &state.sources) {
            return Some(default_source);
        }
    }

    state
        .previous_default_source_name
        .clone()
        .filter(|source_name| is_upstream_mic_source(source_name, &state.sources))
}

// True when `source_name` resolves to a tracked Audio/Source that is a valid
// upstream mic for Soundboard's capture stream. "Upstream" covers BOTH physical
// mics (alsa_input.…) AND virtual processor sources (easyeffects_source,
// noisetorch, …) — what they share is "user voice flowing IN to Soundboard".
fn is_upstream_mic_source(source_name: &str, sources: &HashMap<u32, SourceDescriptor>) -> bool {
    sources.values().any(|candidate| {
        candidate.node_name == source_name
            && upstream_source_allowed(candidate)
            && !is_loopback_sink(candidate)
    })
}

pub(super) fn best_upstream_mic_source_name(
    sources: &HashMap<u32, SourceDescriptor>,
) -> Option<String> {
    sources
        .values()
        .filter(|candidate| upstream_source_allowed(candidate) && !is_loopback_sink(candidate))
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

// Excludes monitor sources (sink monitors aren't real mics) and Soundboard's
// own virtual mic (would create a feedback loop).
fn upstream_source_allowed(source: &SourceDescriptor) -> bool {
    !source.is_monitor && !source.is_our_virtual_mic
}

// Score a source's likelihood of being a user-deployed mic enhancement chain.
// Three independent signals are combined additively so misconfiguration of any
// one signal still leaves the other two to reach the correct decision:
//
//   Signal A (structural):     no hardware device.api set
//                               (PipeWire's device factory sets this on alsa/bluez5/v4l2
//                                sources; software null-sinks/loopbacks/filter-chains
//                                never have it. Not user-editable in normal configs.)
//   Signal B (class):          media.class == "Audio/Source/Virtual"
//                               (PipeWire-standard. Configurable in some modules.)
//   Signal C (name fallback):  matches a known enhancement-app name
//                               (last-resort backup for legacy tools.)
//
// Score range 0..=3. A score-0 source is a clear physical mic; ≥1 is preferred over
// any physical mic. For a physical mic to win against an enhancement source, ALL
// THREE signals would have to disagree — essentially impossible in practice.
fn enhancement_source_score(source: &SourceDescriptor) -> u8 {
    // A null-sink loopback source (screenshare/virtual cable) is not a mic
    // enhancement chain; never let it out-score a real mic. Defensive — auto-detect
    // already filters these out via is_loopback_sink before scoring.
    if is_loopback_sink(source) {
        return 0;
    }

    let signal_a = !source.is_hardware_backed;
    let signal_b = source.is_virtual;

    let mut score: u8 = match (signal_a, signal_b) {
        (true, true) => 2,
        (true, false) | (false, true) => 1,
        (false, false) => 0,
    };

    let node_name = source.node_name.to_ascii_lowercase();
    let display_name = source.display_name.to_ascii_lowercase();
    let name_match = ENHANCEMENT_SOURCE_PATTERNS
        .iter()
        .any(|needle| node_name.contains(needle) || display_name.contains(needle));
    if name_match {
        score = score.saturating_add(1);
    }
    score
}

fn name_looks_like_enhancement_source(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ENHANCEMENT_SOURCE_PATTERNS
        .iter()
        .any(|needle| name.contains(needle))
}

// A PipeWire null sink (loopback/aggregation target) that does NOT also name-match
// a known enhancement app. Null sinks carry application audio, not a mic
// (Vencord/Discord screenshare, OBS virtual audio, virtual cables), so they are
// excluded from mic auto-detect. A null sink whose name DOES match an enhancement
// app (Signal C) is still trusted and NOT excluded — the user's setup wins.
fn is_loopback_sink(source: &SourceDescriptor) -> bool {
    source.is_null_sink_backed
        && !name_looks_like_enhancement_source(&source.node_name)
        && !name_looks_like_enhancement_source(&source.display_name)
}

pub(super) fn restore_default_source(state: &mut LoopState) -> Result<(), EngineError> {
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
        } else {
            warn!(
                "Previous default source '{}' is no longer available; leaving current default source unchanged",
                previous_name
            );
        }
    }

    state.claimed_default = false;
    Ok(())
}

#[cfg(not(test))]
pub(super) fn spawn_default_source_claim(
    in_flight: std::sync::Arc<AtomicBool>,
    virtual_source_id: u32,
) {
    if in_flight.swap(true, Ordering::Relaxed) {
        return;
    }
    let worker_in_flight = in_flight.clone();
    if thread::Builder::new()
        .name("linux-soundboard-default-source".to_string())
        .spawn(move || {
            let wpctl_outcome = set_default_source(virtual_source_id);
            let wpctl_err = wpctl_outcome.as_ref().err().map(|e| e.to_string());
            crate::diagnostics::audit::record_default_source_command(
                "default_source.claim",
                Some(virtual_source_id),
                Some(VIRTUAL_SOURCE_NAME),
                wpctl_err.as_deref().map_or(Ok(()), Err),
            );
            if let Err(err) = &wpctl_outcome {
                warn!("Failed to claim default source: {}", err);
            }
            let pactl_outcome = set_pulse_default_source(VIRTUAL_SOURCE_NAME);
            let pactl_err = pactl_outcome.as_ref().err().map(|e| e.to_string());
            crate::diagnostics::audit::record_default_source_command(
                "default_source.pulse_claim",
                Some(virtual_source_id),
                Some(VIRTUAL_SOURCE_NAME),
                pactl_err.as_deref().map_or(Ok(()), Err),
            );
            if let Err(err) = &pactl_outcome {
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

#[cfg(test)]
pub(super) fn spawn_default_source_claim(
    in_flight: std::sync::Arc<AtomicBool>,
    _virtual_source_id: u32,
) {
    in_flight.store(false, Ordering::Relaxed);
}

#[cfg(not(test))]
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
            let wpctl_outcome = set_default_source(source_id);
            let wpctl_err = wpctl_outcome.as_ref().err().map(|e| e.to_string());
            crate::diagnostics::audit::record_default_source_command(
                "default_source.restore",
                Some(source_id),
                Some(source_name.as_str()),
                wpctl_err.as_deref().map_or(Ok(()), Err),
            );
            if let Err(err) = &wpctl_outcome {
                warn!("Failed to restore default source: {}", err);
            }
            let pactl_outcome = set_pulse_default_source(&source_name);
            let pactl_err = pactl_outcome.as_ref().err().map(|e| e.to_string());
            crate::diagnostics::audit::record_default_source_command(
                "default_source.pulse_restore",
                Some(source_id),
                Some(source_name.as_str()),
                pactl_err.as_deref().map_or(Ok(()), Err),
            );
            if let Err(err) = &pactl_outcome {
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

#[cfg(test)]
fn spawn_default_source_restore(
    in_flight: std::sync::Arc<AtomicBool>,
    _source_id: u32,
    _source_name: String,
) {
    in_flight.store(false, Ordering::Relaxed);
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

#[cfg(not(test))]
fn set_default_source(source_id: u32) -> Result<(), EngineError> {
    let source_id = source_id.to_string();
    let output = run_wpctl_with_timeout(["set-default", source_id.as_str()])?;
    if output.status.success() {
        Ok(())
    } else {
        let detail = command_output_detail(&output);
        if detail.is_empty() {
            Err(EngineError::Routing(
                "wpctl set-default failed without stderr output".to_string(),
            ))
        } else {
            Err(EngineError::Routing(detail))
        }
    }
}

#[cfg(not(test))]
fn set_pulse_default_source(source_name: &str) -> Result<(), EngineError> {
    let output = run_pactl_with_timeout(["set-default-source", source_name])?;
    if output.status.success() {
        Ok(())
    } else {
        let detail = command_output_detail(&output);
        if detail.is_empty() {
            Err(EngineError::Routing(
                "pactl set-default-source failed without stderr output".to_string(),
            ))
        } else {
            Err(EngineError::Routing(detail))
        }
    }
}

#[cfg(not(test))]
fn run_wpctl_with_timeout<const N: usize>(
    args: [&str; N],
) -> Result<std::process::Output, EngineError> {
    run_command_with_timeout("wpctl", &args, WPCTL_COMMAND_TIMEOUT)
}

#[cfg(not(test))]
pub(super) fn run_pactl_with_timeout<const N: usize>(
    args: [&str; N],
) -> Result<std::process::Output, EngineError> {
    run_command_with_timeout("pactl", &args, PACTL_COMMAND_TIMEOUT)
}

#[cfg(not(test))]
pub(super) fn run_command_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<std::process::Output, EngineError> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            EngineError::Routing(format!("Failed to run {} {}: {e}", program, args.join(" ")))
        })?;

    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child.wait_with_output().map_err(|e| {
                    EngineError::Routing(format!("Failed to collect {} output: {e}", program))
                });
            }
            Ok(None) => {
                if started_at.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(EngineError::Routing(format!(
                        "{} {} timed out after {} ms",
                        program,
                        args.join(" "),
                        timeout.as_millis()
                    )));
                }
                thread::sleep(WPCTL_POLL_INTERVAL);
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(EngineError::Routing(format!(
                    "Failed while waiting for {}: {e}",
                    program
                )));
            }
        }
    }
}

#[cfg(not(test))]
pub(super) fn command_output_detail(output: &std::process::Output) -> String {
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
