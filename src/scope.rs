use std::ffi::CString;
use std::io::{Read, Write};
use std::time::Duration;
use visa_rs::enums::attribute::{AttrTmoValue, HasAttribute};
use visa_rs::prelude::{AccessMode, AsResourceManager, DefaultRM, Instrument};
use visa_rs::{ResID, TIMEOUT_IMMEDIATE};

/// Minimal VISA connection to the oscilloscope (write/query only).
pub struct Scope {
    // Declared first so the session is dropped before the resource manager.
    instr: Instrument,
    _rm: DefaultRM,
}

impl Scope {
    /// Lists VISA resources matching `expr` (e.g. "USB?*INSTR").
    pub fn list_resources(expr: &str) -> Result<Vec<String>, String> {
        let rm = DefaultRM::new().map_err(|e| e.to_string())?;
        let expr: ResID = CString::new(expr).map_err(|e| e.to_string())?.into();
        let mut list = rm.find_res_list(&expr).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        while let Some(res) = list.find_next().map_err(|e| e.to_string())? {
            out.push(res.to_string());
        }
        Ok(out)
    }

    pub fn connect(resource: &str, timeout: Duration) -> Result<Self, String> {
        let rm = DefaultRM::new().map_err(|e| format!("VISA resource manager: {e}"))?;
        let res: ResID = CString::new(resource).map_err(|e| e.to_string())?.into();
        let instr = rm
            .open(&res, AccessMode::NO_LOCK, TIMEOUT_IMMEDIATE)
            .map_err(|e| format!("Opening {resource}: {e}"))?;

        let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        let tmo = AttrTmoValue::new_checked(ms).ok_or("Invalid timeout")?;
        instr.set_attr(tmo).map_err(|e| e.to_string())?;

        let mut scope = Self { instr, _rm: rm };
        let idn = scope.query("*IDN?")?;
        tracing::info!("Connected to scope: {}", idn);
        Ok(scope)
    }

    pub fn write(&mut self, cmd: &str) -> Result<(), String> {
        tracing::debug!("scope <- {}", cmd);
        (&self.instr)
            .write_all(format!("{cmd}\n").as_bytes())
            .map_err(|e| format!("Scope write failed: {e}"))
    }

    pub fn query(&mut self, cmd: &str) -> Result<String, String> {
        self.write(cmd)?;
        let mut buf = vec![0u8; 64 * 1024];
        let n = (&self.instr)
            .read(&mut buf)
            .map_err(|e| format!("Scope read failed: {e}"))?;
        let line = String::from_utf8_lossy(&buf[..n]).trim().to_string();
        tracing::debug!("scope -> {}", line);
        Ok(line)
    }
}
