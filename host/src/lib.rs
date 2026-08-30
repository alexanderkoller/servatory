use std::{
    fs,
    io::{Read as _, Write},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;
use servatory_protocol::{
    DeviceMessage, HostMessage, MAX_HOST_FRAME_LEN, ShutdownFailure, ShutdownPhase, encode_host,
};

const SHUTDOWN_FAILURE_DISPLAY_TIME: Duration = Duration::from_secs(3);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs a child process with captured output and a hard deadline.
#[must_use]
pub fn command_output(executable: &str, args: &[&str], timeout: Duration) -> Option<Output> {
    let mut child = Command::new(executable)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;

    thread::scope(|scope| {
        let stdout_reader = scope.spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).ok()?;
            Some(bytes)
        });
        let stderr_reader = scope.spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).ok()?;
            Some(bytes)
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20));
                }
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    break child.wait().ok()?;
                }
            }
        };
        Some(Output {
            status,
            stdout: stdout_reader.join().ok()??,
            stderr: stderr_reader.join().ok()??,
        })
    })
}

pub trait Shutdown {
    /// Requests an orderly operating-system shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error if progress cannot be reported or a shutdown command fails.
    fn poweroff(&mut self, output: &mut impl Write) -> anyhow::Result<()>;
}

pub struct SystemdShutdown;

fn system_poweroff_command() -> Command {
    let mut command = Command::new("/usr/bin/systemctl");
    command.args(["poweroff", "--no-block"]);
    command
}

impl Shutdown for SystemdShutdown {
    fn poweroff(&mut self, output: &mut impl Write) -> anyhow::Result<()> {
        write_shutdown_progress(output, ShutdownPhase::PreparingGuests, 0, 0)?;

        let host_name = fs::read_to_string("/etc/hostname").unwrap_or_else(|_| "proxmox".into());
        let host_name = host_name.trim();
        let initial = match running_guest_count(host_name) {
            Ok(initial) => {
                write_shutdown_progress(output, ShutdownPhase::StoppingGuests, initial, initial)?;
                Some(initial)
            }
            Err(error) => {
                eprintln!("could not count guests before poweroff: {error:#}");
                None
            }
        };

        let status = system_poweroff_command().status().map_err(|error| {
            report_shutdown_failure(
                output,
                ShutdownFailure::HostPoweroff,
                initial.unwrap_or(0),
                error.into(),
            )
        })?;
        if !status.success() {
            return Err(report_shutdown_failure(
                output,
                ShutdownFailure::HostPoweroff,
                initial.unwrap_or(0),
                anyhow::anyhow!("systemctl poweroff --no-block exited with {status}"),
            ));
        }

        let Some(initial) = initial else {
            write_shutdown_progress(output, ShutdownPhase::PoweringOff, 0, 0)?;
            return Ok(());
        };
        let remaining = monitor_system_guest_shutdown(host_name, initial, output)?;
        if remaining == 0 {
            write_shutdown_progress(output, ShutdownPhase::GuestsStopped, initial, 0)?;
            thread::sleep(Duration::from_millis(500));
        }
        write_shutdown_progress(output, ShutdownPhase::PoweringOff, initial, remaining)?;
        Ok(())
    }
}

fn monitor_system_guest_shutdown(
    host_name: &str,
    total: u16,
    output: &mut impl Write,
) -> anyhow::Result<u16> {
    let mut last_remaining = total;
    loop {
        thread::sleep(Duration::from_secs(1));
        let remaining = match running_guest_count(host_name) {
            Ok(remaining) => remaining,
            Err(error) => {
                eprintln!("guest monitoring ended during host shutdown: {error:#}");
                return Ok(last_remaining);
            }
        };
        last_remaining = remaining;
        // Repeating unchanged counts acts as a shutdown heartbeat for the display.
        write_shutdown_progress(output, ShutdownPhase::StoppingGuests, total, remaining)?;
        if remaining == 0 {
            return Ok(0);
        }
    }
}

fn running_guest_count(host_name: &str) -> anyhow::Result<u16> {
    let output = command_output(
        "/usr/bin/pvesh",
        &[
            "get",
            "/cluster/resources",
            "--type",
            "vm",
            "--output-format",
            "json",
        ],
        COMMAND_TIMEOUT,
    )
    .ok_or_else(|| anyhow::anyhow!("could not execute pvesh guest query"))?;
    if !output.status.success() {
        anyhow::bail!("pvesh guest query exited with {}", output.status);
    }
    running_guest_count_from_json(&output.stdout, host_name)
}

fn running_guest_count_from_json(bytes: &[u8], host_name: &str) -> anyhow::Result<u16> {
    let values: Vec<Value> = serde_json::from_slice(bytes)?;
    let count = values
        .iter()
        .filter(|value| {
            value
                .get("node")
                .and_then(Value::as_str)
                .is_none_or(|node| node == host_name)
        })
        .filter(|value| value.get("status").and_then(Value::as_str) == Some("running"))
        .count();
    Ok(u16::try_from(count).unwrap_or(u16::MAX))
}

