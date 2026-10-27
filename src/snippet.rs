use std::{fmt, io, path::PathBuf};

#[derive(Debug)]
pub enum BuildError {
    Decode(String),
    Make(String),
    Io(io::Error),
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::Decode(m) => write!(f, "decode failed: {m}"),
            BuildError::Make(m) => write!(f, "make failed: {m}"),
            BuildError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for BuildError {}

impl From<io::Error> for BuildError {
    fn from(e: io::Error) -> Self {
        BuildError::Io(e)
    }
}

/// Turns a fuzz bitstream into `<assembly_environment>/snippet.elf`.
pub struct SnippetBuilder {
    assembly_environment: PathBuf,
    atndecode_path: PathBuf,
}

impl SnippetBuilder {
    pub fn new(assembly_environment: PathBuf, atndecode_path: PathBuf) -> Self {
        Self {
            assembly_environment,
            atndecode_path,
        }
    }

    /// Decodes `bitstream`, builds the snippet, and returns the path to the fresh ELF.
    pub fn build(&mut self, bitstream: &[u8]) -> Result<PathBuf, BuildError> {
        let elf = self.assembly_environment.join("snippet.elf");

        // Never load a stale ELF if this build fails.
        match std::fs::remove_file(&elf) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }

        self.decode(bitstream)?;
        self.make()?;

        if !elf.is_file() {
            return Err(BuildError::Make(format!(
                "{} was not produced",
                elf.display()
            )));
        }
        Ok(elf)
    }

    /// Runs `atndecode` on the bitstream and writes the assembly the Makefile consumes.
    fn decode(&self, _bitstream: &[u8]) -> Result<(), BuildError> {
        let _ = &self.atndecode_path;
        // TODO: port the decode flow from the other project.
        Err(BuildError::Decode("not implemented".into()))
    }

    /// Runs `make` in the assembly environment.
    fn make(&self) -> Result<(), BuildError> {
        // TODO: port the make invocation from the other project.
        Err(BuildError::Make("not implemented".into()))
    }
}
