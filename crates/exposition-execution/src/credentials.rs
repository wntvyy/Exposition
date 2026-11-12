//! Credential detection and output redaction.
//!
//! Two independent rules keep secrets out of child processes and logs:
//!
//! 1. [`is_credential_name`]: environment variable names that look like
//!    credentials can never be inherited from the host or set on a job. A job
//!    that asks for one is refused, not silently filtered, so the caller learns
//!    about the mistake.
//! 2. [`Redactor`]: every byte of retained or emitted output passes through it.
//!    It replaces the literal values of credential-like variables found in the
//!    application's own environment, plus well-known token shapes
//!    (`sk-ant-...`, `ghp_...`, `github_pat_...`, AWS key ids, ...).

use std::collections::BTreeSet;

use regex::Regex;

/// Replacement text for redacted secrets.
pub const REDACTED: &str = "[REDACTED]";

/// Literal secret values shorter than this are not redacted by value: a
/// two-character "secret" would otherwise shred ordinary output.
pub const MIN_LITERAL_SECRET_LEN: usize = 8;

/// Name prefixes that always denote provider/cloud credentials or configuration.
const CREDENTIAL_PREFIXES: &[&str] = &[
    "ANTHROPIC_",
    "OPENAI_",
    "AWS_",
    "AZURE_",
    "CLAUDE_CODE_",
    "GEMINI_",
    "MISTRAL_",
    "COHERE_",
    "GROQ_",
    "DEEPSEEK_",
    "XAI_",
    "OPENROUTER_",
    "TOGETHER_",
    "REPLICATE_",
];

/// Exact names that carry credentials or point at credential material.
const CREDENTIAL_NAMES: &[&str] = &[
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "NPM_TOKEN",
    "HF_TOKEN",
    "SSH_AUTH_SOCK",
    "DATABASE_URL",
    "NETRC",
    "DOCKER_AUTH_CONFIG",
    "KUBECONFIG",
];

/// Name segments (split on `_`) whose suffix marks a credential, e.g.
/// `GITHUB_TOKEN`, `CLIENT_SECRET`, `MYAPIKEY`.
const CREDENTIAL_SEGMENT_SUFFIXES: &[&str] =
    &["TOKEN", "TOKENS", "SECRET", "SECRETS", "APIKEY", "PASSPHRASE", "CREDENTIAL", "CREDENTIALS", "COOKIE", "COOKIES"];

/// Adjacent segment pairs that mark a key, e.g. `OPENAI_API_KEY`, `SSH_PRIVATE_KEY`.
const CREDENTIAL_KEY_QUALIFIERS: &[&str] =
    &["API", "PRIVATE", "ACCESS", "SECRET", "AUTH", "SESSION", "SIGNING", "ENCRYPTION"];

/// True when an environment variable name looks like it carries a credential.
///
/// Matching is case-insensitive. The rules are deliberately broad: a false
/// positive refuses a job with a clear reason; a false negative leaks a secret.
pub fn is_credential_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if upper.contains("PASSWORD") || upper.contains("PASSWD") {
        return true;
    }
    if CREDENTIAL_NAMES.contains(&upper.as_str()) || CREDENTIAL_PREFIXES.iter().any(|p| upper.starts_with(p)) {
        return true;
    }
    let segments: Vec<&str> = upper.split('_').filter(|s| !s.is_empty()).collect();
    for (index, segment) in segments.iter().enumerate() {
        if CREDENTIAL_SEGMENT_SUFFIXES.iter().any(|suffix| segment.ends_with(suffix)) {
            return true;
        }
        if *segment == "PAT" || *segment == "AUTH" || segment.ends_with("AUTHTOKEN") {
            return true;
        }
        if (*segment == "KEY" || *segment == "KEYS")
            && index > 0
            && CREDENTIAL_KEY_QUALIFIERS.contains(&segments[index - 1])
        {
            return true;
        }
    }
    false
}

/// Token shapes redacted wherever they appear, independent of the environment.
const TOKEN_PATTERNS: &[&str] = &[
    // Anthropic, OpenAI (incl. sk-proj-), and other `sk-` style keys.
    r"sk-ant-[A-Za-z0-9_\-]{8,}",
    r"sk-[A-Za-z0-9_\-]{20,}",
    // GitHub personal/OAuth/app/refresh tokens and fine-grained PATs.
    r"gh[pousr]_[A-Za-z0-9]{20,}",
    r"github_pat_[A-Za-z0-9_]{20,}",
    // GitLab, npm, Hugging Face, Slack.
    r"glpat-[A-Za-z0-9_\-]{20,}",
    r"npm_[A-Za-z0-9]{30,}",
    r"hf_[A-Za-z0-9]{30,}",
    r"xox[abprs]-[A-Za-z0-9\-]{10,}",
    // AWS access key ids and Google API keys.
    r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
    r"AIza[0-9A-Za-z_\-]{35}",
    // HTTP bearer credentials.
    r"(?i)\bbearer\s+[A-Za-z0-9._~+/\-]{20,}=*",
    // PEM private key bodies (header through footer on one buffer).
    r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)",
];

