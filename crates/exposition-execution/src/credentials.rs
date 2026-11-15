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
//!    application's own environment, secrets the application registers while
//!    it runs (the stored provider API key, whatever its shape), plus
//!    well-known token shapes (`sk-ant-...`, `ghp_...`, `github_pat_...`, AWS
//!    key ids, ...). A host value that cannot be a secret is not a literal
//!    ([`host_value_is_secret`]): a path, a profile/region/session-id/location
//!    setting, or a short value under a name that matched only a provider
//!    prefix (`AWS_PROFILE=research` must not make every path containing
//!    "research" look secret). Token shapes are matched whatever the name.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use regex::Regex;

/// Replacement text for redacted secrets.
pub const REDACTED: &str = "[REDACTED]";

/// Literal secret values shorter than this are not redacted by value: a
/// two-character "secret" would otherwise shred ordinary output.
pub const MIN_LITERAL_SECRET_LEN: usize = 8;

/// Values of credential-like host variables that matched only a provider
/// prefix (configuration such as `OPENAI_ORG`) are literal secrets only from
/// this length; names that say "secret" (`*_TOKEN`, `*_PASSWORD`, `*_API_KEY`,
/// ...) use [`MIN_LITERAL_SECRET_LEN`].
pub const MIN_PREFIX_ONLY_SECRET_LEN: usize = 16;

/// Name endings whose values are locations, identifiers, or settings, not
/// secrets (the variables themselves are still never passed to children).
const NON_SECRET_NAME_SUFFIXES: &[&str] = &[
    "_PROFILE",
    "_REGION",
    "_CONFIG_DIR",
    "_CONFIG_FILE",
    "_SESSION_ID",
    "_HOME",
    "_PATH",
    "_DIR",
    "_FILE",
    "_VERSION",
];

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
    names_a_secret(&upper) || CREDENTIAL_PREFIXES.iter().any(|p| upper.starts_with(p))
}

/// The (upper-case) name says it holds a secret, not just that it belongs
/// to a provider.
fn names_a_secret(upper: &str) -> bool {
    if upper.contains("PASSWORD") || upper.contains("PASSWD") {
        return true;
    }
    if CREDENTIAL_NAMES.contains(&upper) {
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

/// A value that is a filesystem location rather than a secret: a Windows
/// drive or UNC path, anything with a backslash, or an absolute or `~/`
/// Unix path made only of path characters. (A value merely containing `/`
/// can still be a secret: base64 keys such as AWS secret access keys do.)
fn looks_like_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    let drive =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/');
    if drive || value.starts_with("\\\\") || value.contains('\\') {
        return true;
    }
    (value.starts_with('/') || value.starts_with("~/"))
        && value.matches('/').count() >= 2
        && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ' ' | '~'))
}

/// Whether the value of host variable `name` is a literal secret: the name
/// looks like a credential, the name does not describe a location or
/// setting (`*_PROFILE`, `*_REGION`, `*_CONFIG_DIR`, `*_SESSION_ID`,
/// `*_HOME`, `*_PATH`, ...), the value is not a path, and it is long enough
/// ([`MIN_LITERAL_SECRET_LEN`] for names that say "secret",
/// [`MIN_PREFIX_ONLY_SECRET_LEN`] for names that only carry a provider
/// prefix).
pub fn host_value_is_secret(name: &str, value: &str) -> bool {
    if !is_credential_name(name) {
        return false;
    }
    let upper = name.to_ascii_uppercase();
    if NON_SECRET_NAME_SUFFIXES.iter().any(|suffix| upper.ends_with(suffix)) {
        return false;
    }
    let value = value.trim();
    if looks_like_path(value) {
        return false;
    }
    let min = if names_a_secret(&upper) { MIN_LITERAL_SECRET_LEN } else { MIN_PREFIX_ONLY_SECRET_LEN };
    value.len() >= min
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
pub struct Redactor {
    /// Literal secrets from the host environment, with the variable they
    /// came from (empty when unnamed).
    base: Vec<(String, String)>,
    /// Secrets registered at run time, by name.
    registered: RwLock<BTreeMap<String, Vec<String>>>,
    /// Every literal secret (and each line of multi-line values), longest
    /// first so a value that contains another is replaced whole.
    literals: RwLock<Arc<Vec<String>>>,
    /// Where each literal came from (a host variable or a registered name).
    sources: RwLock<Arc<BTreeMap<String, String>>>,
    patterns: Vec<Regex>,
}

/// What made text secret ([`Redactor::find_secret`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretMatch {
    /// It contains the literal value of this host variable or registered
    /// secret (`None` when the literal has no name).
    Literal { source: Option<String> },
    /// It matches a well-known token shape.
    TokenShape,
}

