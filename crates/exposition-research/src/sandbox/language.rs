//! Language profiles of workflow batches: what differs between Python, R,
//! and Julia batches (M9). Everything else — staging, containment, limits,
//! cancellation, output registration — is shared and identical.
//!
//! | | Python | R | Julia |
//! |---|---|---|---|
//! | restricted image | `docker.image` | `docker.rImage` | `docker.juliaImage` |
//! | container program | `python3 <entry>` | `Rscript --vanilla <entry>` | `julia --startup-file=no --history-file=no <entry>` |
//! | native program | discovered Python | discovered `Rscript` | discovered `julia` |
//! | inline entry | `__exposition_main__.py` | `__exposition_main__.R` | `__exposition_main__.jl` |
//! | script extensions | `.py` | `.R`, `.r` | `.jl` |
//!
//! A language's restricted batch runs only in that language's configured
//! image; an unconfigured image refuses the batch (never another image, never
//! native execution).

use std::collections::BTreeMap;

use exposition_execution::settings::RestrictedLanguage;

use crate::batch::BatchLanguage;
use crate::types::{RunErrorInfo, RuntimeLanguage};

/// The standard container `PATH` with the official Julia image's binary
/// folder first (the image's own `PATH` is replaced by the constructed
/// environment).
const JULIA_CONTAINER_PATH: &str = "/usr/local/julia/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Name the inline code is staged under (at the working-copy root).
pub(crate) fn inline_entry(language: BatchLanguage) -> &'static str {
    match language {
        BatchLanguage::Python => super::stage::INLINE_ENTRY,
        BatchLanguage::R => "__exposition_main__.R",
        BatchLanguage::Julia => "__exposition_main__.jl",
    }
}

/// The image setting of the language's restricted batches (`None`: the
/// default restricted image).
pub(crate) fn restricted_language(language: BatchLanguage) -> Option<RestrictedLanguage> {
    match language {
        BatchLanguage::Python => None,
        BatchLanguage::R => Some(RestrictedLanguage::R),
        BatchLanguage::Julia => Some(RestrictedLanguage::Julia),
    }
}

/// Where the language's restricted image is configured (for guidance).
pub(crate) fn image_setting(language: BatchLanguage) -> &'static str {
    match restricted_language(language) {
        None => "the restricted-execution image",
        Some(RestrictedLanguage::R) => "the restricted R image",
        Some(RestrictedLanguage::Julia) => "the restricted Julia image",
    }
}

/// The discovered runtime trusted native batches use.
pub(crate) fn runtime_language(language: BatchLanguage) -> RuntimeLanguage {
    match language {
        BatchLanguage::Python => RuntimeLanguage::Python,
        BatchLanguage::R => RuntimeLanguage::R,
        BatchLanguage::Julia => RuntimeLanguage::Julia,
    }
}

/// Lowercase tool name recorded in the run's environment (`python`, `r`, `julia`).
pub(crate) fn tool_name(language: BatchLanguage) -> &'static str {
    match language {
        BatchLanguage::Python => "python",
        BatchLanguage::R => "r",
        BatchLanguage::Julia => "julia",
    }
}

/// The program inside the container image.
pub(crate) fn container_program(language: BatchLanguage) -> &'static str {
    match language {
        BatchLanguage::Python => "python3",
        BatchLanguage::R => "Rscript",
        BatchLanguage::Julia => "julia",
    }
}

/// Interpreter options placed before the script (both modes): no user or
/// site start-up files, no saved workspace or history.
pub(crate) fn interpreter_options(language: BatchLanguage) -> &'static [&'static str] {
    match language {
        BatchLanguage::Python => &[],
        BatchLanguage::R => &["--vanilla"],
        BatchLanguage::Julia => &["--startup-file=no", "--history-file=no"],
    }
}

/// Arguments running `script` (as the program sees its path).
pub(crate) fn arguments(language: BatchLanguage, script: String) -> Vec<String> {
    let mut args: Vec<String> = interpreter_options(language).iter().map(|o| (*o).to_owned()).collect();
    args.push(script);
    args
}

