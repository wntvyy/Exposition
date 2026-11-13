//! `restrictedR`, `nativeR`, `restrictedJulia`, `nativeJulia`: R and Julia
//! workflow batches (M9). They are [`BatchAdapter`]s of their language over
//! the same research batch runner as the Python batches, with the same
//! contract (see [`super::batch`]). Language rules on top:
//! - the code is the step's language: a script input must have the
//!   language's extension (`.R`/`.r`, `.jl`) and an inline payload must be
//!   of the language, checked before anything is staged;
//! - a restricted step runs only in its language's configured image, pinned
//!   to the identity the run's plan bound for its runner (no approved
//!   identity: refused); the research runner refuses another identity and
//!   never falls back to native execution or to another language's image;
//! - a native step runs the discovered `Rscript`/`julia` with the user's
//!   account privileges (the runner checks folder trust before dispatch).

use std::sync::Arc;

use exposition_research::batch::{BatchLanguage, BatchMode, BatchRunner};

use super::Adapters;
use super::batch::BatchAdapter;

/// Runner ids of the R and Julia operations (see the registry).
pub mod runner_ids {
    pub const RESTRICTED_R: &str = "restrictedR";
    pub const NATIVE_R: &str = "nativeR";
    pub const RESTRICTED_JULIA: &str = "restrictedJulia";
    pub const NATIVE_JULIA: &str = "nativeJulia";
}

/// The (runner id, language, mode) of every R and Julia adapter.
pub const ADAPTERS: [(&str, BatchLanguage, BatchMode); 4] = [
    (runner_ids::RESTRICTED_R, BatchLanguage::R, BatchMode::Restricted),
    (runner_ids::NATIVE_R, BatchLanguage::R, BatchMode::TrustedNative),
    (runner_ids::RESTRICTED_JULIA, BatchLanguage::Julia, BatchMode::Restricted),
    (runner_ids::NATIVE_JULIA, BatchLanguage::Julia, BatchMode::TrustedNative),
];

/// Install the four R and Julia adapters over `runner` (the same research
/// batch runner the Python adapters use; `None`: they report themselves
/// unavailable).
pub fn install(adapters: &mut Adapters, runner: Option<Arc<dyn BatchRunner>>) {
    for (id, language, mode) in ADAPTERS {
        adapters.insert(id.to_owned(), Arc::new(BatchAdapter::for_language(runner.clone(), language, mode)));
    }
}

#[cfg(test)]
mod rj_adapter_tests {
    use std::path::Path;

    use exposition_graph::workflow::registry::{self, CodeLanguage};

    use super::*;

    #[test]
    fn adapters_cover_exactly_the_r_and_julia_operations() {
        let mut adapters = Adapters::new();
        install(&mut adapters, None);
        let ops = registry::operations();
        for (id, language, mode) in ADAPTERS {
            let adapter = BatchAdapter::for_language(None, language, mode);
            let op = ops.iter().find(|o| o.id == adapter.operation_id()).expect("registered");
            assert_eq!(op.runner.id, id, "{id} runs {}", op.id);
            assert_eq!(registry::code_language(op), Some(adapter.code_language()));
            assert_eq!(
                op.runner.containment == registry::Containment::RestrictedContainer,
                mode == BatchMode::Restricted
            );
            let reason = adapters.get(id).unwrap().availability(Path::new(".")).unwrap_err();
            assert!(reason.contains(language.title()), "no runner: unavailable, {reason}");
            assert_eq!(adapter.restricted_image().is_some(), mode == BatchMode::Restricted);
        }
        let r_and_julia: Vec<&str> = ops
            .iter()
            .filter(|o| matches!(registry::code_language(o), Some(CodeLanguage::R | CodeLanguage::Julia)))
            .map(|o| o.runner.id.as_str())
            .collect();
        assert_eq!(r_and_julia.len(), ADAPTERS.len());
        assert!(r_and_julia.iter().all(|id| adapters.contains_key(*id)));
        // Python batches use the default restricted image (no image of their own).
        assert!(BatchAdapter::new(None, BatchMode::Restricted).restricted_image().is_none());
    }
}