fn report_shutdown_failure(
    output: &mut impl Write,
    reason: ShutdownFailure,
    guests_remaining: u16,
    error: anyhow::Error,
) -> anyhow::Error {
    if let Err(report_error) = write_shutdown_failure(output, reason, guests_remaining) {
        error.context(format!(
            "also failed to report shutdown failure: {report_error:#}"
        ))
    } else {
        error
    }
}

fn write_shutdown_progress(
    output: &mut impl Write,
    phase: ShutdownPhase,
    guests_total: u16,
    guests_remaining: u16,
) -> anyhow::Result<()> {
    write_host_message(
        HostMessage::ShutdownProgress {
            phase,
            guests_total,
            guests_remaining,
        },
        output,
    )?;
    output.flush()?;
    Ok(())
}

fn write_shutdown_failure(
    output: &mut impl Write,
    reason: ShutdownFailure,
    guests_remaining: u16,
) -> anyhow::Result<()> {
    write_host_message(
        HostMessage::ShutdownFailed {
            reason,
            guests_remaining,
        },
        output,
    )?;
    output.flush()?;
    // Keep the daemon from immediately replacing the failure with a health snapshot.
    thread::sleep(SHUTDOWN_FAILURE_DISPLAY_TIME);
    Ok(())
}

pub struct EventHandler<S> {
    shutdown: S,
    allow_shutdown: bool,
}

impl<S: Shutdown> EventHandler<S> {
    pub const fn new(shutdown: S, allow_shutdown: bool) -> Self {
        Self {
            shutdown,
            allow_shutdown,
        }
    }

    /// Handles a message and returns true when the process should stop.
    ///
    /// # Errors
    ///
    /// Returns an error if the acknowledgement cannot be sent or shutdown fails.
    pub fn handle(
        &mut self,
        message: &DeviceMessage,
        output: &mut impl Write,
    ) -> anyhow::Result<bool> {
        match message {
            DeviceMessage::Hello { .. } => Ok(false),
            DeviceMessage::ShutdownRequested if self.allow_shutdown => {
                write_host_message(HostMessage::ShutdownAccepted, output)?;
                output.flush()?;
                match self.shutdown.poweroff(output) {
                    Ok(()) => Ok(true),
                    Err(error) => {
                        eprintln!("shutdown failed: {error:#}");
                        Ok(false)
                    }
                }
            }
            DeviceMessage::ShutdownRequested => {
                eprintln!("ignoring shutdown request: start with --allow-shutdown to enable it");
                Ok(false)
            }
        }
    }
}

/// Serializes and writes a typed host message as one COBS frame.
///
/// # Errors
///
/// Returns an error if serialization or writing fails.
pub fn write_host_message(message: HostMessage, output: &mut impl Write) -> anyhow::Result<()> {
    let mut storage = [0_u8; MAX_HOST_FRAME_LEN];
    let frame = encode_host(message, &mut storage)
        .map_err(|error| anyhow::anyhow!("encoding host message: {error}"))?;
    output.write_all(frame)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeShutdown {
        calls: usize,
    }

    impl Shutdown for FakeShutdown {
        fn poweroff(&mut self, _output: &mut impl Write) -> anyhow::Result<()> {
            self.calls += 1;
            Ok(())
        }
    }

    #[test]
    fn shutdown_requires_explicit_permission() {
        let mut output = Vec::new();
        let mut handler = EventHandler::new(FakeShutdown::default(), false);
        assert!(
            !handler
                .handle(&DeviceMessage::ShutdownRequested, &mut output)
                .unwrap()
        );
        assert!(output.is_empty());
        assert_eq!(handler.shutdown.calls, 0);
    }

    #[test]
    fn accepted_shutdown_is_acknowledged_before_poweroff() {
        let mut output = Vec::new();
        let mut handler = EventHandler::new(FakeShutdown::default(), true);
        assert!(
            handler
                .handle(&DeviceMessage::ShutdownRequested, &mut output)
                .unwrap()
        );
        assert_eq!(
            servatory_protocol::decode_host(&mut output),
            Ok(HostMessage::ShutdownAccepted)
        );
        assert_eq!(handler.shutdown.calls, 1);
    }

    #[test]
    fn counts_only_running_guests_on_the_local_node() {
        let count = running_guest_count_from_json(
            br#"[
                {"node":"pve-01","status":"running"},
                {"node":"pve-01","status":"stopped"},
                {"node":"pve-02","status":"running"},
                {"node":"pve-01","status":"running"}
            ]"#,
            "pve-01",
        )
        .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn host_poweroff_is_queued_without_blocking_the_guest_monitor() {
        let command = system_poweroff_command();
        assert_eq!(command.get_program(), "/usr/bin/systemctl");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["poweroff", "--no-block"]
        );
    }
}
