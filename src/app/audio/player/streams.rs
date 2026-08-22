use super::*;
use std::sync::Arc;

pub(super) fn create_local_output_stream(
    core: pw::core::CoreRc,
    queues: RtSharedQueues,
    stream_runtime: Arc<StreamRuntimeShared>,
    latency_hint: &str,
) -> Result<StreamHandle, EngineError> {
    let stream = pw::stream::StreamRc::new(
        core,
        LOCAL_PLAYBACK_NODE_NAME,
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Music",
            *pw::keys::NODE_NAME => LOCAL_PLAYBACK_NODE_NAME,
            *pw::keys::NODE_DESCRIPTION => VIRTUAL_OUTPUT_DESCRIPTION,
            // Keep our local + feeder + capture streams under one driver so
            // Discord/screen-share opening a new consumer does not split our
            // streams across two driver clocks (see PipeWire `node.group`).
            "node.group" => SOUNDBOARD_NODE_GROUP,
            // Run the process callback every quantum even when the sink is
            // briefly suspended (BT autosuspend, USB selective suspend). The
            // wake-up burst on resume is otherwise audible as a brief glitch.
            "node.always-process" => "true",
            // Prevent WirePlumber from renegotiating the quantum/rate when
            // Discord enters the graph. JACK clients use these properties to
            // hold their negotiated period; we need the same protection.
            "node.lock-quantum" => "true",
            "node.lock-rate" => "true",
            // Declare a hint so the sink does not pick a much smaller quantum
            // than the soundboard's queue target — undersized quantum is the
            // primary cause of perceived "broken samples" on headphones.
            "node.latency" => latency_hint,
        },
    )
    .map_err(|e| EngineError::Setup(e.to_string()))?;

    let stream_state = Rc::new(RefCell::new(ManagedStreamState::from_pipewire(
        stream.state(),
    )));
    let listener_state = Rc::clone(&stream_state);
    let listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |_stream, _, _old, new| {
            *listener_state.borrow_mut() = ManagedStreamState::from_pipewire(new);
        })
        .process(move |stream, _| {
            write_output_buffer(stream, &queues, &stream_runtime, OutputTarget::Local);
        })
        .register()
        .map_err(|e| EngineError::Setup(e.to_string()))?;

    connect_output_stream(&stream)?;
    *stream_state.borrow_mut() = ManagedStreamState::from_pipewire(stream.state());
    Ok(StreamHandle::new(stream, listener, stream_state))
}

/// Feeder Playback stream that pushes our mix into the real virtual-mic
/// null-sink (loaded separately with `pactl load-module`).
///
/// Apps see that null-sink as `linuxsoundboard.virtual_mic`: a real driver node
/// eligible to be the system default. The feeder connects **without**
/// AUTOCONNECT, because WirePlumber's policy won't link a Stream/Output/Audio
/// to an Audio/Source/Virtual node and silently drops it from the policy graph.
/// explicit_links.rs wires it with link-factory instead — the same thing
/// `pw-link` does on the CLI.
///
/// `node.always-process=true` is mandatory and `node.passive` must stay unset:
/// the feeder has to run every quantum and produce a buffer (silence when the
/// queue is dry) or the explicit link stops being `active`. Without it the
/// feeder sleeps with its driver, the cold wake-up handshake on a hand-made
/// link is unreliable, and the first consumer after engine start gets a burst
/// of zero/stale buffer fragments — audible as a harsh whizzling noise.
pub(super) fn create_runtime_virtual_source_stream(
    core: pw::core::CoreRc,
    queues: RtSharedQueues,
    stream_runtime: Arc<StreamRuntimeShared>,
    latency_hint: &str,
) -> Result<StreamHandle, EngineError> {
    let stream = pw::stream::StreamRc::new(
        core,
        crate::app_meta::VIRTUAL_MIC_FEEDER_NODE_NAME,
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Communication",
            *pw::keys::NODE_NAME => crate::app_meta::VIRTUAL_MIC_FEEDER_NODE_NAME,
            *pw::keys::NODE_DESCRIPTION => "Linux Soundboard Mic Feeder",
            // WirePlumber's session-manager policy ignores this node because
            // AUTOCONNECT is never set on its connect() call below; the graph
            // is wired explicitly via link-factory in explicit_links.rs.
            "node.dont-reconnect" => "true",
            // Always run the process callback, even when no consumer is
            // currently reading from the null-sink. Keeps the explicit links
            // in `active` state and prevents the cold wake-up burst that
            // produces a "whizzling" sound on first capture after start.
            "node.always-process" => "true",
            // Pin to the soundboard driver group so Discord opening a second
            // consumer does not pull the feeder into a competing driver.
            "node.group" => SOUNDBOARD_NODE_GROUP,
            // Hold the negotiated quantum/rate against graph reconfiguration.
            "node.lock-quantum" => "true",
            "node.lock-rate" => "true",
            "node.latency" => latency_hint,
        },
    )
    .map_err(|e| EngineError::Setup(e.to_string()))?;

    let stream_state = Rc::new(RefCell::new(ManagedStreamState::from_pipewire(
        stream.state(),
    )));
    let listener_state = Rc::clone(&stream_state);
    let listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |_stream, _, _old, new| {
            *listener_state.borrow_mut() = ManagedStreamState::from_pipewire(new);
        })
        .process(move |stream, _| {
            write_output_buffer(stream, &queues, &stream_runtime, OutputTarget::Virtual);
        })
        .register()
        .map_err(|e| EngineError::Setup(e.to_string()))?;

    // MAP_BUFFERS + RT_PROCESS — no AUTOCONNECT. RT_PROCESS moves the process
    // callback to the data thread (SCHED_FIFO 88 via RTKit) so it cannot be
    // starved by main-loop work (mix tick, registry events, decoder).
    connect_output_stream_with_flags(
        &stream,
        pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS,
    )?;
    *stream_state.borrow_mut() = ManagedStreamState::from_pipewire(stream.state());
    Ok(StreamHandle::new(stream, listener, stream_state))
}

