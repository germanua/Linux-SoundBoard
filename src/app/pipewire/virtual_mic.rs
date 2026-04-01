//! PipeWire virtual microphone management.

use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

use crate::app_meta::{
    LOOPBACK_LATENCY_MS, VIRTUAL_MIC_DESCRIPTION, VIRTUAL_OUTPUT_DESCRIPTION, VIRTUAL_SINK_NAME,
    VIRTUAL_SOURCE_NAME,
};

use super::command_runner::{CommandOutput, CommandRunner, SystemCommandRunner};

static SINK_MODULE_ID: Mutex<Option<String>> = Mutex::new(None);
static SOURCE_MODULE_ID: Mutex<Option<String>> = Mutex::new(None);
static LOOPBACK_MODULE_ID: Mutex<Option<String>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VirtualMicStatus {
    pub active: bool,
    pub sink_name: Option<String>,
    pub source_name: Option<String>,
    pub loopback_active: bool,
    pub error: Option<String>,
}

pub fn create_virtual_mic() -> VirtualMicStatus {
    create_virtual_mic_with_runner(&SystemCommandRunner)
}

fn create_virtual_mic_with_runner(runner: &impl CommandRunner) -> VirtualMicStatus {
    let _ = remove_virtual_mic_with_runner(runner);

    let sink_args = [
        "load-module",
        "module-null-sink",
        &format!("sink_name={}", VIRTUAL_SINK_NAME),
        &format!(
            "sink_properties=device.description=\"{}\"",
            VIRTUAL_OUTPUT_DESCRIPTION
        ),
    ];
    let sink_output = match runner.run("pactl", &sink_args) {
        Ok(output) if output.success => output,
        Ok(output) => {
            error!("Failed to create virtual sink: {}", output.stderr);
            return VirtualMicStatus {
                error: Some(format!("Failed to create virtual sink: {}", output.stderr)),
                ..Default::default()
            };
        }
        Err(e) => {
            error!("Failed to run pactl: {}", e);
            return VirtualMicStatus {
                error: Some(format!("Failed to run pactl: {}", e)),
                ..Default::default()
            };
        }
    };
    store_module_id(&SINK_MODULE_ID, sink_output.stdout.trim().to_string());

    let source_args = [
        "load-module",
        "module-virtual-source",
        &format!("source_name={}", VIRTUAL_SOURCE_NAME),
        &format!("master={}.monitor", VIRTUAL_SINK_NAME),
        &format!(
            "source_properties=device.description=\"{}\"",
            VIRTUAL_MIC_DESCRIPTION
        ),
    ];
    let source_output = match runner.run("pactl", &source_args) {
        Ok(output) if output.success => output,
        Ok(output) => {
            error!("Failed to create virtual source: {}", output.stderr);
            let _ = remove_virtual_mic_with_runner(runner);
            return VirtualMicStatus {
                error: Some(format!(
                    "Failed to create virtual source: {}",
                    output.stderr
                )),
                ..Default::default()
            };
        }
        Err(e) => {
            error!("Failed to run pactl: {}", e);
            let _ = remove_virtual_mic_with_runner(runner);
            return VirtualMicStatus {
                error: Some(format!("Failed to run pactl: {}", e)),
                ..Default::default()
            };
        }
    };
    store_module_id(&SOURCE_MODULE_ID, source_output.stdout.trim().to_string());

    info!("Virtual microphone created successfully");
    VirtualMicStatus {
        active: true,
        sink_name: Some(VIRTUAL_SINK_NAME.to_string()),
        source_name: Some(VIRTUAL_SOURCE_NAME.to_string()),
        loopback_active: false,
        error: None,
    }
}

pub fn enable_mic_passthrough_with_source(source_name: Option<String>) -> Result<(), String> {
    enable_mic_passthrough_with_source_runner(source_name, &SystemCommandRunner)
}

fn enable_mic_passthrough_with_source_runner(
    source_name: Option<String>,
    runner: &impl CommandRunner,
) -> Result<(), String> {
    let _ = disable_mic_passthrough_with_runner(runner);

    let source = match source_name {
        Some(source) => source,
        None => get_default_source(runner).ok_or("Could not find default microphone")?,
    };

    let sink_arg = format!("sink={}", VIRTUAL_SINK_NAME);
    let source_arg = format!("source={}", source);
    let latency_arg = format!("latency_msec={}", LOOPBACK_LATENCY_MS);
    let args = [
        "load-module",
        "module-loopback",
        source_arg.as_str(),
        sink_arg.as_str(),
        latency_arg.as_str(),
        "source_dont_move=true",
        "sink_dont_move=true",
        "adjust_time=0",
    ];
    let output = runner
        .run("pactl", &args)
        .map_err(|e| format!("Failed to run pactl: {}", e))?;

    if !output.success {
        return Err(format!("Failed to create loopback: {}", output.stderr));
    }

    store_module_id(&LOOPBACK_MODULE_ID, output.stdout.trim().to_string());
    Ok(())
}

pub fn enable_mic_passthrough() -> Result<(), String> {
    enable_mic_passthrough_with_source(None)
}

pub fn disable_mic_passthrough() -> Result<(), String> {
    disable_mic_passthrough_with_runner(&SystemCommandRunner)
}

fn disable_mic_passthrough_with_runner(runner: &impl CommandRunner) -> Result<(), String> {
    if let Ok(mut id) = LOOPBACK_MODULE_ID.lock() {
        if let Some(module_id) = id.take() {
            let _ = runner.run("pactl", &["unload-module", &module_id]);
            info!("Mic passthrough disabled");
        }
    }
    Ok(())
}

