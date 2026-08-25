use std::{
    io::{self, Write},
    process::Command,
};

use s3_display_protocol::{ButtonAction, DeviceMessage, HostMessage, MAX_FRAME_LEN, encode_host};

pub trait Shutdown {
    /// Requests an orderly operating-system shutdown.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the command cannot start or exits unsuccessfully.
    fn poweroff(&mut self) -> io::Result<()>;
}

pub struct SystemdShutdown;

impl Shutdown for SystemdShutdown {
    fn poweroff(&mut self) -> io::Result<()> {
        let status = Command::new("/usr/bin/systemctl")
            .arg("poweroff")
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "systemctl poweroff exited with {status}"
            )))
        }
    }
}

pub struct EventHandler<S> {
    shutdown: S,
    allow_shutdown: bool,
    session_established: bool,
}

impl<S: Shutdown> EventHandler<S> {
    pub const fn new(shutdown: S, allow_shutdown: bool) -> Self {
        Self {
            shutdown,
            allow_shutdown,
            session_established: false,
        }
    }

    /// Handles a message and returns true when the process should stop.
    ///
    /// # Errors
    ///
    /// Returns an error if the acknowledgement cannot be sent or shutdown fails.
    pub fn handle(
        &mut self,
        message: DeviceMessage,
        output: &mut impl Write,
    ) -> anyhow::Result<bool> {
        match message {
            DeviceMessage::Ready => {
                eprintln!("display connected");
                Ok(false)
            }
            DeviceMessage::Ack { sequence } => {
                if !self.session_established {
                    eprintln!("display acknowledged update {sequence}; session established");
                    self.session_established = true;
                }
                Ok(false)
            }
            DeviceMessage::Button(ButtonAction::NextScreen) => {
                eprintln!("display selected the next screen");
                Ok(false)
            }
            DeviceMessage::Button(ButtonAction::ShutdownRequested) if self.allow_shutdown => {
                write_host_message(HostMessage::ShutdownAccepted, output)?;
                output.flush()?;
                self.shutdown.poweroff()?;
                Ok(true)
            }
            DeviceMessage::Button(ButtonAction::ShutdownRequested) => {
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
    let mut storage = [0_u8; MAX_FRAME_LEN];
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
        fn poweroff(&mut self) -> io::Result<()> {
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
                .handle(
                    DeviceMessage::Button(ButtonAction::ShutdownRequested),
                    &mut output
                )
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
                .handle(
                    DeviceMessage::Button(ButtonAction::ShutdownRequested),
                    &mut output
                )
                .unwrap()
        );
        assert_eq!(
            s3_display_protocol::decode_host(&mut output),
            Ok(HostMessage::ShutdownAccepted)
        );
        assert_eq!(handler.shutdown.calls, 1);
    }
}
