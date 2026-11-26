//! Snippet build: pipes the input bitstream into `atndecode` to decode RISC-V
//! assembly, validates and fixes the decoded program with the built-in ISA gate
//! (`src/isagate/`), writes it as `src/snippet.S` in the assembly environment
//! (preceded by the required `#include "init.inc"`), and runs `make` there to
//! produce `snippet.elf`. The fixed assembly is kept so it can be saved next to
//! interesting inputs.

/// Prepended to every generated snippet. The assembly environment's `init.inc`
/// provides the machine init: the linker script enters at `_init`, which sets
/// `mtvec`, PMP, SATP, delegates traps, enables the FPU and loads `gp`, then
/// falls through into the snippet body. Without it, float instructions trap
/// (FS is off) and arbitrary memory accesses trap (no PMP/SATP setup).
const SNIPPET_INIT_INCLUDE: &str = "#include \"init.inc\"\n\n";

use std::{
    fmt, io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Instant,
};

/// Absolutizes `path` against the current directory. Toolchain temp files honor TMPDIR
/// only when it is absolute; a relative TMPDIR is unresolvable from the child's working
/// directory and the toolchain silently falls back to /tmp.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

/// Why a snippet build failed.
#[derive(Debug)]
pub enum BuildError {
    Decode(String),
    /// The built-in ISA gate could not fix the decoded program; the input is skipped.
    Gate(String),
    Make(String),
    Io {
        /// The operation and paths that failed, e.g. `spawning decoder '/x' in '/y'`.
        context: String,
        source: Option<io::Error>,
    },
}

impl BuildError {
    /// An I/O failure, annotated with the operation and paths that caused it.
    fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source: Some(source),
        }
    }

    /// A precondition failure (e.g. a missing directory) without an OS error.
    fn missing(context: impl Into<String>) -> Self {
        Self::Io {
            context: context.into(),
            source: None,
        }
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::Decode(m) => write!(f, "decode failed: {m}"),
            BuildError::Gate(m) => write!(f, "ISA gate rejected the decoded snippet: {m}"),
            BuildError::Make(m) => write!(f, "make failed: {m}"),
            BuildError::Io { context, source } => match source {
                Some(source) => write!(f, "I/O error while {context}: {source}"),
                None => write!(f, "I/O error: {context}"),
            },
        }
    }
}

impl std::error::Error for BuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BuildError::Io {
                source: Some(source),
                ..
            } => Some(source),
            _ => None,
        }
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
    /// Number of decoded programs the ISA gate rejected (unfixable) so far.
    gate_rejections: u64,
    /// Assembly text of the last successful decode.
    last_assembly: Option<String>,
}

impl SnippetBuilder {
    /// Creates a builder for `assembly_environment` that decodes with
    /// `atndecode_path` and builds with `make` using `riscv_toolpath` as `RISCV`.
    pub fn new(
        assembly_environment: PathBuf,
        atndecode_path: PathBuf,
        riscv_toolpath: PathBuf,
    ) -> Self {
        Self {
            // Absolutized so TMPDIR (set to this directory for child processes) is
            // absolute and the toolchain does not fall back to /tmp.
            assembly_environment: absolute(&assembly_environment),
            atndecode_path,
            riscv_toolpath,
            archive_dir: None,
            count: 0,
            gate_rejections: 0,
            last_assembly: None,
        }
    }

    /// Assembly text (with the `init.inc` include) of the last successful decode.
    pub fn last_assembly(&self) -> Option<&str> {
        self.last_assembly.as_deref()
    }

    /// How many decoded programs the ISA gate has rejected (unfixable) so far.
    pub fn gate_rejections(&self) -> u64 {
        self.gate_rejections
    }

