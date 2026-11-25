//! Helpers shared by the integration tests.

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

/// Writes `script` as an executable `/bin/sh` script at `path`.
pub fn executable(path: &Path, script: &str) {
    fs::write(path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
