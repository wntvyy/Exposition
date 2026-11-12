//! Fonts visible to the Typst compiler.
//!
//! Font sets are loaded once per process (embedded Typst fonts first, then
//! optionally system fonts) and shared by every compiler context. The
//! fingerprint identifies the set for build configuration records.

use std::sync::{Arc, OnceLock};

use sha2::{Digest, Sha256};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst_kit::fonts::{FontStore, embedded, system};

use crate::snapshot::hex;

pub struct Fonts {
    store: FontStore,
    fingerprint: String,
    count: usize,
}

impl std::fmt::Debug for Fonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fonts").field("count", &self.count).field("fingerprint", &self.fingerprint).finish()
    }
}

impl Fonts {
    fn load(include_system: bool) -> Self {
        let mut store = FontStore::new();
        let mut hasher = Sha256::new();
        let mut count = 0;
        for (font, info) in embedded() {
            hasher.update(format!("embedded:{}:{:?}\n", info.family, info.variant).as_bytes());
            store.push((font, info));
            count += 1;
        }
        if include_system {
            for (path, info) in system() {
                hasher.update(format!("system:{}:{:?}:{}\n", info.family, info.variant, path.index).as_bytes());
                store.push((path, info));
                count += 1;
            }
        }
        Self { store, fingerprint: hex(&hasher.finalize()[..12]), count }
    }

    /// Embedded fonts only (deterministic across machines).
    pub fn embedded_only() -> Arc<Self> {
        static FONTS: OnceLock<Arc<Fonts>> = OnceLock::new();
        Arc::clone(FONTS.get_or_init(|| Arc::new(Self::load(false))))
    }

    /// Embedded fonts followed by the system's fonts.
    pub fn with_system() -> Arc<Self> {
        static FONTS: OnceLock<Arc<Fonts>> = OnceLock::new();
        Arc::clone(FONTS.get_or_init(|| Arc::new(Self::load(true))))
    }

    pub fn book(&self) -> &LazyHash<FontBook> {
        self.store.book()
    }

    pub fn font(&self, index: usize) -> Option<Font> {
        self.store.font(index)
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}
