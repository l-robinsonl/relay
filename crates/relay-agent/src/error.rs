//! What can go wrong, in terms the interface can act on.

/// The result of an agent operation.
pub type AgentResult<T> = Result<T, AgentError>;

/// A failure in the assistant.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// No key is configured for the chosen provider.
    #[error("no API key is set for {provider}")]
    NoApiKey {
        /// Which provider wanted one.
        provider: String,
    },

    /// The provider refused the credentials.
    #[error("{provider} rejected the API key")]
    BadApiKey {
        /// Which provider refused.
        provider: String,
    },

    /// The provider asked us to slow down.
    #[error("{provider} is rate limiting this key{}", retry_after.map(|s| format!("; retry in {s}s")).unwrap_or_default())]
    RateLimited {
        /// Which provider.
        provider: String,
        /// Seconds to wait, when the provider said.
        retry_after: Option<u64>,
    },

    /// The provider could not be reached.
    #[error("could not reach {provider}: {detail}")]
    Transport {
        /// Which provider.
        provider: String,
        /// What the HTTP layer said.
        detail: String,
    },

    /// The provider answered, but not with something we understand.
    #[error("{provider} returned {status}: {detail}")]
    Provider {
        /// Which provider.
        provider: String,
        /// The HTTP status.
        status: u16,
        /// Whatever it said, for the log.
        detail: String,
    },

    /// A tool was asked for that does not exist.
    #[error("there is no tool called '{0}'")]
    UnknownTool(String),

    /// A tool's arguments did not match its schema.
    #[error("{tool} was called with arguments it cannot use: {detail}")]
    BadToolInput {
        /// Which tool.
        tool: String,
        /// What was wrong.
        detail: String,
    },

    /// The user declined to let a tool run.
    #[error("you declined to run {0}")]
    Declined(String),

    /// The loop hit its ceiling without the model finishing.
    #[error("the assistant took more than {0} steps without finishing")]
    TooManySteps(usize),

    /// Anything else.
    #[error("{0}")]
    Internal(String),
}

impl AgentError {
    /// Whether trying the same thing again might work.
    ///
    /// The interface offers a retry button on exactly these.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } | Self::Transport { .. } => true,
            // 5xx is the provider's problem and usually passes; 4xx is
            // ours and will not.
            Self::Provider { status, .. } => *status >= 500,
            _ => false,
        }
    }
}
