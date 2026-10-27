use std::io;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpStream};
use std::path::Path;
use std::time::Duration;

/// Client for OpenOCD's Tcl port.
#[derive(Debug)]
pub struct OpenOcd {
    reader: BufReader<TcpStream>,
    stream: TcpStream,
}

impl Drop for OpenOcd {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

impl OpenOcd {
    /// Tcl port message terminator, appended to commands and ending every response.
    const TOKEN: u8 = 0x1a;
    /// Socket read/write timeout.
    const TIMEOUT: Duration = Duration::from_secs(60);

    /// Connects to OpenOCD at `addr` (host:port).
    pub fn new(addr: &str) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_read_timeout(Some(Self::TIMEOUT))?;
        stream.set_write_timeout(Some(Self::TIMEOUT))?;
        tracing::info!("Connected to OpenOCD at {}", addr);
        Ok(Self {
            reader: BufReader::new(stream.try_clone()?),
            stream,
        })
    }

    /// Runs a Tcl command and returns its output; Tcl errors become `Err`.
    pub fn send_command(&mut self, command: &str) -> io::Result<String> {
        if command.contains(Self::TOKEN as char) {
            tracing::error!("Command is malformed!");
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Command is malformed!",
            ))
        } else {
            // The Tcl port reports errors as plain text; wrap the command to detect them.
            self.send(&format!(
                "if {{[catch {{{command}}} __r]}} {{set __r \"ERROR: $__r\"}}; set __r"
            ))?;
            let response = self.recv()?;
            if response.starts_with("ERROR:") {
                tracing::error!("OpenOCD command '{}' failed: {}", command, response);
                return Err(io::Error::other(response));
            }
            tracing::debug!("openocd '{}' -> {}", command, response);
            Ok(response)
        }
    }

    /// Sends a command followed by the terminator.
    fn send(&mut self, command: &str) -> io::Result<()> {
        let full_command = format!("{}{}", command, Self::TOKEN as char);
        self.stream.write_all(full_command.as_bytes())?;
        self.stream.flush()?;
        Ok(())
    }

    /// Reads one response up to the terminator.
    fn recv(&mut self) -> Result<String, io::Error> {
        let mut response = Vec::new();

        self.reader.read_until(Self::TOKEN, &mut response)?;
        if !response.ends_with(&[Self::TOKEN]) {
            tracing::error!("Failed to parse OpenOCD response. Something is missing.");
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Failed to read OpenOCD response",
            ));
        }

        response.pop();

        Ok(String::from_utf8_lossy(&response).into_owned())
    }

    /// Resets the target and halts it (`reset init`).
    pub fn reset_init(&mut self) -> io::Result<()> {
        self.send_command("reset halt")?;
        Ok(())
    }

    /// Resets and halts the target without running the `reset-init` event; used for recovery.
    pub fn reset_halt(&mut self) -> io::Result<()> {
        self.send_command("reset halt")?;
        Ok(())
    }

    /// Resets the target, loads the ELF, and halts it.
    pub fn load_program(&mut self, binary_path: &Path) -> io::Result<()> {
        tracing::info!(
            "Loading program: {}",
            binary_path.as_os_str().to_string_lossy()
        );

        self.reset_init()?;
        self.send_command(&format!(
            "load_image {} 0x0 elf",
            binary_path.as_os_str().to_string_lossy()
        ))?;
        self.send_command("set_reg {pc 0x80000000}")?;

        self.halt()?;

        tracing::info!("Program loaded successfully.");
        Ok(())
    }

    /// Halts the target (1 s timeout).
    pub fn halt(&mut self) -> io::Result<()> {
        self.send_command("halt 1000")?;
        Ok(())
    }

    /// Lets the target run.
    pub fn resume(&mut self) -> io::Result<()> {
        self.send_command("resume")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Starts a one-shot server that answers any command with `reply`; returns its address.
    fn fake_server(reply: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(sock.try_clone().unwrap());
            let mut buf = Vec::new();
            while reader.read_until(0x1a, &mut buf).unwrap_or(0) > 0 {
                buf.clear();
                sock.write_all(reply.as_bytes()).unwrap();
                sock.write_all(&[0x1a]).unwrap();
            }
        });
        addr
    }

    #[test]
    fn ok_response_is_returned() {
        let mut o = OpenOcd::new(&fake_server("fine")).unwrap();
        assert_eq!(o.send_command("halt").unwrap(), "fine");
    }

    #[test]
    fn tcl_error_is_detected() {
        let mut o = OpenOcd::new(&fake_server("ERROR: boom")).unwrap();
        assert!(o.send_command("halt").is_err());
    }
}