/// Variables set inside the container (besides `EXPOSITION_OUTPUT_DIR`).
pub(crate) fn container_environment(language: BatchLanguage) -> BTreeMap<String, String> {
    let pairs: &[(&str, &str)] = match language {
        BatchLanguage::Python => &[
            ("PYTHONDONTWRITEBYTECODE", "1"),
            ("PYTHONUNBUFFERED", "1"),
            ("PYTHONIOENCODING", "utf-8"),
            ("MPLBACKEND", "Agg"),
            ("MPLCONFIGDIR", "/tmp/matplotlib"),
        ],
        BatchLanguage::R => &[],
        // The depot defaults to `$HOME/.julia` = `/tmp/.julia` (writable
        // tmpfs) followed by the image's own depots.
        BatchLanguage::Julia => &[("PATH", JULIA_CONTAINER_PATH)],
    };
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

/// Variables set for a trusted native batch (besides the output area and
/// the runtime's own library locations from discovery).
pub(crate) fn native_environment(language: BatchLanguage) -> BTreeMap<String, String> {
    match language {
        BatchLanguage::Python => BTreeMap::from([("MPLBACKEND".to_owned(), "Agg".to_owned())]),
        BatchLanguage::R | BatchLanguage::Julia => BTreeMap::new(),
    }
}

/// Why trusted native batches of the language cannot run: nothing found.
pub(crate) fn native_missing(language: BatchLanguage) -> (String, String) {
    match language {
        BatchLanguage::Python => (
            "No Python interpreter was found for trusted native batches.".into(),
            "Install Python 3 (or create a project .venv) and refresh research environments.".into(),
        ),
        BatchLanguage::R => (
            "No R installation (Rscript) was found for trusted native batches.".into(),
            "Install R from https://cran.r-project.org/ and refresh research environments. Exposition never installs runtimes itself."
                .into(),
        ),
        BatchLanguage::Julia => (
            "No Julia installation was found for trusted native batches.".into(),
            "Install Julia from https://julialang.org/downloads/ and refresh research environments. Exposition never installs runtimes itself."
                .into(),
        ),
    }
}

fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name.split('.').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
}

/// `Name: message` of the last Python exception line in `stderr`, if any.
fn python_error(stderr: &str) -> Option<RunErrorInfo> {
    let line = stderr.lines().rev().map(str::trim).find(|l| !l.is_empty())?;
    let (name, message) = line.split_once(": ").map_or((line, ""), |(n, m)| (n, m));
    let valid = identifier(name)
        && (name.ends_with("Error")
            || name.ends_with("Exception")
            || name.ends_with("Interrupt")
            || name == "SystemExit");
    valid.then(|| RunErrorInfo { ename: name.to_owned(), evalue: message.to_owned() })
}

/// The last R error of `stderr`: `Error in <call> : <message>` or
/// `Error: <message>` (the message may continue on the next line).
fn r_error(stderr: &str) -> Option<RunErrorInfo> {
    let lines: Vec<&str> = stderr.lines().collect();
    let at = lines.iter().rposition(|l| l.starts_with("Error in ") || l.starts_with("Error: ") || *l == "Error:")?;
    let line = lines[at];
    let message = if let Some(rest) = line.strip_prefix("Error: ") {
        rest.trim()
    } else {
        line.split_once(" : ").map(|(_, m)| m.trim()).unwrap_or("")
    };
    let message = if message.is_empty() { lines.get(at + 1).map(|l| l.trim()).unwrap_or("") } else { message };
    Some(RunErrorInfo { ename: "Error".into(), evalue: message.chars().take(500).collect() })
}