/// Redacts secret values and token shapes from text.
#[derive(Debug)]
pub struct Redactor {
    /// Literal secret values (and each line of multi-line values), longest first
    /// so a value that contains another is replaced whole.
    literals: Vec<String>,
    patterns: Vec<Regex>,
}

impl Redactor {
    /// Build a redactor from the host environment: the values of every
    /// credential-like variable become literal secrets.
    pub fn from_host_env<I, K, V>(vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let secrets =
            vars.into_iter().filter(|(k, _)| is_credential_name(k.as_ref())).map(|(_, v)| v.as_ref().to_owned());
        Self::with_literals(secrets)
    }

    pub fn with_literals<I: IntoIterator<Item = String>>(values: I) -> Self {
        let mut set = BTreeSet::new();
        for value in values {
            let trimmed = value.trim();
            if trimmed.len() >= MIN_LITERAL_SECRET_LEN {
                set.insert(trimmed.to_owned());
            }
            // Output is redacted line by line, so multi-line secrets (PEM keys,
            // JSON service accounts) are also matched per line.
            if value.contains('\n') {
                for line in value.lines().map(str::trim) {
                    if line.len() >= MIN_LITERAL_SECRET_LEN {
                        set.insert(line.to_owned());
                    }
                }
            }
        }
        let mut literals: Vec<String> = set.into_iter().collect();
        literals.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let patterns = TOKEN_PATTERNS.iter().map(|p| Regex::new(p).expect("token pattern compiles")).collect();
        Self { literals, patterns }
    }

    /// Number of literal secrets known (for diagnostics; never the values).
    pub fn literal_count(&self) -> usize {
        self.literals.len()
    }

    /// True when the text matches a token shape or contains a known secret.
    pub fn contains_secret(&self, text: &str) -> bool {
        self.literals.iter().any(|l| text.contains(l.as_str())) || self.patterns.iter().any(|p| p.is_match(text))
    }

    /// Replace every secret occurrence with [`REDACTED`].
    pub fn redact(&self, text: &str) -> String {
        replace_ranges(text, &self.secret_ranges(text))
    }

    /// Byte ranges of every secret occurrence (literal values and token
    /// shapes), sorted and merged. Range ends are char boundaries.
    pub fn secret_ranges(&self, text: &str) -> Vec<(usize, usize)> {
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for literal in &self.literals {
            ranges.extend(text.match_indices(literal.as_str()).map(|(start, m)| (start, start + m.len())));
        }
        for pattern in &self.patterns {
            ranges.extend(pattern.find_iter(text).map(|m| (m.start(), m.end())));
        }
        merge_ranges(ranges)
    }

    /// Largest prefix length of `text` (a char boundary) that can be redacted
    /// and released now without splitting a secret that may continue in later
    /// output. Holds back a trailing token match or a suffix that begins a
    /// known literal secret.
    pub fn safe_release_len(&self, text: &str) -> usize {
        let mut cut = text.len();
        for pattern in &self.patterns {
            for m in pattern.find_iter(text) {
                if m.end() == text.len() {
                    cut = cut.min(m.start());
                }
            }
        }
        // A suffix of `text` (3+ bytes; shorter fragments reveal nothing useful)
        // that is a proper prefix of a literal secret.
        for literal in &self.literals {
            let max = literal.len().saturating_sub(1).min(text.len());
            for len in (3..=max).rev() {
                let start = text.len() - len;
                if text.is_char_boundary(start) && literal.starts_with(&text[start..]) {
                    cut = cut.min(start);
                    break;
                }
            }
        }
        // Token prefixes that are not yet long enough to match a pattern.
        for prefix in ["sk-", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "glpat-", "xox", "hf_", "npm_"] {
            if let Some(pos) = text.rfind(prefix) {
                let tail = &text[pos..];
                if tail.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                    cut = cut.min(pos);
                }
            }
        }
        cut
    }
}

