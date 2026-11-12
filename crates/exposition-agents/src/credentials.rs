//! Provider credentials in OS credential storage.
//!
//! The API key is stored only in the platform credential store (Windows
//! Credential Manager, macOS Keychain, Linux keyutils). It is never written to
//! project files, the task database, logs, events, or child-process
//! environments, and the webview only ever learns whether a key is configured.
//!
//! On Windows the key is kept on this machine only (local-machine persistence,
//! not the roaming "enterprise" persistence the keyring crate uses); a key
//! saved by an earlier version is moved there, byte for byte, the first time
//! it is read. Code running as the same user (trusted native kernels,
//! commands, builds) can still read it through the Credential Manager API.

use std::sync::Mutex;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    #[error("credential storage is unavailable: {0}")]
    Unavailable(String),
    #[error("the API key is not valid: {0}")]
    Invalid(String),
}

/// A secret value that is redacted from `Debug` output.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// One stored secret (the provider API key).
pub trait SecretStore: Send + Sync {
    /// Human-readable store name, e.g. "Windows Credential Manager".
    fn describe(&self) -> String;
    fn get(&self) -> Result<Option<Secret>, CredentialError>;
    fn set(&self, secret: &str) -> Result<(), CredentialError>;
    fn delete(&self) -> Result<(), CredentialError>;
}

pub const KEYRING_SERVICE: &str = "org.exposition.desktop";
pub const KEYRING_USER: &str = "anthropic-api-key";

/// Validate and normalize a pasted API key without revealing it in errors.
pub fn normalize_api_key(raw: &str) -> Result<String, CredentialError> {
    let key = raw.trim();
    if key.is_empty() {
        return Err(CredentialError::Invalid("the key is empty".into()));
    }
    if key.len() < 16 || key.len() > 512 {
        return Err(CredentialError::Invalid("the key length is not plausible".into()));
    }
    if !key.chars().all(|c| c.is_ascii_graphic()) {
        return Err(CredentialError::Invalid("the key contains whitespace or non-ASCII characters".into()));
    }
    Ok(key.to_owned())
}

/// The platform credential store.
pub struct KeyringStore {
    service: String,
    user: String,
    /// The stored key's persistence was checked (Windows).
    #[cfg(windows)]
    persistence_checked: AtomicBool,
}

impl KeyringStore {
    /// `namespace` isolates development/test entries from the real one.
    pub fn new(namespace: Option<&str>) -> Self {
        let service = match namespace {
            Some(ns) if !ns.is_empty() => format!("{KEYRING_SERVICE}.{ns}"),
            _ => KEYRING_SERVICE.to_owned(),
        };
        Self {
            service,
            user: KEYRING_USER.to_owned(),
            #[cfg(windows)]
            persistence_checked: AtomicBool::new(false),
        }
    }

    /// The Credential Manager target name (keyring's `user.service`).
    #[cfg(windows)]
    pub fn target(&self) -> String {
        format!("{}.{}", self.user, self.service)
    }

    /// Keep a key saved with roaming persistence (earlier versions) on this
    /// machine only. Runs once per store; failures leave the key as it was.
    #[cfg(windows)]
    fn keep_on_this_machine(&self) {
        if self.persistence_checked.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Err(code) = crate::wincred::keep_on_this_machine(&self.target()) {
            tracing::warn!("could not change the stored key's persistence (Win32 error {code}); it stays as it was");
        }
    }

    fn entry(&self) -> Result<keyring::Entry, CredentialError> {
        keyring::Entry::new(&self.service, &self.user).map_err(|e| CredentialError::Unavailable(describe_error(&e)))
    }
}

fn describe_error(error: &keyring::Error) -> String {
    // keyring errors never contain the secret, but keep messages generic anyway.
    match error {
        keyring::Error::NoStorageAccess(_) => "access to the credential store was denied".into(),
        keyring::Error::PlatformFailure(_) => "the credential store reported a failure".into(),
        keyring::Error::TooLong(..) => "the key is too long for the credential store".into(),
        _ => "the credential store returned an error".into(),
    }
}

impl SecretStore for KeyringStore {
    fn describe(&self) -> String {
        if cfg!(windows) {
            "Windows Credential Manager".into()
        } else if cfg!(target_os = "macos") {
            "macOS Keychain".into()
        } else {
            "OS keyring".into()
        }
    }

    fn get(&self) -> Result<Option<Secret>, CredentialError> {
        #[cfg(windows)]
        self.keep_on_this_machine();
        match self.entry()?.get_password() {
            Ok(value) => Ok(Some(Secret(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CredentialError::Unavailable(describe_error(&e))),
        }
    }

    fn set(&self, secret: &str) -> Result<(), CredentialError> {
        // Windows: written in keyring's layout, but kept on this machine only.
        #[cfg(windows)]
        {
            match crate::wincred::write_password(
                &self.target(),
                secret,
                &self.user,
                "Exposition provider API key",
                crate::wincred::PERSIST_LOCAL_MACHINE,
            ) {
                Ok(true) => Ok(()),
                Ok(false) => {
                    Err(CredentialError::Unavailable("the key did not read back as saved; try saving it again".into()))
                }
                Err(code) => Err(CredentialError::Unavailable(format!("the credential store reported error {code}"))),
            }
        }
        #[cfg(not(windows))]
        {
            self.entry()?.set_password(secret).map_err(|e| CredentialError::Unavailable(describe_error(&e)))
        }
    }

    fn delete(&self) -> Result<(), CredentialError> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CredentialError::Unavailable(describe_error(&e))),
        }
    }
}

/// In-memory store for tests.
#[derive(Default)]
pub struct MemoryStore(Mutex<Option<String>>);

impl SecretStore for MemoryStore {
    fn describe(&self) -> String {
        "in-memory test store".into()
    }

    fn get(&self) -> Result<Option<Secret>, CredentialError> {
        Ok(self.0.lock().unwrap_or_else(|p| p.into_inner()).clone().map(Secret))
    }

    fn set(&self, secret: &str) -> Result<(), CredentialError> {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(secret.to_owned());
        Ok(())
    }

    fn delete(&self) -> Result<(), CredentialError> {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_redacted_and_keys_validated() {
        let secret = Secret("sk-ant-very-secret-value".into());
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert!(normalize_api_key("  sk-ant-abcdefghijklmnop \n").is_ok());
        assert!(normalize_api_key("short").is_err());
        let err = normalize_api_key("sk-ant-abc def ghijklmnop").unwrap_err();
        assert!(!err.to_string().contains("ghijklmnop"));
    }
}