/// The first Julia `ERROR:` line of `stderr`, unwrapped from `LoadError`.
fn julia_error(stderr: &str) -> Option<RunErrorInfo> {
    let line = stderr.lines().find_map(|l| l.trim().strip_prefix("ERROR: "))?;
    let line = line.strip_prefix("LoadError: ").unwrap_or(line);
    let (name, message) = match line.split_once(": ") {
        Some((name, message)) if identifier(name) => (name, message),
        _ => ("Error", line),
    };
    Some(RunErrorInfo { ename: name.to_owned(), evalue: message.chars().take(500).collect() })
}

/// The error a failed batch reported on stderr, if recognizable.
pub(crate) fn error_info(language: BatchLanguage, stderr: &str) -> Option<RunErrorInfo> {
    match language {
        BatchLanguage::Python => python_error(stderr),
        BatchLanguage::R => r_error(stderr),
        BatchLanguage::Julia => julia_error(stderr),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_errors_are_recognised() {
        let stderr = "Traceback (most recent call last):\n  File \"x\", line 1\nModuleNotFoundError: No module named 'helpers'\n";
        let e = error_info(BatchLanguage::Python, stderr).unwrap();
        assert_eq!(e.ename, "ModuleNotFoundError");
        assert_eq!(e.evalue, "No module named 'helpers'");
        assert!(error_info(BatchLanguage::Python, "plain output\n").is_none());
        assert!(error_info(BatchLanguage::Python, "").is_none());
    }

    #[test]
    fn r_errors_are_recognised() {
        let e = error_info(
            BatchLanguage::R,
            "Error in library(ggplot2) : there is no package called 'ggplot2'\nExecution halted\n",
        )
        .unwrap();
        assert_eq!((e.ename.as_str(), e.evalue.as_str()), ("Error", "there is no package called 'ggplot2'"));
        let e = error_info(BatchLanguage::R, "Error: object 'x' not found\nExecution halted\n").unwrap();
        assert_eq!(e.evalue, "object 'x' not found");
        let e =
            error_info(BatchLanguage::R, "Error in file(file, \"rt\") : \n  cannot open the connection\nCalls: x\n")
                .unwrap();
        assert_eq!(e.evalue, "cannot open the connection");
        assert!(error_info(BatchLanguage::R, "Warning message:\nsomething\n").is_none());
    }

    #[test]
    fn julia_errors_are_recognised() {
        let stderr = "ERROR: LoadError: ArgumentError: Package CSV not found in current path.\nStacktrace:\n [1] top\n";
        let e = error_info(BatchLanguage::Julia, stderr).unwrap();
        assert_eq!((e.ename.as_str(), e.evalue.as_str()), ("ArgumentError", "Package CSV not found in current path."));
        let e = error_info(BatchLanguage::Julia, "ERROR: LoadError: boom\n").unwrap();
        assert_eq!((e.ename.as_str(), e.evalue.as_str()), ("Error", "boom"));
        assert!(error_info(BatchLanguage::Julia, "WARNING: x\n").is_none());
    }

    #[test]
    fn every_language_has_a_distinct_entry_and_image() {
        for language in BatchLanguage::ALL {
            assert!(language.is_script(inline_entry(language)), "{language:?}");
            for other in BatchLanguage::ALL.into_iter().filter(|o| *o != language) {
                assert!(!other.is_script(inline_entry(language)), "{other:?} runs {language:?}'s entry");
                assert_ne!(restricted_language(language), restricted_language(other));
            }
        }
        assert!(BatchLanguage::R.is_script("analysis.R") && BatchLanguage::R.is_script("a.r"));
        assert!(!BatchLanguage::R.is_script(".R") && !BatchLanguage::R.is_script("analysis.py"));
        assert!(BatchLanguage::Julia.is_script("model.JL") && !BatchLanguage::Julia.is_script("model.jl.txt"));
        assert_eq!(arguments(BatchLanguage::R, "a.R".into()), ["--vanilla", "a.R"]);
        assert_eq!(arguments(BatchLanguage::Python, "a.py".into()), ["a.py"]);
        assert!(container_environment(BatchLanguage::Julia)["PATH"].starts_with("/usr/local/julia/bin:"));
    }
}
