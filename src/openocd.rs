use std::io;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpStream};
use std::path::Path;

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
    const TOKEN: u8 = 0x1a;

    pub fn new(addr: &str) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        println!("Connected to OpenOCD at {}", addr);
        Ok(Self {
            reader: BufReader::new(stream.try_clone()?),
            stream,
        })
    }

    pub fn send_command(&mut self, command: &str) -> io::Result<String> {
        if command.contains(Self::TOKEN as char) {
            eprintln!("Command is malformed!");
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Command is malformed!",
            ))
        } else {
            self.send(command)?;
            self.recv()
        }
    }

    fn send(&mut self, command: &str) -> io::Result<()> {
        let full_command = format!("{}{}", command, Self::TOKEN as char);
        self.stream.write_all(full_command.as_bytes())?;
        self.stream.flush()?;
        Ok(())
    }

    fn recv(&mut self) -> Result<String, io::Error> {
        let mut response = Vec::new();

        self.reader.read_until(Self::TOKEN, &mut response)?;
        if !response.ends_with(&[Self::TOKEN]) {
            eprintln!("Failed to parse OpenOCD response. Something is missing.");
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Failed to read OpenOCD response",
            ));
        }

        response.pop();

        Ok(String::from_utf8_lossy(&response).parse().unwrap())
    }

    pub fn reset_init(&mut self) -> io::Result<()> {
        self.send_command("reset init")?;
        Ok(())
    }

    pub fn load_program(&mut self, binary_path: &Path, start_addr: u32) -> io::Result<()> {
        println!(
            "Loading program: {}",
            binary_path.as_os_str().to_string_lossy()
        );

        self.reset_init()?;
        self.send_command(&format!(
            "load {}",
            binary_path.as_os_str().to_string_lossy()
        ))?;

        self.halt()?;
        self.set_pc(start_addr)?;
        self.halt()?;

        println!("Program loaded successfully.");
        Ok(())
    }

    pub fn halt(&mut self) -> io::Result<()> {
        self.send_command("halt 1000")?;
        Ok(())
    }

    pub fn resume(&mut self) -> io::Result<()> {
        self.send_command("resume")?;
        Ok(())
    }

    pub fn set_pc(&mut self, pc: u32) -> io::Result<()> {
        self.send_command(&format!("set_reg {{pc 0x{:08x}}}", pc))?;
        Ok(())
    }
}
