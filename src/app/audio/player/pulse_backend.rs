use super::*;
use pulse::context::{Context, FlagSet as ContextFlagSet, State as ContextState};
use pulse::def::BufferAttr;
use pulse::mainloop::threaded::Mainloop;
use pulse::proplist::Proplist;
use pulse::sample::{Format, Spec};
use pulse::stream::{FlagSet as StreamFlagSet, PeekResult, SeekMode, State as StreamState, Stream};
use std::cell::RefCell;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;

type PulseStream = Rc<RefCell<Stream>>;

pub(super) struct PulseAudioBackend {
    mainloop: Rc<RefCell<Mainloop>>,
    context: Rc<RefCell<Context>>,
    local_stream: Option<PulseStream>,
    virtual_stream: Option<PulseStream>,
    capture_stream: Option<PulseStream>,
    queues: RtSharedQueues,
}

impl PulseAudioBackend {
    pub(super) fn new(
        queues: RtSharedQueues,
        stream_runtime: Arc<StreamRuntimeShared>,
        runtime: &RuntimeConfig,
    ) -> Result<Self, EngineError> {
        let spec = pulse_spec()?;
        let mut proplist = Proplist::new().ok_or_else(|| {
            EngineError::Setup("Failed to allocate PulseAudio proplist".to_string())
        })?;
        proplist
            .set_str(
                pulse::proplist::properties::APPLICATION_NAME,
                "Linux Soundboard",
            )
            .map_err(|e| {
                EngineError::Setup(format!("Failed to set PulseAudio application name: {e:?}"))
            })?;

        let mainloop = Rc::new(RefCell::new(Mainloop::new().ok_or_else(|| {
            EngineError::Setup("Failed to create PulseAudio mainloop".to_string())
        })?));
        let context = Rc::new(RefCell::new(
            Context::new_with_proplist(mainloop.borrow().deref(), "Linux Soundboard", &proplist)
                .ok_or_else(|| {
                    EngineError::Setup("Failed to create PulseAudio context".to_string())
                })?,
        ));

        connect_context(&mainloop, &context)?;

        let local_stream = create_playback_stream(
            &mainloop,
            &context,
            &spec,
            "linuxsoundboard.local_playback",
            None,
            runtime.local_output_target_samples(),
            queues.clone(),
            stream_runtime.clone(),
            OutputTarget::Local,
        )
        .map_err(|err| format!("PulseAudio local output unavailable: {err}"))
        .ok();

        // Feed the null sink backing linuxsoundboard.virtual_mic.
        let virtual_stream = create_playback_stream(
            &mainloop,
            &context,
            &spec,
            "linuxsoundboard.virtual_mic_feeder",
            Some(VIRTUAL_SOURCE_NAME),
            runtime.virtual_output_target_samples(),
            queues.clone(),
            stream_runtime.clone(),
            OutputTarget::Virtual,
        )
        .map_err(|err| {
            warn!(
                "PulseAudio virtual mic feeder unavailable: {err}. \
                 The virtual mic will be silent under PulseAudio."
            );
            err
        })
        .ok();

        let mut backend = Self {
            mainloop,
            context,
            local_stream,
            virtual_stream,
            capture_stream: None,
            queues,
        };
        backend.recreate_capture_stream(runtime)?;
        Ok(backend)
    }

    pub(super) fn virtual_stream_active(&self) -> bool {
        self.stream_ready(self.virtual_stream.as_ref())
    }

    pub(super) fn local_stream_active(&self) -> bool {
        self.stream_ready(self.local_stream.as_ref())
    }

    pub(super) fn capture_stream_active(&self) -> bool {
        self.stream_ready(self.capture_stream.as_ref())
    }

    pub(super) fn recreate_capture_stream(
        &mut self,
        runtime: &RuntimeConfig,
    ) -> Result<(), EngineError> {
        self.drop_capture_stream();
        if !runtime.mic_passthrough {
            return Ok(());
        }

        let target = runtime
            .mic_source
            .as_deref()
            .filter(|source| *source != VIRTUAL_SOURCE_NAME);
        let spec = pulse_spec()?;
        let stream = create_capture_stream(
            &self.mainloop,
            &self.context,
            &spec,
            target,
            runtime.virtual_output_target_samples(),
            self.queues.clone(),
        )?;
        self.capture_stream = Some(stream);
        Ok(())
    }

