//! Background loudness worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

#[cfg(test)]
use std::sync::mpsc::{channel, Receiver, Sender};

#[cfg(test)]
use crate::audio::loudness;

pub type MissingLoudnessAnalysisCompletion =
    Box<dyn FnOnce(Result<u32, crate::audio::LoudnessError>) + Send + 'static>;

pub struct MissingLoudnessAnalysisCoordinator {
    in_flight: Arc<AtomicBool>,
    #[cfg(test)]
    start_count: Arc<std::sync::atomic::AtomicUsize>,
}

impl MissingLoudnessAnalysisCoordinator {
    pub fn new() -> Self {
        Self {
            in_flight: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            start_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    pub fn is_in_flight(&self) -> bool {
        self.in_flight.load(Ordering::Acquire)
    }

    pub fn try_start<F>(
        &self,
        task_name: &'static str,
        task: F,
        on_complete: Option<MissingLoudnessAnalysisCompletion>,
    ) -> Result<bool, crate::audio::LoudnessError>
    where
        F: FnOnce() -> Result<u32, crate::audio::LoudnessError> + Send + 'static,
    {
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(false);
        }

        let in_flight = Arc::clone(&self.in_flight);
        let spawn_result = thread::Builder::new()
            .name(task_name.to_string())
            .spawn(move || {
                let result = task();
                in_flight.store(false, Ordering::Release);
                if let Some(on_complete) = on_complete {
                    on_complete(result);
                }
            });

        if let Err(e) = spawn_result {
            self.in_flight.store(false, Ordering::Release);
            return Err(crate::audio::LoudnessError::Io(format!(
                "Failed to spawn loudness analysis thread: {e}"
            )));
        }

        #[cfg(test)]
        self.start_count.fetch_add(1, Ordering::AcqRel);

        Ok(true)
    }

    #[cfg(test)]
    pub fn start_count(&self) -> usize {
        self.start_count.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub fn wait_for_idle(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if !self.is_in_flight() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }
}

impl Default for MissingLoudnessAnalysisCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[derive(Debug)]
pub enum AnalysisCommand {
    AnalyzeFile { path: String },
    AnalyzeAll,
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub enum AnalysisEvent {
    Complete { analyzed_count: u32 },
    Error { message: String },
}

#[cfg(test)]
pub struct AnalysisWorker {
    command_tx: Sender<AnalysisCommand>,
    event_rx: Receiver<AnalysisEvent>,
}

#[cfg(test)]
impl AnalysisWorker {
    pub fn new() -> Self {
        let (command_tx, command_rx) = channel();
        let (event_tx, event_rx) = channel();

        thread::spawn(move || {
            analysis_thread_main(command_rx, event_tx);
        });

        Self {
            command_tx,
            event_rx,
        }
    }

    pub fn analyze_file(&self, _id: String, path: String) {
        let _ = self.command_tx.send(AnalysisCommand::AnalyzeFile { path });
    }

    pub fn analyze_all(&self) {
        let _ = self.command_tx.send(AnalysisCommand::AnalyzeAll);
    }

    pub fn try_recv_event(&self) -> Result<AnalysisEvent, std::sync::mpsc::TryRecvError> {
        self.event_rx.try_recv()
    }
}

#[cfg(test)]
impl Default for AnalysisWorker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
fn analysis_thread_main(command_rx: Receiver<AnalysisCommand>, event_tx: Sender<AnalysisEvent>) {
    while let Ok(cmd) = command_rx.recv() {
        match cmd {
            AnalysisCommand::AnalyzeFile { path } => {
                match analyze_loudness_path_for_test_worker(std::path::Path::new(&path)) {
                    Ok(lufs) if lufs.is_finite() => {
                        let _ = event_tx.send(AnalysisEvent::Complete { analyzed_count: 1 });
                    }
                    Ok(_) | Err(_) => {
                        let _ = event_tx.send(AnalysisEvent::Error {
                            message: format!("Failed to analyze: {}", path),
                        });
                    }
                }
            }
            AnalysisCommand::AnalyzeAll => {
                let _ = event_tx.send(AnalysisEvent::Complete { analyzed_count: 0 });
            }
        }
    }
}

#[cfg(test)]
fn analyze_loudness_path_for_test_worker(
    path: &std::path::Path,
) -> Result<f64, loudness::LoudnessError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        loudness::reset_loudness_analysis_cancelled();
        match loudness::analyze_loudness_path(path) {
            Err(loudness::LoudnessError::Cancelled) if std::time::Instant::now() < deadline => {
                std::thread::yield_now();
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AnalysisEvent, AnalysisWorker};
    use crate::test_support::audio_fixtures::{
        cleanup_test_audio_path, create_test_audio_file_with_duration,
    };
    use std::sync::mpsc::TryRecvError;
    use std::time::{Duration, Instant};

    fn recv_event_with_timeout(
        worker: &AnalysisWorker,
        timeout: Duration,
    ) -> Option<AnalysisEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            match worker.try_recv_event() {
                Ok(event) => return Some(event),
                Err(TryRecvError::Empty) => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(TryRecvError::Disconnected) => return None,
            }
        }
    }

    #[test]
    fn analyze_file_emits_complete_for_valid_audio_file() {
        let worker = AnalysisWorker::new();
        let audio_path = create_test_audio_file_with_duration("wav", 1_000);
        worker.analyze_file(
            "sound-1".to_string(),
            audio_path.to_string_lossy().to_string(),
        );

        let event = recv_event_with_timeout(&worker, Duration::from_secs(5))
            .expect("worker should emit an analysis event");
        match event {
            AnalysisEvent::Complete { analyzed_count } => {
                assert_eq!(analyzed_count, 1);
            }
            other => panic!("unexpected analysis event: {other:?}"),
        }

        cleanup_test_audio_path(&audio_path);
    }

    #[test]
    fn analyze_file_emits_error_for_missing_file() {
        let worker = AnalysisWorker::new();
        let missing_path = "/tmp/lsb-analysis-worker-missing-file.wav".to_string();
        worker.analyze_file("missing".to_string(), missing_path.clone());

        let event = recv_event_with_timeout(&worker, Duration::from_secs(5))
            .expect("worker should emit an analysis event");
        match event {
            AnalysisEvent::Error { message } => {
                assert!(message.contains("Failed to analyze"));
                assert!(message.contains(&missing_path));
            }
            other => panic!("unexpected analysis event: {other:?}"),
        }
    }

    #[test]
    fn analyze_all_emits_complete_with_zero_count() {
        let worker = AnalysisWorker::new();
        worker.analyze_all();

        let event = recv_event_with_timeout(&worker, Duration::from_secs(2))
            .expect("worker should emit an analysis event");
        match event {
            AnalysisEvent::Complete { analyzed_count } => {
                assert_eq!(analyzed_count, 0);
            }
            other => panic!("unexpected analysis event: {other:?}"),
        }
    }
}
