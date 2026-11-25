//! Snippet build: writes the grammar-generated RISC-V assembly as
//! `src/snippet.S` in the assembly environment and runs `make` there to
//! produce `snippet.elf`. The assembly text is kept so it can be saved next
//! to interesting inputs.

use std::{
    fmt, io,
    path::{Path, PathBuf},
    process::Command,
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
    Make(String),
    Io {
        /// The operation and paths that failed, e.g. `spawning make in '/y'`.
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

/// Turns grammar-generated assembly source into `<assembly_environment>/snippet.elf`.
pub struct SnippetBuilder {
    assembly_environment: PathBuf,
    /// Toolchain directory exported to `make` as `RISCV`.
    riscv_toolpath: PathBuf,
    /// If set, every built snippet is also saved here as `NNNNNN.S` (for debugging).
    archive_dir: Option<PathBuf>,
    /// Number of snippets built so far, used for the archive file names.
    count: u64,
    /// Assembly text of the last successful build.
    last_assembly: Option<String>,
}

impl SnippetBuilder {
    /// Creates a builder for `assembly_environment` that builds with `make`
    /// using `riscv_toolpath` as `RISCV`.
    pub fn new(assembly_environment: PathBuf, riscv_toolpath: PathBuf) -> Self {
        Self {
            // Absolutized so TMPDIR (set to this directory for child processes) is
            // absolute and the toolchain does not fall back to /tmp.
            assembly_environment: absolute(&assembly_environment),
            riscv_toolpath,
            archive_dir: None,
            count: 0,
            last_assembly: None,
        }
    }

    /// Assembly text of the last successful build.
    pub fn last_assembly(&self) -> Option<&str> {
        self.last_assembly.as_deref()
    }

    /// Also saves every built snippet in `dir` (created if missing).
    pub fn with_archive(mut self, dir: PathBuf) -> io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        self.archive_dir = Some(dir);
        Ok(self)
    }

    /// The configured paths, for log messages when a build fails.
    pub fn paths_summary(&self) -> String {
        format!(
            "assembly_environment={}, riscv_toolpath={}, snippet_archive={}",
            self.assembly_environment.display(),
            self.riscv_toolpath.display(),
            self.archive_dir
                .as_ref()
                .map_or_else(|| "off".to_string(), |dir| dir.display().to_string()),
        )
    }

    /// Writes `source` as `src/snippet.S`, builds the snippet, and returns the path
    /// to the fresh ELF. The source is a complete assembly program (the grammar
    /// emits `_start:` and the data section itself), so no `init.inc` is included.
    pub fn build_from_source(&mut self, source: &str) -> Result<PathBuf, BuildError> {
        let elf = self.assembly_environment.join("snippet.elf");
        let src_dir = self.assembly_environment.join("src");

        tracing::debug!(
            "Building snippet from {} source bytes; env={} (dir: {}), src={} (dir: {}), RISCV={}, archive={:?}",
            source.len(),
            self.assembly_environment.display(),
            self.assembly_environment.is_dir(),
            src_dir.display(),
            src_dir.is_dir(),
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
        let start = Instant::now();
        self.write_source(source)?;
        let written = start.elapsed();
        self.make()?;
        tracing::debug!(
            "Built {} (write {:?}, make {:?})",
            elf.display(),
            written,
            start.elapsed() - written
        );

        if !elf.is_file() {
            return Err(BuildError::Make(format!(
                "{} was not produced",
                elf.display()
            )));
        }
        Ok(elf)
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

    // A fresh temp dir with a Makefile stands in for the assembly environment.
    fn env(makefile: &str) -> (PathBuf, SnippetBuilder) {
        let dir = std::env::temp_dir().join(format!("snippet_test_{}", rand_id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Makefile"), makefile).unwrap();
        (
            dir.clone(),
            SnippetBuilder::new(dir, "/opt/riscv".into()),
        )
    }

    fn rand_id() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[test]
    fn builds_source_verbatim_without_include() {
        let (dir, mut b) = env("snippet.elf: src/snippet.S\n\tcp $< $@\n");
        let elf = b.build_from_source("_start:\n\tnop\n").unwrap();
        // The grammar output is a complete program; no init.inc is prepended.
        assert_eq!(
            std::fs::read_to_string(elf).unwrap(),
            "_start:\n\tnop\n"
        );
        assert_eq!(b.last_assembly().unwrap(), "_start:\n\tnop\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn make_receives_riscv_variable() {
        let (dir, mut b) = env("snippet.elf:\n\techo $$RISCV > $@\n");
        let elf = b.build_from_source("x").unwrap();
        assert_eq!(std::fs::read_to_string(elf).unwrap().trim(), "/opt/riscv");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn relative_assembly_environment_is_absolutized() {
        // TMPDIR is only honored when absolute; a relative one makes the toolchain
        // fall back to /tmp from the child's working directory.
        let b = SnippetBuilder::new("rel_build".into(), "/opt/riscv".into());
        assert!(b.assembly_environment.is_absolute());
        assert!(b.assembly_environment.ends_with("rel_build"));
    }

    #[test]
    fn make_receives_tmpdir_inside_the_assembly_environment() {
        let (dir, mut b) = env("snippet.elf:\n\techo $$TMPDIR > $@\n");
        let elf = b.build_from_source("x").unwrap();
        // The toolchain must not fall back to /tmp for its intermediate files.
        assert_eq!(
            std::fs::read_to_string(elf).unwrap().trim(),
            dir.to_str().unwrap()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn archive_keeps_every_snippet() {
        let (dir, b) = env("snippet.elf:\n\ttouch $@\n");
        let archive = dir.join("archive");
        let mut b = b.with_archive(archive.clone()).unwrap();
        b.build_from_source("one").unwrap();
        b.build_from_source("two").unwrap();
        assert!(
            std::fs::read_to_string(archive.join("000001.S"))
                .unwrap()
                .contains("one")
        );
        assert!(
            std::fs::read_to_string(archive.join("000002.S"))
                .unwrap()
                .contains("two")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn make_failure_is_reported() {
        let (dir, mut b) = env("all:\n\tfalse\n");
        std::fs::write(dir.join("snippet.elf"), b"old").unwrap();
        // The stale ELF is never returned because a failed make is an error.
        assert!(matches!(
            b.build_from_source("x"),
            Err(BuildError::Make(_))
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_build_paths_are_named_in_the_error() {
        let dir = std::env::temp_dir().join(format!("snippet_test_{}", rand_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Makefile"), "all:\n\ttrue\n").unwrap();

        // Environment exists but has no src/ directory.
        let mut b = SnippetBuilder::new(dir.clone(), "/opt/riscv".into());
        let err = b.build_from_source("x").unwrap_err().to_string();
        assert!(err.contains("no src/ directory"), "{err}");
        assert!(err.contains(&dir.display().to_string()), "{err}");

        // The assembly environment itself is missing.
        let mut b = SnippetBuilder::new(dir.join("gone"), "/opt/riscv".into());
        let err = b.build_from_source("x").unwrap_err().to_string();
        assert!(err.contains("is not a directory"), "{err}");

        std::fs::remove_dir_all(dir).unwrap();
    }
}