    fn drop_capture_stream(&mut self) {
        if let Some(stream) = self.capture_stream.take() {
            self.mainloop.borrow_mut().lock();
            let _ = stream.borrow_mut().disconnect();
            self.mainloop.borrow_mut().unlock();
        }
    }

    pub(super) fn stop_streams_for_shutdown(&mut self) {
        self.mainloop.borrow_mut().lock();
        for stream in [
            self.capture_stream.take(),
            self.virtual_stream.take(),
            self.local_stream.take(),
        ]
        .into_iter()
        .flatten()
        {
            let _ = stream.borrow_mut().disconnect();
        }
        self.mainloop.borrow_mut().unlock();
    }

    fn stream_ready(&self, stream: Option<&PulseStream>) -> bool {
        let Some(stream) = stream else {
            return false;
        };
        self.mainloop.borrow_mut().lock();
        let ready = stream.borrow().get_state() == StreamState::Ready;
        self.mainloop.borrow_mut().unlock();
        ready
    }
}

impl Drop for PulseAudioBackend {
    fn drop(&mut self) {
        self.mainloop.borrow_mut().lock();
        if let Some(stream) = self.capture_stream.take() {
            let _ = stream.borrow_mut().disconnect();
        }
        if let Some(stream) = self.virtual_stream.take() {
            let _ = stream.borrow_mut().disconnect();
        }
        if let Some(stream) = self.local_stream.take() {
            let _ = stream.borrow_mut().disconnect();
        }
        self.context.borrow_mut().disconnect();
        self.mainloop.borrow_mut().unlock();
        self.mainloop.borrow_mut().stop();
    }
}

fn pulse_spec() -> Result<Spec, EngineError> {
    let spec = Spec {
        format: Format::F32le,
        channels: TARGET_OUTPUT_CHANNELS as u8,
        rate: TARGET_OUTPUT_SAMPLE_RATE,
    };
    spec.is_valid()
        .then_some(spec)
        .ok_or_else(|| EngineError::Setup("Invalid PulseAudio sample spec".to_string()))
}

