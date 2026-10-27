use std::{
    fmt, io,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::Instant,
};

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
    /// Toolchain directory exported to `make` as `RISCV`.
    riscv_toolpath: PathBuf,
    /// If set, every decoded snippet is also saved here as `NNNNNN.S` (for debugging).
    archive_dir: Option<PathBuf>,
    /// Number of snippets decoded so far, used for the archive file names.
    count: u64,
}

impl SnippetBuilder {
    pub fn new(
        assembly_environment: PathBuf,
        atndecode_path: PathBuf,
        riscv_toolpath: PathBuf,
    ) -> Self {
        Self {
            assembly_environment,
            atndecode_path,
            riscv_toolpath,
            archive_dir: None,
            count: 0,
        }
    }

    /// Also saves every decoded snippet in `dir` (created if missing).
    pub fn with_archive(mut self, dir: PathBuf) -> io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        self.archive_dir = Some(dir);
        Ok(self)
    }

    /// Decodes `bitstream`, builds the snippet, and returns the path to the fresh ELF.
    pub fn build(&mut self, bitstream: &[u8]) -> Result<PathBuf, BuildError> {
        let elf = self.assembly_environment.join("snippet.elf");

        tracing::debug!("Building snippet from {} input bytes", bitstream.len());
        let start = Instant::now();
        self.decode(bitstream)?;
        let decoded = start.elapsed();
        self.make()?;
        tracing::debug!(
            "Built {} (decode {:?}, make {:?})",
            elf.display(),
            decoded,
            start.elapsed() - decoded
        );

        if !elf.is_file() {
            return Err(BuildError::Make(format!(
                "{} was not produced",
                elf.display()
            )));
        }
        Ok(elf)
    }

    /// Pipes the bitstream into `atndecode` and writes its stdout to `src/snippet.S`,
    /// preceded by `#include "init.inc"`.
    fn decode(&mut self, bitstream: &[u8]) -> Result<(), BuildError> {
        let mut child = Command::new(&self.atndecode_path)
            .current_dir(&self.assembly_environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Write from a thread so a full stdout pipe cannot deadlock us; dropping stdin sends EOF.
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let data = bitstream.to_vec();
        let writer = thread::spawn(move || stdin.write_all(&data));

        let out = child.wait_with_output()?;
        // The decoder may exit without reading all input; a broken pipe is then not an error.
        match writer.join().expect("stdin writer panicked") {
            Err(e) if e.kind() != io::ErrorKind::BrokenPipe => return Err(e.into()),
            _ => {}
        }

        tracing::debug!(
            "atndecode: {}, {} bytes of assembly",
            out.status,
            out.stdout.len()
        );
        if !out.status.success() {
            tracing::debug!("atndecode stderr: {}", String::from_utf8_lossy(&out.stderr));
            return Err(BuildError::Decode(format!(
                "{}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }

        let assembly = format!(
            "#include \"init.inc\"\n\n{}\n",
            String::from_utf8_lossy(&out.stdout)
        );
        let source = self.assembly_environment.join("src").join("snippet.S");
        std::fs::write(source, &assembly)?;

        self.count += 1;
        if let Some(dir) = &self.archive_dir {
            let copy = dir.join(format!("{:06}.S", self.count));
            std::fs::write(&copy, &assembly)?;
            tracing::debug!("Saved snippet to {}", copy.display());
        }
        Ok(())
    }

    /// Runs `make` in the assembly environment.
    fn make(&self) -> Result<(), BuildError> {
        let out = Command::new("make")
            .arg("-B")
            .env("RISCV", &self.riscv_toolpath)
            .current_dir(&self.assembly_environment)
            .output()?;
        tracing::debug!("make: {}", out.status);
        if !out.status.success() {
            tracing::debug!(
                "make stdout: {}\nmake stderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return Err(BuildError::Make(format!(
                "{}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Existing binaries stand in for the decoder; freshly written scripts race with
    // parallel tests on "Text file busy".
    fn env(decoder: &str, makefile: &str) -> (PathBuf, SnippetBuilder) {
        let dir = std::env::temp_dir().join(format!("snippet_test_{}", rand_id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Makefile"), makefile).unwrap();
        (
            dir.clone(),
            SnippetBuilder::new(dir, decoder.into(), "/opt/riscv".into()),
        )
    }

    fn rand_id() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[test]
    fn builds_from_stdin() {
        let (dir, mut b) = env("/bin/cat", "snippet.elf: src/snippet.S\n\tcp $< $@\n");
        let elf = b.build(b"nop\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(elf).unwrap(),
            "#include \"init.inc\"\n\nnop\n\n"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn make_receives_riscv_variable() {
        let (dir, mut b) = env("/bin/cat", "snippet.elf:\n\techo $$RISCV > $@\n");
        let elf = b.build(b"x").unwrap();
        assert_eq!(std::fs::read_to_string(elf).unwrap().trim(), "/opt/riscv");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn archive_keeps_every_snippet() {
        let (dir, b) = env("/bin/cat", "snippet.elf:\n\ttouch $@\n");
        let archive = dir.join("archive");
        let mut b = b.with_archive(archive.clone()).unwrap();
        b.build(b"one").unwrap();
        b.build(b"two").unwrap();
        assert!(std::fs::read_to_string(archive.join("000001.S"))
            .unwrap()
            .contains("one"));
        assert!(std::fs::read_to_string(archive.join("000002.S"))
            .unwrap()
            .contains("two"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn decode_failure_is_reported() {
        let (dir, mut b) = env("/bin/false", "all:\n\ttrue\n");
        assert!(matches!(b.build(b"x"), Err(BuildError::Decode(_))));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn make_failure_is_reported() {
        let (dir, mut b) = env("/bin/cat", "all:\n\tfalse\n");
        std::fs::write(dir.join("snippet.elf"), b"old").unwrap();
        // The stale ELF is never returned because a failed make is an error.
        assert!(matches!(b.build(b"x"), Err(BuildError::Make(_))));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
