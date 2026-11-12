use thiserror::Error;

/// Research failures. `code()` is stable and machine-readable; messages are safe
/// to show (they never contain environment values or secrets).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResearchError {
    #[error("no workspace is open for research")]
    NoWorkspace,
    #[error("{0}")]
    InvalidWorkspace(String),
    #[error(
        "this workspace is not trusted; research kernels run code with your account's privileges, so trust the workspace before starting a session"
    )]
    NotTrusted,
    #[error("{0}")]
    PolicyRefused(String),
    #[error("unknown research environment {0}; refresh the environment list")]
    UnknownEnvironment(String),
    #[error("{reason} {guidance}")]
    EnvironmentUnavailable { reason: String, guidance: String },
    #[error("unknown research session {0}")]
    UnknownSession(String),
    #[error("unknown run {0}")]
    UnknownRun(String),
    #[error("unknown artifact {0}")]
    UnknownArtifact(String),
    #[error("session {session} belongs to another owner; agent runs use their own sessions")]
    OwnerMismatch { session: String },
    #[error("session {session} is {state}; {hint}")]
    SessionNotReady { session: String, state: String, hint: String },
    #[error("{0}")]
    InvalidRequest(String),
    #[error("{0}")]
    StaleSource(String),
    #[error("failed to launch the research helper: {0}")]
    Launch(String),
    #[error("research helper error ({code}): {message}")]
    Helper { code: String, message: String },
    #[error("the research helper did not answer within {0} seconds")]
    Timeout(u64),
    #[error("the research helper is no longer running")]
    HelperGone,
    #[error("{0}")]
    Io(String),
}

impl ResearchError {
    pub fn code(&self) -> String {
        match self {
            ResearchError::NoWorkspace => "research.noWorkspace".into(),
            ResearchError::InvalidWorkspace(_) => "research.invalidWorkspace".into(),
            ResearchError::NotTrusted => "research.notTrusted".into(),
            ResearchError::PolicyRefused(_) => "research.policyRefused".into(),
            ResearchError::UnknownEnvironment(_) => "research.unknownEnvironment".into(),
            ResearchError::EnvironmentUnavailable { .. } => "research.environmentUnavailable".into(),
            ResearchError::UnknownSession(_) => "research.unknownSession".into(),
            ResearchError::UnknownRun(_) => "research.unknownRun".into(),
            ResearchError::UnknownArtifact(_) => "research.unknownArtifact".into(),
            ResearchError::OwnerMismatch { .. } => "research.ownerMismatch".into(),
            ResearchError::SessionNotReady { .. } => "research.sessionNotReady".into(),
            ResearchError::InvalidRequest(_) => "research.invalidRequest".into(),
            ResearchError::StaleSource(_) => "research.staleSource".into(),
            ResearchError::Launch(_) => "research.launchFailed".into(),
            ResearchError::Helper { code, .. } => format!("research.helper.{code}"),
            ResearchError::Timeout(_) => "research.timeout".into(),
            ResearchError::HelperGone => "research.helperGone".into(),
            ResearchError::Io(_) => "research.io".into(),
        }
    }

    pub(crate) fn io(context: &str, error: std::io::Error) -> Self {
        ResearchError::Io(format!("{context}: {error}"))
    }
}