    /// Also saves every decoded snippet in `dir` (created if missing).
    pub fn with_archive(mut self, dir: PathBuf) -> io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        self.archive_dir = Some(dir);
        Ok(self)
    }

    /// The configured paths, for log messages when a build fails.
    pub fn paths_summary(&self) -> String {
        format!(
            "assembly_environment={}, atndecode={}, riscv_toolpath={}, snippet_archive={}",
            self.assembly_environment.display(),
            self.atndecode_path.display(),
            self.riscv_toolpath.display(),
            self.archive_dir
                .as_ref()
                .map_or_else(|| "off".to_string(), |dir| dir.display().to_string()),
        )
    }

    /// Decodes `bitstream`, runs the ISA gate on the decoded program, builds the
    /// snippet, and returns the path to the fresh ELF. A program the gate cannot
    /// fix is rejected (`BuildError::Gate`) and never built.
    pub fn build(&mut self, bitstream: &[u8]) -> Result<PathBuf, BuildError> {
        let elf = self.assembly_environment.join("snippet.elf");
        let src_dir = self.assembly_environment.join("src");

        tracing::debug!(
            "Building snippet from {} input bytes; env={} (dir: {}), src={} (dir: {}), decoder={} (file: {}), RISCV={}, archive={:?}",
            bitstream.len(),
            self.assembly_environment.display(),
            self.assembly_environment.is_dir(),
            src_dir.display(),
            src_dir.is_dir(),
            self.atndecode_path.display(),
            self.atndecode_path.is_file(),
            self.riscv_toolpath.display(),
            self.archive_dir,
        );
        if !self.assembly_environment.is_dir() {
            return Err(BuildError::missing(format!(
                "assembly environment {} is not a directory",
                self.assembly_environment.display()
            )));
        }
        if !src_dir.is_dir() {
            return Err(BuildError::missing(format!(
                "assembly environment {} has no src/ directory",
                self.assembly_environment.display()
            )));
        }
        if !self.atndecode_path.is_file() {
            return Err(BuildError::missing(format!(
                "decoder {} is not a file",
                self.atndecode_path.display()
            )));
        }
        let start = Instant::now();
        let decoded_assembly = self.decode(bitstream)?;
        let decoded = start.elapsed();
        // The built-in ISA gate validates and fixes the bare decoded program
        // (memory operands, alignment, immediates, branch targets); the
        // `init.inc` include is prepended only afterwards, so the gate only
        // ever sees the program itself.
        let fixed = match crate::isagate::gate(&decoded_assembly) {
            Ok(fixed) => fixed,
            Err(e) => {
                self.gate_rejections += 1;
                tracing::debug!(
                    "ISA gate rejected input of {} bytes (rejections so far: {}): {e}",
                    bitstream.len(),
                    self.gate_rejections
                );
                return Err(BuildError::Gate(e.to_string()));
            }
        };
        let source = format!("{SNIPPET_INIT_INCLUDE}{fixed}\n");
        self.write_source(&source)?;
        let written = start.elapsed() - decoded;
        self.make()?;
        tracing::debug!(
            "Built {} (decode {:?}, gate+write {:?}, make {:?})",
            elf.display(),
            decoded,
            written,
            start.elapsed() - decoded - written
        );

        if !elf.is_file() {
            return Err(BuildError::Make(format!(
                "{} was not produced",
                elf.display()
            )));
        }
        Ok(elf)
    }

    /// Pipes the bitstream into `atndecode` and returns its stdout as the bare
    /// decoded assembly (without the `init.inc` include).
    fn decode(&mut self, bitstream: &[u8]) -> Result<String, BuildError> {
        tracing::debug!(
            "Running decoder {:?} in {:?} with {} bytes on stdin",
            self.atndecode_path,
            self.assembly_environment,
            bitstream.len()
        );
        let mut child = Command::new(&self.atndecode_path)
            .current_dir(&self.assembly_environment)
            // Toolchain temp files must not go to /tmp (often size-limited); the assembly
            // environment already holds the build outputs, so it has space.
            .env("TMPDIR", &self.assembly_environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                BuildError::io(
                    format!(
                        "spawning decoder {} in {}",
                        self.atndecode_path.display(),
                        self.assembly_environment.display()
                    ),
                    e,
                )
            })?;

        // Write from a thread so a full stdout pipe cannot deadlock us; dropping stdin sends EOF.
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let data = bitstream.to_vec();
        let writer = thread::spawn(move || stdin.write_all(&data));

        let out = child.wait_with_output().map_err(|e| {
            BuildError::io(
                format!("waiting for decoder {}", self.atndecode_path.display()),
                e,
            )
        })?;
        // The decoder may exit without reading all input; a broken pipe is then not an error.
        match writer.join().expect("stdin writer panicked") {
            Err(e) if e.kind() != io::ErrorKind::BrokenPipe => {
                return Err(BuildError::io(
                    format!(
                        "writing {} input bytes to decoder {}",
                        bitstream.len(),
                        self.atndecode_path.display()
                    ),
                    e,
                ));
            }
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

        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Writes `source` to `src/snippet.S`, keeps it as the last assembly, and
    /// archives a copy if an archive directory is configured.
    fn write_source(&mut self, source: &str) -> Result<(), BuildError> {
        let path = self.assembly_environment.join("src").join("snippet.S");
        std::fs::write(&path, source).map_err(|e| {
            BuildError::io(format!("writing assembly source {}", path.display()), e)
        })?;

        self.count += 1;
        self.last_assembly = Some(source.to_string());
        if let Some(dir) = &self.archive_dir {
            let copy = dir.join(format!("{:06}.S", self.count));
            std::fs::write(&copy, source).map_err(|e| {
                BuildError::io(format!("writing snippet archive {}", copy.display()), e)
            })?;
            tracing::debug!("Saved snippet to {}", copy.display());
        }
        Ok(())
    }

    /// Runs `make` in the assembly environment.
    fn make(&self) -> Result<(), BuildError> {
        tracing::debug!(
            "Running make -B in {:?} with RISCV={:?} and TMPDIR={:?}",
            self.assembly_environment,
            self.riscv_toolpath,
            self.assembly_environment
        );
        let out = Command::new("make")
            .arg("-B")
            .env("RISCV", &self.riscv_toolpath)
            .env("TMPDIR", &self.assembly_environment)
            .current_dir(&self.assembly_environment)
            .output()
            .map_err(|e| {
                BuildError::io(
                    format!(
                        "spawning make (resolved via PATH) in {} with RISCV={}",
                        self.assembly_environment.display(),
                        self.riscv_toolpath.display()
                    ),
                    e,
                )
            })?;
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
        let elf = b.build(b"mv x1,x2\n").unwrap();
        // The ISA gate labels every statement; the include is prepended after it.
        assert_eq!(
            std::fs::read_to_string(elf).unwrap(),
            "#include \"init.inc\"\n\n.Lbr0:\nmv x1,x2\n\n"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn gate_rejection_is_reported_and_counted() {
        let (dir, mut b) = env("/bin/cat", "snippet.elf: src/snippet.S\n\tcp $< $@\n");
        // `bogus` is not an instruction: the gate cannot fix it and rejects it.
        assert!(matches!(b.build(b"bogus\n"), Err(BuildError::Gate(_))));
        assert_eq!(b.gate_rejections(), 1);
        // A rejected program is never written or built.
        assert!(!dir.join("src").join("snippet.S").exists());
        assert!(!dir.join("snippet.elf").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn make_receives_riscv_variable() {
        let (dir, mut b) = env("/bin/cat", "snippet.elf:\n\techo $$RISCV > $@\n");
        let elf = b.build(b"mv x1,x2\n").unwrap();
        assert_eq!(std::fs::read_to_string(elf).unwrap().trim(), "/opt/riscv");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn relative_assembly_environment_is_absolutized() {
        // TMPDIR is only honored when absolute; a relative one makes the toolchain
        // fall back to /tmp from the child's working directory.
        let b = SnippetBuilder::new("rel_build".into(), "/bin/cat".into(), "/opt/riscv".into());
        assert!(b.assembly_environment.is_absolute());
        assert!(b.assembly_environment.ends_with("rel_build"));
    }

    #[test]
    fn make_receives_tmpdir_inside_the_assembly_environment() {
        let (dir, mut b) = env("/bin/cat", "snippet.elf:\n\techo $$TMPDIR > $@\n");
        let elf = b.build(b"mv x1,x2\n").unwrap();
        // The toolchain must not fall back to /tmp for its intermediate files.
        assert_eq!(
            std::fs::read_to_string(elf).unwrap().trim(),
            dir.to_str().unwrap()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn archive_keeps_every_snippet() {
        let (dir, b) = env("/bin/cat", "snippet.elf:\n\ttouch $@\n");
        let archive = dir.join("archive");
        let mut b = b.with_archive(archive.clone()).unwrap();
        b.build(b"mv x1,x2\n").unwrap();
        b.build(b"mv x3,x4\n").unwrap();
        assert!(
            std::fs::read_to_string(archive.join("000001.S"))
                .unwrap()
                .contains("mv x1,x2")
        );
        assert!(
            std::fs::read_to_string(archive.join("000002.S"))
                .unwrap()
                .contains("mv x3,x4")
        );
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
        assert!(matches!(b.build(b"mv x1,x2\n"), Err(BuildError::Make(_))));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_build_paths_are_named_in_the_error() {
        let dir = std::env::temp_dir().join(format!("snippet_test_{}", rand_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Makefile"), "all:\n\ttrue\n").unwrap();

        // Environment exists but has no src/ directory.
        let mut b = SnippetBuilder::new(dir.clone(), "/bin/cat".into(), "/opt/riscv".into());
        let err = b.build(b"x").unwrap_err().to_string();
        assert!(err.contains("no src/ directory"), "{err}");
        assert!(err.contains(&dir.display().to_string()), "{err}");

        // src/ exists but the decoder binary is missing.
        std::fs::create_dir(dir.join("src")).unwrap();
        let mut b = SnippetBuilder::new(
            dir.clone(),
            "/nonexistent/decode".into(),
            "/opt/riscv".into(),
        );
        let err = b.build(b"x").unwrap_err().to_string();
        assert!(
            err.contains("decoder /nonexistent/decode is not a file"),
            "{err}"
        );

        // The assembly environment itself is missing.
        let mut b = SnippetBuilder::new(dir.join("gone"), "/bin/cat".into(), "/opt/riscv".into());
        let err = b.build(b"x").unwrap_err().to_string();
        assert!(err.contains("is not a directory"), "{err}");

        // The decoder exists but cannot be executed: the spawn error names it.
        let decoder = dir.join("decoder");
        std::fs::write(&decoder, b"#!/bin/sh\n").unwrap();
        let mut b = SnippetBuilder::new(dir.clone(), decoder.clone(), "/opt/riscv".into());
        let err = b.build(b"x").unwrap_err().to_string();
        assert!(err.contains("spawning decoder"), "{err}");
        assert!(err.contains(&decoder.display().to_string()), "{err}");
        assert!(err.contains("Permission denied"), "{err}");

        std::fs::remove_dir_all(dir).unwrap();
    }
}
