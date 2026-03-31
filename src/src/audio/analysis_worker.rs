//! Background worker for loudness analysis with progress reporting.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use crate::audio::loudness;

/// Commands sent to the analysis worker
#[derive(Debug)]
pub enum AnalysisCommand {
    /// Analyze a single file
    AnalyzeFile { id: String, path: String },
    /// Analyze all files in config
    AnalyzeAll,
    /// Cancel ongoing analysis
    Cancel,
}

/// Events emitted by the analysis worker
#[derive(Debug, Clone)]
pub enum AnalysisEvent {
    /// Progress update: (completed_count, total_count, current_file_path)
    Progress {
        completed: u32,
        total: u32,
        current_file: String,
    },
    /// Analysis completed: (number of files analyzed)
    Complete { analyzed_count: u32 },
    /// Analysis failed with error message
    Error { message: String },
}

/// Analysis worker that runs in a background thread
pub struct AnalysisWorker {
    command_tx: Sender<AnalysisCommand>,
    event_rx: Receiver<AnalysisEvent>,
}

impl AnalysisWorker {
    /// Create a new analysis worker
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

    /// Request analysis of a single file
    pub fn analyze_file(&self, id: String, path: String) {
        let _ = self
            .command_tx
            .send(AnalysisCommand::AnalyzeFile { id, path });
    }

    /// Request analysis of all files in config
    pub fn analyze_all(&self) {
        let _ = self.command_tx.send(AnalysisCommand::AnalyzeAll);
    }

    /// Cancel ongoing analysis
    pub fn cancel(&self) {
        let _ = self.command_tx.send(AnalysisCommand::Cancel);
    }

    /// Try to receive an event without blocking
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
    // Worker runs until channel is closed
    while let Ok(cmd) = command_rx.recv() {
        match cmd {
            AnalysisCommand::AnalyzeFile { id: _, path } => {
                loudness::reset_loudness_analysis_cancelled();
                match loudness::analyze_loudness_path(std::path::Path::new(&path)) {
                    Ok(lufs) if lufs.is_finite() => {
                        // Emit completion event with result
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
                // This would be called from commands with proper config access
                let _ = event_tx.send(AnalysisEvent::Complete { analyzed_count: 0 });
            }
            AnalysisCommand::Cancel => {
                loudness::cancel_loudness_analysis();
            }
        }
    }
}
