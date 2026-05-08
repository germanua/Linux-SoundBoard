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
    queues: Arc<Mutex<ProcessQueues>>,
    stream_runtime: Arc<StreamRuntimeShared>,
}

impl PulseAudioBackend {
    pub(super) fn new(
        queues: Arc<Mutex<ProcessQueues>>,
        stream_runtime: Arc<StreamRuntimeShared>,
        runtime: &RuntimeConfig,
    ) -> Result<Self, String> {
        let spec = pulse_spec()?;
        let mut proplist =
            Proplist::new().ok_or_else(|| "Failed to allocate PulseAudio proplist".to_string())?;
        proplist
            .set_str(
                pulse::proplist::properties::APPLICATION_NAME,
                "Linux Soundboard",
            )
            .map_err(|e| format!("Failed to set PulseAudio application name: {e:?}"))?;

        let mainloop =
            Rc::new(RefCell::new(Mainloop::new().ok_or_else(|| {
                "Failed to create PulseAudio mainloop".to_string()
            })?));
        let context = Rc::new(RefCell::new(
            Context::new_with_proplist(mainloop.borrow().deref(), "Linux Soundboard", &proplist)
                .ok_or_else(|| "Failed to create PulseAudio context".to_string())?,
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

        let virtual_stream = if runtime.persistent_virtual_mic {
            create_playback_stream(
                &mainloop,
                &context,
                &spec,
                VIRTUAL_FEEDER_NODE_NAME,
                Some(VIRTUAL_SOURCE_NAME),
                runtime.virtual_output_target_samples(),
                queues.clone(),
                stream_runtime.clone(),
                OutputTarget::Virtual,
            )
            .map_err(|err| format!("PulseAudio virtual mic unavailable: {err}"))
            .ok()
        } else {
            None
        };

        let mut backend = Self {
            mainloop,
            context,
            local_stream,
            virtual_stream,
            capture_stream: None,
            queues,
            stream_runtime,
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
    ) -> Result<(), String> {
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
            self.stream_runtime.clone(),
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

fn pulse_spec() -> Result<Spec, String> {
    let spec = Spec {
        format: Format::F32le,
        channels: TARGET_OUTPUT_CHANNELS as u8,
        rate: TARGET_OUTPUT_SAMPLE_RATE,
    };
    spec.is_valid()
        .then_some(spec)
        .ok_or_else(|| "Invalid PulseAudio sample spec".to_string())
}

fn connect_context(
    mainloop: &Rc<RefCell<Mainloop>>,
    context: &Rc<RefCell<Context>>,
) -> Result<(), String> {
    {
        let ml_ref = Rc::clone(mainloop);
        context
            .borrow_mut()
            .set_state_callback(Some(Box::new(move || unsafe {
                (*ml_ref.as_ptr()).signal(false);
            })));
    }

    context
        .borrow_mut()
        .connect(None, ContextFlagSet::NOFLAGS, None)
        .map_err(|e| format!("Failed to connect PulseAudio context: {e:?}"))?;

    mainloop.borrow_mut().lock();
    if let Err(err) = mainloop.borrow_mut().start() {
        mainloop.borrow_mut().unlock();
        return Err(format!("Failed to start PulseAudio mainloop: {err:?}"));
    }

    loop {
        match context.borrow().get_state() {
            ContextState::Ready => break,
            ContextState::Failed | ContextState::Terminated => {
                mainloop.borrow_mut().unlock();
                return Err("PulseAudio context failed or terminated".to_string());
            }
            _ => mainloop.borrow_mut().wait(),
        }
    }

    context.borrow_mut().set_state_callback(None);
    mainloop.borrow_mut().unlock();
    Ok(())
}

fn create_playback_stream(
    mainloop: &Rc<RefCell<Mainloop>>,
    context: &Rc<RefCell<Context>>,
    spec: &Spec,
    name: &str,
    target_sink: Option<&str>,
    target_samples: usize,
    queues: Arc<Mutex<ProcessQueues>>,
    stream_runtime: Arc<StreamRuntimeShared>,
    target: OutputTarget,
) -> Result<PulseStream, String> {
    mainloop.borrow_mut().lock();
    let stream = Rc::new(RefCell::new(
        Stream::new(&mut context.borrow_mut(), name, spec, None)
            .ok_or_else(|| format!("Failed to create PulseAudio stream {name}"))?,
    ));

    {
        let stream_ref = Rc::clone(&stream);
        stream
            .borrow_mut()
            .set_write_callback(Some(Box::new(move |requested_bytes| {
                write_playback_bytes(
                    &stream_ref,
                    &queues,
                    &stream_runtime,
                    target,
                    requested_bytes,
                );
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
        return Err(format!(
            "Failed to connect PulseAudio playback stream {name}: {err:?}"
        ));
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
    queues: Arc<Mutex<ProcessQueues>>,
    stream_runtime: Arc<StreamRuntimeShared>,
) -> Result<PulseStream, String> {
    mainloop.borrow_mut().lock();
    let stream = Rc::new(RefCell::new(
        Stream::new(
            &mut context.borrow_mut(),
            "linuxsoundboard.mic_capture",
            spec,
            None,
        )
        .ok_or_else(|| "Failed to create PulseAudio capture stream".to_string())?,
    ));

    {
        let stream_ref = Rc::clone(&stream);
        stream
            .borrow_mut()
            .set_read_callback(Some(Box::new(move |_| {
                read_capture_bytes(&stream_ref, &queues, &stream_runtime);
            })));
    }

    let attr = capture_buffer_attr(target_samples);
    let flags = StreamFlagSet::ADJUST_LATENCY | StreamFlagSet::AUTO_TIMING_UPDATE;
    if let Err(err) = stream
        .borrow_mut()
        .connect_record(target_source, Some(&attr), flags)
    {
        mainloop.borrow_mut().unlock();
        return Err(format!(
            "Failed to connect PulseAudio capture stream: {err:?}"
        ));
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
) -> Result<(), String> {
    {
        let ml_ref = Rc::clone(mainloop);
        stream
            .borrow_mut()
            .set_state_callback(Some(Box::new(move || unsafe {
                (*ml_ref.as_ptr()).signal(false);
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
                return Err("PulseAudio stream failed or terminated".to_string());
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
        prebuf: 0,
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
    queues: &Arc<Mutex<ProcessQueues>>,
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

    let mut samples = vec![0.0; sample_count];
    if let Ok(mut queues) = queues.try_lock() {
        match target {
            OutputTarget::Local => {
                let _ = queues.local.pop_into(&mut samples);
            }
            OutputTarget::Virtual => {
                let _ = queues.virtual_out.pop_into(&mut samples);
            }
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

fn read_capture_bytes(
    stream: &PulseStream,
    queues: &Arc<Mutex<ProcessQueues>>,
    stream_runtime: &Arc<StreamRuntimeShared>,
) {
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

    let batch_samples = stream_runtime.capture_batch_samples();
    if let Ok(mut queues) = queues.try_lock() {
        for chunk in samples.chunks(batch_samples) {
            queues.mic_in.push_slice(chunk);
        }
    }
}