fn get_default_source(runner: &impl CommandRunner) -> Option<String> {
    let output = runner.run("pactl", &["get-default-source"]).ok()?;
    if output.success {
        let source = output.stdout.trim().to_string();
        if !source.is_empty() && !source.contains("LinuxSoundboard") {
            return Some(source);
        }
    }

    let output = runner.run("pactl", &["list", "short", "sources"]).ok()?;
    parse_short_names(&output)
        .into_iter()
        .find(|name| !name.ends_with(".monitor") && !name.contains("LinuxSoundboard"))
}

pub fn remove_virtual_mic() -> Result<(), String> {
    remove_virtual_mic_with_runner(&SystemCommandRunner)
}

fn remove_virtual_mic_with_runner(runner: &impl CommandRunner) -> Result<(), String> {
    let _ = disable_mic_passthrough_with_runner(runner);
    unload_modules_by_name(runner, VIRTUAL_SINK_NAME, VIRTUAL_SOURCE_NAME);

    clear_module_id(&SOURCE_MODULE_ID);
    clear_module_id(&SINK_MODULE_ID);
    Ok(())
}

fn unload_modules_by_name(runner: &impl CommandRunner, sink_name: &str, source_name: &str) {
    let output = match runner.run("pactl", &["list", "modules"]) {
        Ok(output) if output.success => output,
        Ok(_) => {
            warn!("pactl list modules failed");
            return;
        }
        Err(e) => {
            warn!("Failed to list modules: {}", e);
            return;
        }
    };

    let mut current_module_id: Option<String> = None;
    let mut should_unload = false;

    for line in output.stdout.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("Module #") {
            if should_unload {
                if let Some(id) = current_module_id.take() {
                    let _ = runner.run("pactl", &["unload-module", &id]);
                }
            }

            if let Some(id_str) = trimmed.strip_prefix("Module #") {
                current_module_id = Some(id_str.to_string());
                should_unload = false;
            }
        }

        if trimmed.contains(&format!("sink_name={}", sink_name))
            || trimmed.contains(&format!("source_name={}", source_name))
            || trimmed.contains(&format!("master={}", sink_name))
        {
            should_unload = true;
        }
    }

    if should_unload {
        if let Some(id) = current_module_id {
            let _ = runner.run("pactl", &["unload-module", &id]);
        }
    }
}

pub fn list_sinks() -> Vec<String> {
    let runner = SystemCommandRunner;
    runner
        .run("pactl", &["list", "short", "sinks"])
        .map(|output| parse_short_names(&output))
        .unwrap_or_default()
}

pub fn list_sources() -> Vec<String> {
    let runner = SystemCommandRunner;
    runner
        .run("pactl", &["list", "short", "sources"])
        .map(|output| {
            parse_short_names(&output)
                .into_iter()
                .filter(|name| !name.ends_with(".monitor") || name.contains("LinuxSoundboard"))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_short_names(output: &CommandOutput) -> Vec<String> {
    if !output.success {
        return Vec::new();
    }
    output
        .stdout
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .map(ToString::to_string)
        .collect()
}

fn store_module_id(target: &Mutex<Option<String>>, value: String) {
    if let Ok(mut id) = target.lock() {
        *id = Some(value);
    }
}

fn clear_module_id(target: &Mutex<Option<String>>) {
    if let Ok(mut id) = target.lock() {
        *id = None;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::io;
    use std::rc::Rc;

    use super::*;

    type CommandInvocation = (String, Vec<String>);

    #[derive(Clone, Default)]
    struct FakeRunner {
        commands: Rc<RefCell<Vec<CommandInvocation>>>,
        responses: Rc<HashMap<CommandInvocation, CommandOutput>>,
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
            let key = (
                program.to_string(),
                args.iter()
                    .map(|arg| (*arg).to_string())
                    .collect::<Vec<_>>(),
            );
            self.commands.borrow_mut().push(key.clone());
            self.responses
                .get(&key)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing response"))
        }
    }

    #[test]
    fn create_virtual_mic_builds_expected_pactl_commands() {
        let commands = Rc::new(RefCell::new(Vec::new()));
        let responses = HashMap::from([
            (
                (
                    "pactl".to_string(),
                    vec!["list".to_string(), "modules".to_string()],
                ),
                CommandOutput {
                    success: true,
                    stdout: String::new(),
                    stderr: String::new(),
                },
            ),
            (
                (
                    "pactl".to_string(),
                    vec![
                        "load-module".to_string(),
                        "module-null-sink".to_string(),
                        format!("sink_name={}", VIRTUAL_SINK_NAME),
                        format!(
                            "sink_properties=device.description=\"{}\"",
                            VIRTUAL_OUTPUT_DESCRIPTION
                        ),
                    ],
                ),
                CommandOutput {
                    success: true,
                    stdout: "12\n".to_string(),
                    stderr: String::new(),
                },
            ),
            (
                (
                    "pactl".to_string(),
                    vec![
                        "load-module".to_string(),
                        "module-virtual-source".to_string(),
                        format!("source_name={}", VIRTUAL_SOURCE_NAME),
                        format!("master={}.monitor", VIRTUAL_SINK_NAME),
                        format!(
                            "source_properties=device.description=\"{}\"",
                            VIRTUAL_MIC_DESCRIPTION
                        ),
                    ],
                ),
                CommandOutput {
                    success: true,
                    stdout: "34\n".to_string(),
                    stderr: String::new(),
                },
            ),
        ]);

        let runner = FakeRunner {
            commands: Rc::clone(&commands),
            responses: Rc::new(responses),
        };

        let status = create_virtual_mic_with_runner(&runner);
        assert!(status.active);
        assert_eq!(status.sink_name.as_deref(), Some(VIRTUAL_SINK_NAME));
        assert_eq!(status.source_name.as_deref(), Some(VIRTUAL_SOURCE_NAME));
        assert_eq!(commands.borrow().len(), 3);
    }
}
