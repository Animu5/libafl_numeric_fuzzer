use std::error::Error;
use std::ffi::CString;
use std::fmt;
use std::io::{self, Read, Write};
use std::time::Duration;
use visa_rs::enums::attribute::{AttrTmoValue, HasAttribute};
use visa_rs::prelude::{AccessMode, AsResourceManager, DefaultRM, Instrument};
use visa_rs::{ResID, TIMEOUT_IMMEDIATE};

/// Errors from the VISA connection to the oscilloscope.
#[derive(Debug)]
pub enum ScopeError {
    /// The VISA library or its default resource manager is unavailable.
    ResourceManager(String),
    /// Listing or parsing a resource string failed (e.g. it contains a NUL byte).
    InvalidResource(String),
    /// Opening the instrument failed.
    Open { resource: String, message: String },
    /// The requested I/O timeout is not accepted by VISA.
    InvalidTimeout(Duration),
    /// Setting a VISA attribute failed.
    Attribute(String),
    /// Writing a command failed.
    Write(io::Error),
    /// Reading a response failed or timed out.
    Read(io::Error),
}

impl fmt::Display for ScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResourceManager(m) => write!(f, "VISA resource manager: {m}"),
            Self::InvalidResource(m) => write!(f, "invalid VISA resource: {m}"),
            Self::Open { resource, message } => write!(f, "opening {resource}: {message}"),
            Self::InvalidTimeout(t) => write!(f, "invalid scope timeout: {t:?}"),
            Self::Attribute(m) => write!(f, "setting VISA attribute: {m}"),
            Self::Write(e) => write!(f, "scope write failed: {e}"),
            Self::Read(e) => write!(f, "scope read failed: {e}"),
        }
    }
}

impl Error for ScopeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Write(e) | Self::Read(e) => Some(e),
            _ => None,
        }
    }
}

/// Minimal VISA connection to the oscilloscope for sending and querying SCPI commands.
pub struct Scope {
    // Declared first so the session is dropped before the resource manager.
    instr: Instrument,
    _rm: DefaultRM,
}

impl Scope {
    /// Lists VISA resources matching a VISA resource expression, such as `"USB?*INSTR"`.
    ///
    /// Use the returned resource name with [`Scope::connect`].
    pub fn list_resources(expr: &str) -> Result<Vec<String>, ScopeError> {
        let rm = DefaultRM::new().map_err(|e| ScopeError::ResourceManager(e.to_string()))?;
        let expr: ResID = CString::new(expr)
            .map_err(|e| ScopeError::InvalidResource(e.to_string()))?
            .into();
        let mut list = rm
            .find_res_list(&expr)
            .map_err(|e| ScopeError::InvalidResource(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(res) = list
            .find_next()
            .map_err(|e| ScopeError::InvalidResource(e.to_string()))?
        {
            out.push(res.to_string());
        }
        Ok(out)
    }

    /// Opens a VISA resource and verifies the connection by querying `*IDN?`
    ///
    /// `timeout` configures the instrument's I/O timeout. Opening the resource does not
    /// wait for a VISA lock.
    pub fn connect(resource: &str, timeout: Duration) -> Result<Self, ScopeError> {
        let rm = DefaultRM::new().map_err(|e| ScopeError::ResourceManager(e.to_string()))?;
        let res: ResID = CString::new(resource)
            .map_err(|e| ScopeError::InvalidResource(e.to_string()))?
            .into();
        let instr = rm
            .open(&res, AccessMode::NO_LOCK, TIMEOUT_IMMEDIATE)
            .map_err(|e| ScopeError::Open {
                resource: resource.to_string(),
                message: e.to_string(),
            })?;

        let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        let tmo = AttrTmoValue::new_checked(ms).ok_or(ScopeError::InvalidTimeout(timeout))?;
        instr
            .set_attr(tmo)
            .map_err(|e| ScopeError::Attribute(e.to_string()))?;

        let mut scope = Self { instr, _rm: rm };
        let idn = scope.query("*IDN?")?;
        tracing::info!("Connected to scope: {}", idn);
        Ok(scope)
    }

    /// Sends an SCPI command, appending the newline command terminator.
    pub fn write(&mut self, cmd: &str) -> Result<(), ScopeError> {
        tracing::debug!("scope <- {}", cmd);
        (&self.instr)
            .write_all(format!("{cmd}\n").as_bytes())
            .map_err(ScopeError::Write)
    }

    /// Sends an SCPI query and returns its response with the surrounding whitespace removed.
    ///
    /// VISA's configured timeout bounds the read. The response is read in one VISA
    /// read operation; commands should return a response that fits in the buffer.
    pub fn query(&mut self, cmd: &str) -> Result<String, ScopeError> {
        self.write(cmd)?;
        let mut buf = vec![0u8; 64 * 1024];
        let n = (&self.instr).read(&mut buf).map_err(ScopeError::Read)?;
        let line = String::from_utf8_lossy(&buf[..n]).trim().to_string();
        tracing::debug!("scope -> {}", line);
        Ok(line)
    }
}
