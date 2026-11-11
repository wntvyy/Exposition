//! The bundled kernel host helper (`workers/kernel-host`).
//!
//! The helper source is compiled into the application and installed, by content
//! hash, into an application-owned directory before launch. It is therefore
//! versioned with the application and never fetched or installed from elsewhere.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::ResearchError;

/// Helper protocol version this supervisor speaks.
pub const PROTOCOL_VERSION: u32 = 1;
/// Helper release this supervisor bundles.
pub const HELPER_VERSION: &str = "0.1.0";
pub const HELPER_FILE_NAME: &str = "exposition_kernel_host.py";
pub const HELPER_SOURCE: &str = include_str!("../../../workers/kernel-host/exposition_kernel_host.py");

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Install the helper under `dir` (once per content hash) and return its path.
pub fn install_helper(dir: &Path) -> Result<PathBuf, ResearchError> {
    let hash = sha256_hex(HELPER_SOURCE.as_bytes());
    let target_dir = dir.join(format!("{HELPER_VERSION}-{}", &hash[..12]));
    let target = target_dir.join(HELPER_FILE_NAME);
    if let Ok(existing) = std::fs::read(&target)
        && sha256_hex(&existing) == hash
    {
        return Ok(target);
    }
    std::fs::create_dir_all(&target_dir).map_err(|e| ResearchError::io("creating helper directory", e))?;
    let partial = target_dir.join(format!("{HELPER_FILE_NAME}.{}.partial", std::process::id()));
    std::fs::write(&partial, HELPER_SOURCE.as_bytes()).map_err(|e| ResearchError::io("writing helper", e))?;
    std::fs::rename(&partial, &target).map_err(|e| ResearchError::io("installing helper", e))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_helper_matches_supervisor_versions() {
        assert!(HELPER_SOURCE.contains(&format!("PROTOCOL_VERSION = {PROTOCOL_VERSION}\n")));
        assert!(HELPER_SOURCE.contains(&format!("HELPER_VERSION = \"{HELPER_VERSION}\"\n")));
    }

    #[test]
    fn install_is_idempotent_and_repairs_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let first = install_helper(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&first).unwrap(), HELPER_SOURCE);
        std::fs::write(&first, "print('tampered')").unwrap();
        let second = install_helper(dir.path()).unwrap();
        assert_eq!(first, second);
        assert_eq!(std::fs::read_to_string(&second).unwrap(), HELPER_SOURCE);
    }
}
