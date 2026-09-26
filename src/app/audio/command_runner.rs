use std::fs;
use std::io;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
const SYSTEMCTL_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub trait CommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemCommandRunner;

pub fn run_command(program: &str, args: &[&str]) -> io::Result<CommandOutput> {
    SystemCommandRunner.run(program, args)
}

#[cfg_attr(test, allow(dead_code))]
pub fn run_command_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> io::Result<CommandOutput> {
    run_with_timeout(program, args, timeout)
}

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
        run_with_timeout(program, args, command_timeout(program))
    }
}

fn run_with_timeout(program: &str, args: &[&str], timeout: Duration) -> io::Result<CommandOutput> {
    let runtime_dir = crate::private_runtime::directory()?;
    let token = uuid::Uuid::new_v4();
    let stdout_path = runtime_dir.join(format!("command-{token}.stdout"));
    let stderr_path = runtime_dir.join(format!("command-{token}.stderr"));
    let stdout_file = crate::private_runtime::open_new(&stdout_path)?;
    let stderr_file = match crate::private_runtime::open_new(&stderr_path) {
        Ok(file) => file,
        Err(error) => {
            let _ = fs::remove_file(&stdout_path);
            return Err(error);
        }
    };
    let child = Command::new(program)
        .args(args)
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return Err(error);
        }
    };
    let result = (|| {
        let started_at = Instant::now();
        let status = loop {
            match child.try_wait()? {
                Some(status) => break status,
                None if started_at.elapsed() >= timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "{} {} timed out after {} ms",
                            program,
                            args.join(" "),
                            timeout.as_millis()
                        ),
                    ));
                }
                None => std::thread::sleep(COMMAND_POLL_INTERVAL),
            }
        };
        let stdout = fs::read(&stdout_path)?;
        let stderr = fs::read(&stderr_path)?;
        Ok(CommandOutput {
            success: status.success(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    })();
    let _ = fs::remove_file(&stdout_path);
    let _ = fs::remove_file(&stderr_path);
    result
}

fn command_timeout(program: &str) -> Duration {
    if program == "systemctl" {
        SYSTEMCTL_COMMAND_TIMEOUT
    } else {
        DEFAULT_COMMAND_TIMEOUT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_success_output() {
        let output = run_with_timeout("sh", &["-c", "printf ok"], Duration::from_secs(1))
            .expect("command succeeds");
        assert!(output.success);
        assert_eq!(output.stdout, "ok");
    }

    #[test]
    fn captures_output_larger_than_a_pipe_buffer() {
        let script =
            "i=0; while [ $i -lt 20000 ]; do printf 12345678901234567890; i=$((i+1)); done";
        let output = run_with_timeout("sh", &["-c", script], Duration::from_secs(2))
            .expect("large-output command succeeds");
        assert!(output.success);
        assert_eq!(output.stdout.len(), 400000);
    }

    #[test]
    fn terminates_a_hung_command() {
        let error = run_with_timeout("sh", &["-c", "sleep 5"], Duration::from_millis(50))
            .expect_err("command must time out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
