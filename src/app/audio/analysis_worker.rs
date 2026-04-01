//! Background loudness worker.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use crate::audio::loudness;

/// Commands sent to the worker thread.
#[derive(Debug)]
pub enum AnalysisCommand {
    AnalyzeFile { id: String, path: String },
    AnalyzeAll,
    Cancel,
}

/// Events emitted by the worker thread.
#[derive(Debug, Clone)]
pub enum AnalysisEvent {
    Progress {
        completed: u32,
        total: u32,
        current_file: String,
    },
    Complete {
        analyzed_count: u32,
    },
    Error {
        message: String,
    },
}

pub struct AnalysisWorker {
    command_tx: Sender<AnalysisCommand>,
    event_rx: Receiver<AnalysisEvent>,
}

impl AnalysisWorker {
    /// Start the worker thread.
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

    /// Queue a single-file analysis.
    pub fn analyze_file(&self, id: String, path: String) {
        let _ = self
            .command_tx
            .send(AnalysisCommand::AnalyzeFile { id, path });
    }

    /// Queue a full-library analysis.
    pub fn analyze_all(&self) {
        let _ = self.command_tx.send(AnalysisCommand::AnalyzeAll);
    }

    /// Cancel the current analysis pass.
    pub fn cancel(&self) {
        let _ = self.command_tx.send(AnalysisCommand::Cancel);
    }

    /// Poll for the next worker event.
    pub fn try_recv_event(&self) -> Result<AnalysisEvent, std::sync::mpsc::TryRecvError> {
        self.event_rx.try_recv()
    }
}

impl Default for AnalysisWorker {
    fn default() -> Self {
        Self::new()
    }
}

fn analysis_thread_main(command_rx: Receiver<AnalysisCommand>, event_tx: Sender<AnalysisEvent>) {
    while let Ok(cmd) = command_rx.recv() {
        match cmd {
            AnalysisCommand::AnalyzeFile { id: _, path } => {
                loudness::reset_loudness_analysis_cancelled();
                match loudness::analyze_loudness_path(std::path::Path::new(&path)) {
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
            AnalysisCommand::Cancel => {
                loudness::cancel_loudness_analysis();
            }
        }
    }
}
