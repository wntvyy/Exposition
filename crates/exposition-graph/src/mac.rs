//! Sealing evidence recorded by this installation (HMAC-SHA256, RFC 2104).
//!
//! A project's graph database travels with the project (clones, archives,
//! shared folders) and may be written by anything. Evidence a service of
//! this installation registers is sealed with a MAC under a key kept in the
//! application data folder (never in the project); evidence without a valid
//! seal is *imported* — a historical claim of its recorded origin, never
//! presented as current local verification.

use std::path::Path;

use sha2::{Digest, Sha256};

const BLOCK: usize = 64;

/// HMAC-SHA256 of `message` under `key`.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner);
    outer.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The seal of an evidence record: over its id and its stored body.
pub fn evidence_mac(key: &[u8; 32], id: &str, body: &str) -> String {
    let mut message = Vec::with_capacity(id.len() + 1 + body.len());
    message.extend_from_slice(id.as_bytes());
    message.push(0);
    message.extend_from_slice(body.as_bytes());
    format!("v1:{}", hex(&hmac_sha256(key, &message)))
}

/// Whether `mac` seals `id` + `body` under `key` (compared in constant time).
pub fn verify_evidence_mac(key: &[u8; 32], id: &str, body: &str, mac: &str) -> bool {
    let expected = evidence_mac(key, id, body);
    expected.len() == mac.len() && expected.bytes().zip(mac.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// File (in the application data folder) holding the installation's key.
pub const KEY_FILE: &str = "graph-evidence.key";

/// The installation's evidence key: read from `data_dir`, created on first
/// use. When it cannot be stored, a key for this session only is returned
/// (evidence recorded now then reads as imported in a later session —
/// never the other way round).
pub fn installation_key(data_dir: &Path) -> [u8; 32] {
    let path = data_dir.join(KEY_FILE);
    if let Ok(bytes) = std::fs::read(&path)
        && bytes.len() == 32
    {
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        return key;
    }
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let _ = std::fs::create_dir_all(data_dir);
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut f) => {
            use std::io::Write;
            if f.write_all(&key).and_then(|_| f.sync_all()).is_err() {
                let _ = std::fs::remove_file(&path);
            }
            key
        }
        // Another process created it meanwhile: use theirs.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match std::fs::read(&path) {
            Ok(bytes) if bytes.len() == 32 => {
                key.copy_from_slice(&bytes);
                key
            }
            _ => key,
        },
        Err(_) => key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231_test_vectors() {
        // Test case 2.
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 6 (key longer than the block).
        assert_eq!(
            hex(&hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn seals_bind_the_id_and_body_to_the_key() {
        let key = [7u8; 32];
        let mac = evidence_mac(&key, "evd-1", "{}");
        assert!(verify_evidence_mac(&key, "evd-1", "{}", &mac));
        assert!(!verify_evidence_mac(&key, "evd-2", "{}", &mac));
        assert!(!verify_evidence_mac(&key, "evd-1", "{ }", &mac));
        assert!(!verify_evidence_mac(&[8u8; 32], "evd-1", "{}", &mac));
        assert!(!verify_evidence_mac(&key, "evd-1", "{}", "v1:00"));
    }

    #[test]
    fn the_installation_key_persists_in_the_data_folder() {
        let dir = tempfile::tempdir().unwrap();
        let a = installation_key(dir.path());
        let b = installation_key(dir.path());
        assert_eq!(a, b);
        assert_ne!(a, installation_key(tempfile::tempdir().unwrap().path()));
    }
}