pub(super) fn create_capture_stream(
    core: pw::core::CoreRc,
    queues: RtSharedQueues,
    target_node_name: &str,
    latency_hint: &str,
) -> Result<StreamHandle, EngineError> {
    let stream = pw::stream::StreamRc::new(
        core,
        MIC_CAPTURE_NODE_NAME,
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Communication",
            *pw::keys::NODE_NAME => MIC_CAPTURE_NODE_NAME,
            "target.object" => target_node_name,
            "node.dont-fallback" => "true",
            "node.dont-move" => "true",
            "node.dont-reconnect" => "true",
            "state.restore-target" => "false",
            "node.latency" => latency_hint,
            // Pin to the soundboard driver group so capture stays on the same
            // clock as local + feeder; otherwise a separate driver introduces
            // a clock-domain bridge that the audioadapter would have to
            // resample over, producing micro-glitches.
            "node.group" => SOUNDBOARD_NODE_GROUP,
            "node.always-process" => "true",
            "node.lock-quantum" => "true",
            "node.lock-rate" => "true",
        },
    )
    .map_err(|e| EngineError::Setup(e.to_string()))?;

    let stream_state = Rc::new(RefCell::new(ManagedStreamState::from_pipewire(
        stream.state(),
    )));
    let listener_state = Rc::clone(&stream_state);
    let listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |_stream, _, _old, new| {
            let next = ManagedStreamState::from_pipewire(new);
            if next == ManagedStreamState::Error {
                warn!("PipeWire mic capture stream entered error state");
            }
            *listener_state.borrow_mut() = next;
        })
        .process(move |stream, _| {
            read_capture_buffer(stream, &queues);
        })
        .register()
        .map_err(|e| EngineError::Setup(e.to_string()))?;

    let format = build_audio_format_pod()?;
    let mut params = [spa::pod::Pod::from_bytes(&format)
        .ok_or_else(|| EngineError::Setup("Invalid PipeWire format pod".to_string()))?];
    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            // AUTOCONNECT for source binding; RT_PROCESS so the capture
            // callback runs on the data thread.
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| EngineError::Setup(e.to_string()))?;

    *stream_state.borrow_mut() = ManagedStreamState::from_pipewire(stream.state());
    Ok(StreamHandle::new(stream, listener, stream_state))
}

fn connect_output_stream(stream: &pw::stream::StreamRc) -> Result<(), EngineError> {
    connect_output_stream_with_flags(
        stream,
        // RT_PROCESS routes the callback to the data thread. Without it the
        // callback runs on the main loop shared with mix-tick and registry
        // listeners; a slow mix-tick stalls the local output and causes glitches.
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
    )
}

fn connect_output_stream_with_flags(
    stream: &pw::stream::StreamRc,
    flags: pw::stream::StreamFlags,
) -> Result<(), EngineError> {
    let format = build_audio_format_pod()?;
    let mut params = [spa::pod::Pod::from_bytes(&format)
        .ok_or_else(|| EngineError::Setup("Invalid PipeWire format pod".to_string()))?];
    stream
        .connect(spa::utils::Direction::Output, None, flags, &mut params)
        .map_err(|e| EngineError::Setup(e.to_string()))
}

fn build_audio_format_pod() -> Result<Vec<u8>, EngineError> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(TARGET_OUTPUT_SAMPLE_RATE);
    info.set_channels(TARGET_OUTPUT_CHANNELS);
    let value = pw::spa::pod::Value::Object(pw::spa::pod::Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    });
    pw::spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &value)
        .map(|result| result.0.into_inner())
        .map_err(|e| {
            EngineError::Setup(format!("Failed to build PipeWire audio format pod: {e:?}"))
        })
}

