use std::path::Path;
use std::sync::Mutex;

/// Logs to `path`, truncating any existing file. The writer is unbuffered so nothing is lost if a crash handler kills the process.
/// Filter with `RUST_LOG` (default: info).
pub fn init(path: &Path) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    tracing_subscriber::fmt()
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    Ok(())
}
