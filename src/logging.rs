use std::{io, path::Path, sync::Mutex};
use tracing_subscriber::EnvFilter;

/// Logs to `path`, truncating any existing file. The writer is unbuffered so nothing is lost if a crash handler kills the process.
///
/// The filter comes from `level` if given (`--log-level` / `log_level`), else from `RUST_LOG`
/// (e.g. `warn,libafl_fuzzer::openocd=debug`), else `info`. LibAFL itself logs through the
/// `log` crate; those records are bridged into tracing so they land in the same file and
/// can be filtered by the same directives.
pub fn init(path: &Path, level: Option<&str>) -> io::Result<()> {
    // The "tracing-log" feature makes fmt::init() also install the `log` -> tracing bridge.
    let file = std::fs::File::create(path)?;
    let filter = match level {
        Some(level) => EnvFilter::try_new(level).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid log level or filter {level:?}: {e}"),
            )
        })?,
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
    };
    tracing_subscriber::fmt()
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .with_env_filter(filter)
        .init();
    Ok(())
}