/// PipeWire `node.group` value shared by all three of our streams. Keeps the
/// soundboard's local output, virtual-mic feeder, and capture stream pinned
/// to the same driver so cross-clock-domain resampling never creeps in when
/// other clients (Discord, browsers) join the graph.
const SOUNDBOARD_NODE_GROUP: &str = "linuxsoundboard.engine";

#[derive(Clone, Copy)]
enum OutputTarget {
    Local,
    Virtual,
}

fn write_output_buffer(
    stream: &pw::stream::Stream,
    queues: &RtSharedQueues,
    stream_runtime: &Arc<StreamRuntimeShared>,
    target: OutputTarget,
) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return;
    }

    let data = &mut datas[0];
    let target_name = match target {
        OutputTarget::Local => "local",
        OutputTarget::Virtual => "virtual",
    };
    let bytes_len = {
        let chunk_size = data.chunk().size() as usize;
        let Some(bytes) = data.data() else {
            return;
        };

        let mapped_bytes = bytes.len();
        let requested_bytes = chunk_size.min(mapped_bytes);
        let fallback_samples = match target {
            OutputTarget::Local => MAX_LOCAL_OUTPUT_CALLBACK_SAMPLES,
            OutputTarget::Virtual => stream_runtime.max_virtual_callback_samples(),
        };
        let fallback_bytes = (fallback_samples * mem::size_of::<f32>()).min(mapped_bytes);
        let effective_bytes = if requested_bytes == 0 {
            fallback_bytes
        } else {
            requested_bytes
        };
        let requested_samples = effective_bytes / mem::size_of::<f32>();
        let max_callback_samples = match target {
            OutputTarget::Local => MAX_LOCAL_OUTPUT_CALLBACK_SAMPLES,
            OutputTarget::Virtual => stream_runtime.max_virtual_callback_samples(),
        };
        let callback_samples = requested_samples.min(max_callback_samples);
        if callback_samples < requested_samples {
            trace!(
                "PipeWire callback request capped: target={} requested_samples={} callback_samples={}",
                target_name,
                requested_samples,
                callback_samples
            );
        }

        if callback_samples == 0 {
            0
        } else {
            let effective_bytes = callback_samples * mem::size_of::<f32>();
            OUTPUT_CALLBACK_SCRATCH.with(|scratch| {
                let mut scratch = scratch.borrow_mut();
                if scratch.len() < callback_samples {
                    scratch.resize(callback_samples, 0.0);
                } else {
                    scratch[..callback_samples].fill(0.0);
                }

                let dequeued = if let Some(mut queues) = queues.try_lock() {
                    match target {
                        OutputTarget::Local => queues.local.pop_into(&mut scratch[..callback_samples]),
                        OutputTarget::Virtual => {
                            queues.virtual_out.pop_into(&mut scratch[..callback_samples])
                        }
                    }
                } else {
                    // The mix tick is holding the lock. Emit silence for this
                    // quantum and count it: one quantum of zeros downstream
                    // beats the main loop stalling the RT thread for 10-50 ms.
                    match target {
                        OutputTarget::Local => stream_runtime.record_local_underrun(),
                        OutputTarget::Virtual => stream_runtime.record_virtual_underrun(),
                    }
                    0
                };

                if dequeued > 0 && dequeued < callback_samples {
                    match target {
                        OutputTarget::Local => stream_runtime.record_local_underrun(),
                        OutputTarget::Virtual => stream_runtime.record_virtual_underrun(),
                    }
                    trace!(
                        "PipeWire output underrun: target={} dequeued_samples={} requested_samples={} mapped_samples={}",
                        target_name,
                        dequeued,
                        callback_samples,
                        mapped_bytes / mem::size_of::<f32>()
                    );
                }

                for (chunk, sample) in bytes[..effective_bytes]
                    .chunks_exact_mut(4)
                    .zip(scratch[..callback_samples].iter().copied())
                {
                    chunk.copy_from_slice(&sample.to_le_bytes());
                }
            });

            effective_bytes
        }
    };

    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = mem::size_of::<f32>() as i32;
    *chunk.size_mut() = bytes_len as u32;
}

fn read_capture_buffer(stream: &pw::stream::Stream, queues: &RtSharedQueues) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return;
    }

    let data = &mut datas[0];
    let chunk_size = data.chunk().size() as usize;
    let Some(bytes) = data.data() else {
        return;
    };
    let valid = &bytes[..chunk_size.min(bytes.len())];
    let sample_count = valid.len() / mem::size_of::<f32>();
    if sample_count == 0 {
        return;
    }

    CAPTURE_CALLBACK_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        if scratch.len() < sample_count {
            scratch.resize(sample_count, 0.0);
        }

        for (slot, chunk) in scratch[..sample_count]
            .iter_mut()
            .zip(valid.chunks_exact(4))
        {
            *slot = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]).clamp(-1.0, 1.0);
        }

        if let Some(mut queues) = queues.try_lock() {
            queues.mic_in.push_slice(&scratch[..sample_count]);
        }
        // else: drop frame — RT thread must never block; resyncs on next capture callback
    });
}
