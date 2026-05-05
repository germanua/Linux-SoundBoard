use super::*;

pub(super) fn create_local_output_stream(
    core: pw::core::CoreRc,
    queues: std::sync::Arc<std::sync::Mutex<ProcessQueues>>,
    stream_runtime: std::sync::Arc<StreamRuntimeShared>,
) -> Result<StreamHandle, String> {
    let stream = pw::stream::StreamRc::new(
        core,
        LOCAL_PLAYBACK_NODE_NAME,
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Music",
            *pw::keys::NODE_NAME => LOCAL_PLAYBACK_NODE_NAME,
            *pw::keys::NODE_DESCRIPTION => VIRTUAL_OUTPUT_DESCRIPTION,
        },
    )
    .map_err(|e| e.to_string())?;

    let listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            write_output_buffer(stream, &queues, &stream_runtime, OutputTarget::Local);
        })
        .register()
        .map_err(|e| e.to_string())?;

    connect_output_stream(&stream)?;
    Ok(StreamHandle {
        _stream: stream,
        _listener: listener,
    })
}

pub(super) fn create_virtual_source_stream(
    core: pw::core::CoreRc,
    queues: std::sync::Arc<std::sync::Mutex<ProcessQueues>>,
    stream_runtime: std::sync::Arc<StreamRuntimeShared>,
    latency_hint: &str,
) -> Result<StreamHandle, String> {
    let stream = pw::stream::StreamRc::new(
        core,
        VIRTUAL_SOURCE_NAME,
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Communication",
            *pw::keys::MEDIA_CLASS => "Audio/Source",
            *pw::keys::NODE_NAME => VIRTUAL_SOURCE_NAME,
            *pw::keys::NODE_DESCRIPTION => VIRTUAL_MIC_DESCRIPTION,
            "node.virtual" => "true",
            "priority.session" => "10",
            "node.latency" => latency_hint,
        },
    )
    .map_err(|e| e.to_string())?;

    let listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            write_output_buffer(stream, &queues, &stream_runtime, OutputTarget::Virtual);
        })
        .register()
        .map_err(|e| e.to_string())?;

    connect_output_stream(&stream)?;
    Ok(StreamHandle {
        _stream: stream,
        _listener: listener,
    })
}

pub(super) fn create_capture_stream(
    core: pw::core::CoreRc,
    queues: std::sync::Arc<std::sync::Mutex<ProcessQueues>>,
    stream_runtime: std::sync::Arc<StreamRuntimeShared>,
    target_node_name: &str,
    latency_hint: &str,
) -> Result<StreamHandle, String> {
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
            "node.latency" => latency_hint,
        },
    )
    .map_err(|e| e.to_string())?;

    let listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            read_capture_buffer(stream, &queues, &stream_runtime);
        })
        .register()
        .map_err(|e| e.to_string())?;

    let format = build_audio_format_pod()?;
    let mut params = [spa::pod::Pod::from_bytes(&format).ok_or("Invalid PipeWire format pod")?];
    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|e| e.to_string())?;

    Ok(StreamHandle {
        _stream: stream,
        _listener: listener,
    })
}

fn connect_output_stream(stream: &pw::stream::StreamRc) -> Result<(), String> {
    let format = build_audio_format_pod()?;
    let mut params = [spa::pod::Pod::from_bytes(&format).ok_or("Invalid PipeWire format pod")?];
    stream
        .connect(
            spa::utils::Direction::Output,
            None,
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|e| e.to_string())
}

fn build_audio_format_pod() -> Result<Vec<u8>, String> {
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
        .map_err(|e| format!("Failed to build PipeWire audio format pod: {e:?}"))
}

#[derive(Clone, Copy)]
enum OutputTarget {
    Local,
    Virtual,
}

fn write_output_buffer(
    stream: &pw::stream::Stream,
    queues: &std::sync::Arc<std::sync::Mutex<ProcessQueues>>,
    stream_runtime: &std::sync::Arc<StreamRuntimeShared>,
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

                let dequeued = if let Ok(mut queues) = queues.lock() {
                    match target {
                        OutputTarget::Local => queues.local.pop_into(&mut scratch[..callback_samples]),
                        OutputTarget::Virtual => {
                            queues.virtual_out.pop_into(&mut scratch[..callback_samples])
                        }
                    }
                } else {
                    0
                };

                if dequeued > 0 && dequeued < callback_samples {
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

fn read_capture_buffer(
    stream: &pw::stream::Stream,
    queues: &std::sync::Arc<std::sync::Mutex<ProcessQueues>>,
    stream_runtime: &std::sync::Arc<StreamRuntimeShared>,
) {
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

    let batch_samples = stream_runtime.capture_batch_samples();
    let fast_lane = stream_runtime.fast_lane_passthrough_enabled();

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

        if let Ok(mut queues) = queues.lock() {
            if fast_lane {
                queues.virtual_out.push_slice(&scratch[..sample_count]);
            } else {
                for chunk in scratch[..sample_count].chunks(batch_samples) {
                    queues.mic_in.push_slice(chunk);
                }
            }
        }
    });
}