fn connect_context(
    mainloop: &Rc<RefCell<Mainloop>>,
    context: &Rc<RefCell<Context>>,
) -> Result<(), EngineError> {
    {
        let ml_ref = Rc::clone(mainloop);
        context
            .borrow_mut()
            .set_state_callback(Some(Box::new(move || {
                // SAFETY: ml_ref keeps the mainloop alive for this callback.
                unsafe { (*ml_ref.as_ptr()).signal(false) };
            })));
    }

    context
        .borrow_mut()
        .connect(None, ContextFlagSet::NOFLAGS, None)
        .map_err(|e| EngineError::Setup(format!("Failed to connect PulseAudio context: {e:?}")))?;

    mainloop.borrow_mut().lock();
    if let Err(err) = mainloop.borrow_mut().start() {
        mainloop.borrow_mut().unlock();
        return Err(EngineError::Setup(format!(
            "Failed to start PulseAudio mainloop: {err:?}"
        )));
    }

    loop {
        match context.borrow().get_state() {
            ContextState::Ready => break,
            ContextState::Failed | ContextState::Terminated => {
                mainloop.borrow_mut().unlock();
                return Err(EngineError::Setup(
                    "PulseAudio context failed or terminated".to_string(),
                ));
            }
            _ => mainloop.borrow_mut().wait(),
        }
    }

    context.borrow_mut().set_state_callback(None);
    mainloop.borrow_mut().unlock();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn create_playback_stream(
    mainloop: &Rc<RefCell<Mainloop>>,
    context: &Rc<RefCell<Context>>,
    spec: &Spec,
    name: &str,
    target_sink: Option<&str>,
    target_samples: usize,
    queues: RtSharedQueues,
    stream_runtime: Arc<StreamRuntimeShared>,
    target: OutputTarget,
) -> Result<PulseStream, EngineError> {
    mainloop.borrow_mut().lock();
    let stream = Rc::new(RefCell::new(
        Stream::new(&mut context.borrow_mut(), name, spec, None).ok_or_else(|| {
            EngineError::Setup(format!("Failed to create PulseAudio stream {name}"))
        })?,
    ));

    {
        let stream_ref = Rc::clone(&stream);
        let stream_runtime_for_write = stream_runtime.clone();
        stream
            .borrow_mut()
            .set_write_callback(Some(Box::new(move |requested_bytes| {
                write_playback_bytes(
                    &stream_ref,
                    &queues,
                    &stream_runtime_for_write,
                    target,
                    requested_bytes,
                );
            })));
    }

    {
        let stream_runtime_for_underflow = stream_runtime.clone();
        let name_owned = name.to_string();
        stream
            .borrow_mut()
            .set_underflow_callback(Some(Box::new(move || {
                match target {
                    OutputTarget::Local => stream_runtime_for_underflow.record_local_underrun(),
                    OutputTarget::Virtual => stream_runtime_for_underflow.record_virtual_underrun(),
                }
                log::debug!("PulseAudio underflow on {}", name_owned);
            })));
    }

    let attr = playback_buffer_attr(target_samples);
    let flags = StreamFlagSet::ADJUST_LATENCY | StreamFlagSet::AUTO_TIMING_UPDATE;
    if let Err(err) =
        stream
            .borrow_mut()
            .connect_playback(target_sink, Some(&attr), flags, None, None)
    {
        mainloop.borrow_mut().unlock();
        return Err(EngineError::Setup(format!(
            "Failed to connect PulseAudio playback stream {name}: {err:?}"
        )));
    }

    if let Err(err) = wait_for_stream_ready(mainloop, &stream) {
        mainloop.borrow_mut().unlock();
        return Err(err);
    }
    mainloop.borrow_mut().unlock();
    Ok(stream)
}

fn create_capture_stream(
    mainloop: &Rc<RefCell<Mainloop>>,
    context: &Rc<RefCell<Context>>,
    spec: &Spec,
    target_source: Option<&str>,
    target_samples: usize,
    queues: RtSharedQueues,
) -> Result<PulseStream, EngineError> {
    mainloop.borrow_mut().lock();
    let stream = Rc::new(RefCell::new(
        Stream::new(
            &mut context.borrow_mut(),
            "linuxsoundboard.mic_capture",
            spec,
            None,
        )
        .ok_or_else(|| {
            EngineError::Setup("Failed to create PulseAudio capture stream".to_string())
        })?,
    ));

    {
        let stream_ref = Rc::clone(&stream);
        stream
            .borrow_mut()
            .set_read_callback(Some(Box::new(move |_| {
                read_capture_bytes(&stream_ref, &queues);
            })));
    }

    let attr = capture_buffer_attr(target_samples);
    let flags = StreamFlagSet::ADJUST_LATENCY | StreamFlagSet::AUTO_TIMING_UPDATE;
    if let Err(err) = stream
        .borrow_mut()
        .connect_record(target_source, Some(&attr), flags)
    {
        mainloop.borrow_mut().unlock();
        return Err(EngineError::Setup(format!(
            "Failed to connect PulseAudio capture stream: {err:?}"
        )));
    }

    if let Err(err) = wait_for_stream_ready(mainloop, &stream) {
        mainloop.borrow_mut().unlock();
        return Err(err);
    }
    mainloop.borrow_mut().unlock();
    Ok(stream)
}

fn wait_for_stream_ready(
    mainloop: &Rc<RefCell<Mainloop>>,
    stream: &PulseStream,
) -> Result<(), EngineError> {
    {
        let ml_ref = Rc::clone(mainloop);
        stream
            .borrow_mut()
            .set_state_callback(Some(Box::new(move || {
                // SAFETY: as in connect_context, with the mainloop lock held.
                unsafe { (*ml_ref.as_ptr()).signal(false) };
            })));
    }

    loop {
        match stream.borrow().get_state() {
            StreamState::Ready => {
                stream.borrow_mut().set_state_callback(None);
                return Ok(());
            }
            StreamState::Failed | StreamState::Terminated => {
                stream.borrow_mut().set_state_callback(None);
                return Err(EngineError::Setup(
                    "PulseAudio stream failed or terminated".to_string(),
                ));
            }
            _ => mainloop.borrow_mut().wait(),
        }
    }
}

fn playback_buffer_attr(target_samples: usize) -> BufferAttr {
    let target_bytes = samples_to_bytes(target_samples);
    BufferAttr {
        maxlength: u32::MAX,
        tlength: target_bytes,
        prebuf: samples_to_bytes(MIX_CHUNK_FRAMES * TARGET_OUTPUT_CHANNELS as usize),
        minreq: samples_to_bytes(MIX_CHUNK_FRAMES * TARGET_OUTPUT_CHANNELS as usize),
        fragsize: u32::MAX,
    }
}

fn capture_buffer_attr(target_samples: usize) -> BufferAttr {
    BufferAttr {
        maxlength: u32::MAX,
        tlength: u32::MAX,
        prebuf: u32::MAX,
        minreq: u32::MAX,
        fragsize: samples_to_bytes(
            target_samples.min(MIX_CHUNK_FRAMES * TARGET_OUTPUT_CHANNELS as usize),
        ),
    }
}

fn samples_to_bytes(samples: usize) -> u32 {
    samples
        .saturating_mul(std::mem::size_of::<f32>())
        .min(u32::MAX as usize) as u32
}

#[derive(Clone, Copy)]
enum OutputTarget {
    Local,
    Virtual,
}

fn write_playback_bytes(
    stream: &PulseStream,
    queues: &RtSharedQueues,
    stream_runtime: &Arc<StreamRuntimeShared>,
    target: OutputTarget,
    requested_bytes: usize,
) {
    let max_samples = match target {
        OutputTarget::Local => MAX_LOCAL_OUTPUT_CALLBACK_SAMPLES,
        OutputTarget::Virtual => stream_runtime.max_virtual_callback_samples(),
    };
    let mut sample_count = (requested_bytes / std::mem::size_of::<f32>()).min(max_samples);
    sample_count -= sample_count % TARGET_OUTPUT_CHANNELS as usize;
    if sample_count == 0 {
        return;
    }

    let mut samples = vec![0.0f32; sample_count];
    let dequeued = if let Some(mut queues) = queues.try_lock() {
        match target {
            OutputTarget::Local => queues.local.pop_into(&mut samples),
            OutputTarget::Virtual => queues.virtual_out.pop_into(&mut samples),
        }
    } else {
        // Output silence instead of blocking the PulseAudio thread.
        match target {
            OutputTarget::Local => stream_runtime.record_local_underrun(),
            OutputTarget::Virtual => stream_runtime.record_virtual_underrun(),
        }
        0
    };

    if dequeued > 0 && dequeued < sample_count {
        match target {
            OutputTarget::Local => stream_runtime.record_local_underrun(),
            OutputTarget::Virtual => stream_runtime.record_virtual_underrun(),
        }
    }

    let mut bytes = Vec::with_capacity(sample_count * std::mem::size_of::<f32>());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }

    if let Err(err) = stream
        .borrow_mut()
        .write_copy(&bytes, 0, SeekMode::Relative)
    {
        warn!("PulseAudio playback stream write failed: {err:?}");
    }
}

fn read_capture_bytes(stream: &PulseStream, queues: &RtSharedQueues) {
    let mut samples = Vec::new();
    {
        let mut stream = stream.borrow_mut();
        match stream.peek() {
            Ok(PeekResult::Data(bytes)) => {
                samples.reserve(bytes.len() / std::mem::size_of::<f32>());
                for chunk in bytes.chunks_exact(4) {
                    samples.push(
                        f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                            .clamp(-1.0, 1.0),
                    );
                }
                let _ = stream.discard();
            }
            Ok(PeekResult::Hole(_)) => {
                let _ = stream.discard();
            }
            Ok(PeekResult::Empty) => {}
            Err(err) => {
                warn!("PulseAudio capture stream read failed: {err:?}");
            }
        }
    }

    if samples.is_empty() {
        return;
    }

    if let Some(mut queues) = queues.try_lock() {
        queues.mic_in.push_slice(&samples);
    }
    // else: drop frame, mic_in resyncs on next read callback.
}
