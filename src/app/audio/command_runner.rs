use std::io;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_millis(900);
const SYSTEMCTL_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
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

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
        let mut child = Command::new(program)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let timeout = command_timeout(program);
        let started_at = Instant::now();
        loop {
            match child.try_wait()? {
                Some(_) => {
                    let output = child.wait_with_output()?;
                    return Ok(CommandOutput {
                        success: output.status.success(),
                        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    });
                }
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
        }
    }
}

fn command_timeout(program: &str) -> Duration {
    if program == "systemctl" {
        SYSTEMCTL_COMMAND_TIMEOUT
    } else {
        DEFAULT_COMMAND_TIMEOUT
    }
}