/// Never prints the secrets themselves.
impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redactor")
            .field("literals", &self.literal_count())
            .field("patterns", &self.patterns.len())
            .finish()
    }
}

/// The literal forms of a secret value: trimmed, and each line of a
/// multi-line value (output is redacted line by line).
fn literal_forms(value: &str) -> Vec<String> {
    let mut forms = Vec::new();
    let trimmed = value.trim();
    if trimmed.len() >= MIN_LITERAL_SECRET_LEN {
        forms.push(trimmed.to_owned());
    }
    if value.contains('\n') {
        for line in value.lines().map(str::trim) {
            if line.len() >= MIN_LITERAL_SECRET_LEN {
                forms.push(line.to_owned());
            }
        }
    }
    forms
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
        let secrets = vars
            .into_iter()
            .filter(|(k, v)| host_value_is_secret(k.as_ref(), v.as_ref()))
            .map(|(k, v)| (k.as_ref().to_owned(), v.as_ref().to_owned()));
        Self::with_named_literals(secrets)
    }

    /// Literal secrets without names.
    pub fn with_literals<I: IntoIterator<Item = String>>(values: I) -> Self {
        Self::with_named_literals(values.into_iter().map(|v| (String::new(), v)))
    }

    /// Literal secrets with the variable each came from (for refusal messages).
    pub fn with_named_literals<I: IntoIterator<Item = (String, String)>>(values: I) -> Self {
        // Multi-line secrets (PEM keys, JSON service accounts) are also
        // matched per line.
        let base: Vec<(String, String)> = values
            .into_iter()
            .flat_map(|(name, v)| literal_forms(&v).into_iter().map(move |f| (name.clone(), f)))
            .collect();
        let patterns = TOKEN_PATTERNS.iter().map(|p| Regex::new(p).expect("token pattern compiles")).collect();
        let redactor = Self {
            base,
            registered: RwLock::new(BTreeMap::new()),
            literals: RwLock::new(Arc::new(Vec::new())),
            sources: RwLock::new(Arc::new(BTreeMap::new())),
            patterns,
        };
        redactor.rebuild();
        redactor
    }

    fn rebuild(&self) {
        let registered = self.registered.read().unwrap_or_else(|p| p.into_inner());
        let mut sources: BTreeMap<String, String> = BTreeMap::new();
        for (name, literal) in &self.base {
            sources.entry(literal.clone()).or_insert_with(|| name.clone());
        }
        for (name, forms) in registered.iter() {
            for literal in forms {
                sources.entry(literal.clone()).or_insert_with(|| name.clone());
            }
        }
        let set: BTreeSet<&String> = sources.keys().collect();
        let mut literals: Vec<String> = set.into_iter().cloned().collect();
        literals.sort_by_key(|s| std::cmp::Reverse(s.len()));
        *self.literals.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(literals);
        *self.sources.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(sources);
    }

    /// Register (`Some`) or forget (`None`) a secret the application holds at
    /// run time, under a name of its choosing. Its value is redacted from all
    /// output from now on, whatever its shape.
    pub fn register(&self, name: &str, value: Option<&str>) {
        {
            let mut registered = self.registered.write().unwrap_or_else(|p| p.into_inner());
            match value.map(literal_forms) {
                Some(forms) if !forms.is_empty() => {
                    registered.insert(name.to_owned(), forms);
                }
                _ => {
                    registered.remove(name);
                }
            }
        }
        self.rebuild();
    }

    fn current_literals(&self) -> Arc<Vec<String>> {
        Arc::clone(&self.literals.read().unwrap_or_else(|p| p.into_inner()))
    }

    /// Number of literal secrets known (for diagnostics; never the values).
    pub fn literal_count(&self) -> usize {
        self.current_literals().len()
    }

    /// True when the text matches a token shape or contains a known secret.
    pub fn contains_secret(&self, text: &str) -> bool {
        self.find_secret(text).is_some()
    }

    /// Why the text is secret, if it is: the variable (or registered name)
    /// whose value it contains, or a token shape. Never the value itself.
    pub fn find_secret(&self, text: &str) -> Option<SecretMatch> {
        if let Some(literal) = self.current_literals().iter().find(|l| text.contains(l.as_str())) {
            let sources = Arc::clone(&self.sources.read().unwrap_or_else(|p| p.into_inner()));
            let source = sources.get(literal).filter(|name| !name.is_empty()).cloned();
            return Some(SecretMatch::Literal { source });
        }
        self.patterns.iter().any(|p| p.is_match(text)).then_some(SecretMatch::TokenShape)
    }

    /// Replace every secret occurrence with [`REDACTED`].
    pub fn redact(&self, text: &str) -> String {
        replace_ranges(text, &self.secret_ranges(text))
    }

    /// Byte ranges of every secret occurrence (literal values and token
    /// shapes), sorted and merged. Range ends are char boundaries.
    pub fn secret_ranges(&self, text: &str) -> Vec<(usize, usize)> {
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for literal in self.current_literals().iter() {
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
        for literal in self.current_literals().iter() {
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
    fn registered_secrets_are_redacted_whatever_their_shape_until_forgotten() {
        let r = Redactor::from_host_env([("ANTHROPIC_API_KEY", "host-env-secret-value-1")]);
        let key = "Zq9plainKeyNoKnownShape7f3a2b1c";
        assert_eq!(r.redact(&format!("k={key}")), format!("k={key}"), "unknown shapes are not caught by pattern");
        r.register("provider", Some(key));
        assert_eq!(r.redact(&format!("k={key} h=host-env-secret-value-1")), "k=[REDACTED] h=[REDACTED]");
        assert!(r.contains_secret(key));
        // A split secret is held back until it can be redacted whole.
        assert_eq!(r.safe_release_len("out Zq9plainKey"), 4);
        assert!(!format!("{r:?}").contains(key), "Debug never shows secrets");
        r.register("provider", Some("another-plain-key-value-99"));
        assert_eq!(r.redact(key), key, "a replaced value is no longer a secret");
        r.register("provider", None);
        assert_eq!(r.redact("another-plain-key-value-99"), "another-plain-key-value-99");
        assert_eq!(r.redact("host-env-secret-value-1"), "[REDACTED]", "host secrets stay");
    }

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

    /// R-a1 F6: values that cannot be secrets are not literals; real ones are.
    #[test]
    fn only_host_values_that_can_be_secrets_are_literals() {
        let not_secrets = [
            ("AWS_PROFILE", "research"),
            ("AWS_DEFAULT_REGION", "eu-central-1"),
            ("AZURE_CONFIG_DIR", r"C:\Users\u"),
            ("CLAUDE_CODE_SESSION_ID", "5efb95c5-e1e2-456f-a33c-4f8b09ff0d72"),
            ("GOOGLE_APPLICATION_CREDENTIALS", r"C:\Users\u\keys\sa.json"),
            ("KUBECONFIG", "/home/u/.kube/config"),
            ("SSH_AUTH_SOCK", "/tmp/ssh-XXXX/agent.123"),
            ("AWS_SHARED_CREDENTIALS_FILE", "~/.aws/credentials"),
            ("OPENAI_ORG", "org-research"),
            ("HF_HOME", r"D:\models\hf"),
            ("ANTHROPIC_API_KEY_PATH", r"\\server\share\key.txt"),
        ];
        for (name, value) in not_secrets {
            assert!(!host_value_is_secret(name, value), "{name}={value} is not a secret");
        }
        let secrets = [
            ("ANTHROPIC_API_KEY", "fake-anthropic-value-123456"),
            ("DB_PASSWORD", "hunter2pw"),
            ("AWS_SECRET_ACCESS_KEY", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"),
            ("AWS_SESSION_TOKEN", "IQoJb3JpZ2luX2VjEXAMPLETOKEN"),
            ("CLAUDE_CODE_OAUTH_TOKEN", "0123456789abcdef0123"),
            ("GITHUB_TOKEN", "ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
            (
                "SERVICE_PRIVATE_KEY",
                "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n-----END PRIVATE KEY-----",
            ),
            ("DATABASE_URL", "postgres://user:pw@db.example/x"),
            ("AZURE_OPENAI_ENDPOINT", "https://example.openai.azure.com/"),
        ];
        for (name, value) in secrets {
            assert!(host_value_is_secret(name, value), "{name} keeps its value secret");
        }
        let r = Redactor::from_host_env(not_secrets.iter().chain(secrets.iter()).copied());
        assert_eq!(
            r.redact("profile research in C:\\Users\\u eu-central-1"),
            "profile research in C:\\Users\\u eu-central-1"
        );
        assert_eq!(r.redact("k=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"), "k=[REDACTED]");
        assert_eq!(r.find_secret("x hunter2pw y"), Some(SecretMatch::Literal { source: Some("DB_PASSWORD".into()) }));
        assert_eq!(r.find_secret("token sk-ant-api03-AbCdEf0123456789_xyz"), Some(SecretMatch::TokenShape));
        assert_eq!(r.find_secret("C:\\Users\\u\\research"), None);
        r.register("provider-api-key", Some("Zq9plainKeyNoKnownShape7f3a2b1c"));
        assert_eq!(
            r.find_secret("a Zq9plainKeyNoKnownShape7f3a2b1c b"),
            Some(SecretMatch::Literal { source: Some("provider-api-key".into()) })
        );
        assert_eq!(
            Redactor::with_literals(["unnamed-literal-1".into()]).find_secret("unnamed-literal-1"),
            Some(SecretMatch::Literal { source: None })
        );
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