/// Sort and merge overlapping or touching ranges.
pub fn merge_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// Replace each (sorted, disjoint, char-boundary) range with [`REDACTED`].
pub fn replace_ranges(text: &str, ranges: &[(usize, usize)]) -> String {
    if ranges.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for &(start, end) in ranges {
        out.push_str(&text[at..start]);
        out.push_str(REDACTED);
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

impl Default for Redactor {
    fn default() -> Self {
        Self::with_literals(std::iter::empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_names_are_detected() {
        for name in [
            "ANTHROPIC_API_KEY",
            "anthropic_base_url",
            "OPENAI_API_KEY",
            "OPENAI_ORG",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_PROFILE",
            "AZURE_OPENAI_ENDPOINT",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "GOOGLE_API_KEY",
            "GITHUB_TOKEN",
            "GH_TOKEN",
            "NPM_TOKEN",
            "HF_TOKEN",
            "MY_SERVICE_TOKEN",
            "CLIENT_SECRET",
            "DB_PASSWORD",
            "MYPASSWORDFILE",
            "PGPASSWD",
            "STRIPE_SECRET_KEY",
            "SSH_PRIVATE_KEY",
            "SSH_AUTH_SOCK",
            "GITHUB_PAT",
            "NGROK_AUTHTOKEN",
            "SLACK_BOT_TOKEN",
            "SESSION_COOKIE",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "API_KEY",
            "MYAPIKEY",
        ] {
            assert!(is_credential_name(name), "{name} should be treated as a credential");
        }
    }

    #[test]
    fn ordinary_names_are_not_credentials() {
        for name in [
            "PATH",
            "SystemRoot",
            "TEMP",
            "USERPROFILE",
            "APPDATA",
            "PYTHONPATH",
            "TOKENIZERS_PARALLELISM",
            "MPLBACKEND",
            "TEXINPUTS",
            "KEYBOARD_LAYOUT",
            "HF_HOME",
            "LANG",
            "OMP_NUM_THREADS",
            "XDG_DATA_PATH",
            "SOURCE_DATE_EPOCH",
        ] {
            assert!(!is_credential_name(name), "{name} should not be treated as a credential");
        }
    }

    #[test]
    fn redacts_literal_values_from_host_env() {
        let r = Redactor::from_host_env([
            ("ANTHROPIC_API_KEY", "fake-anthropic-value-123456"),
            ("SHORT_TOKEN", "abc"),
            ("PATH", "C:\\Windows"),
        ]);
        assert_eq!(r.literal_count(), 1);
        let out = r.redact("key=fake-anthropic-value-123456 path=C:\\Windows abc");
        assert_eq!(out, "key=[REDACTED] path=C:\\Windows abc");
    }

    #[test]
    fn redacts_token_shapes() {
        let r = Redactor::default();
        let cases = [
            "sk-ant-api03-AbCdEf0123456789_xyz",
            "sk-proj-abcdefghijklmnopqrstuvwxyz012345",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "github_pat_11ABCDEFG0123456789_abcdefghijklmnop",
            "AKIAIOSFODNN7EXAMPLE",
            "Authorization: Bearer abcdefghijklmnopqrstuvwxyz.0123",
            "xoxb-1234567890-abcdefghij",
            "glpat-abcdefghijklmnopqrstuv",
            "hf_abcdefghijklmnopqrstuvwxyzABCDEFGH",
        ];
        for case in cases {
            let out = r.redact(&format!("before {case} after"));
            assert!(out.contains(REDACTED), "{case} not redacted: {out}");
            assert!(out.starts_with("before "), "{out}");
            assert!(r.contains_secret(case), "{case}");
        }
        assert_eq!(r.redact("nothing secret: sk-short, ghp_x"), "nothing secret: sk-short, ghp_x");
    }

    #[test]
    fn redacts_multiline_secret_per_line() {
        let pem = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n-----END PRIVATE KEY-----";
        let r = Redactor::from_host_env([("SERVICE_PRIVATE_KEY", pem)]);
        assert_eq!(r.redact("line: MIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n"), "line: [REDACTED]\n");
        assert!(r.redact(pem).contains(REDACTED));
        assert!(!r.redact(pem).contains("MIIEvQ"));
    }

    #[test]
    fn safe_release_holds_back_partial_secrets() {
        let r = Redactor::from_host_env([("MY_TOKEN", "supersecretvalue")]);
        // Suffix that starts the literal secret is held back.
        let text = "progress 50% supersec";
        assert_eq!(&text[..r.safe_release_len(text)], "progress 50% ");
        // A token shape touching the end may continue.
        let text = "token sk-ant-abcdefghijk";
        assert_eq!(&text[..r.safe_release_len(text)], "token ");
        // An incomplete token prefix is held.
        let text = "value ghp_";
        assert_eq!(&text[..r.safe_release_len(text)], "value ");
        // Ordinary text is released whole.
        let text = "Running pass 3 of 4";
        assert_eq!(r.safe_release_len(text), text.len());
    }
}
