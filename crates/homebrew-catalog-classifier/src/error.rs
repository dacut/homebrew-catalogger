use std::path::PathBuf;

/// Errors produced while loading Homebrew metadata or classifying formulae.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A metadata file or directory could not be read.
    #[error("unable to read {path}: {source}")]
    Io {
        /// The path that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A metadata file did not contain valid JSON.
    #[error("invalid JSON in {path}: {source}")]
    Json {
        /// The file that failed to parse.
        path: PathBuf,
        /// The underlying JSON error.
        #[source]
        source: serde_json::Error,
    },

    /// A metadata file was valid JSON but not in a recognized Homebrew formula format.
    #[error("{path} is not a recognized Homebrew formula metadata file: {reason}")]
    UnrecognizedMetadata {
        /// The file that was not recognized.
        path: PathBuf,
        /// Why the file was rejected.
        reason: String,
    },

    /// The location of the Homebrew cache could not be determined for this platform.
    #[error("unable to determine the Homebrew cache directory; set HOMEBREW_CACHE or pass a path explicitly")]
    NoDefaultCacheDir,

    /// The requested formula was not present in the metadata.
    #[error("formula {0:?} not found in Homebrew metadata")]
    FormulaNotFound(String),

    /// No Anthropic API credentials were supplied or found in the environment.
    #[error("no Anthropic API credentials found; set ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN")]
    MissingCredentials,

    /// Amazon Bedrock could not be used: no region, no AWS credentials, a signing failure, or an unsupported option.
    #[error("Amazon Bedrock: {0}")]
    Bedrock(String),

    /// The HTTP request to the API failed before a response was received, or the
    /// response body could not be decoded.
    #[error("HTTP error calling the API: {0}")]
    Http(#[from] reqwest::Error),

    /// The API (the Claude API or Amazon Bedrock) returned an error response.
    #[error("API error (HTTP {status}, {kind}): {message}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// The API's error type, e.g. `rate_limit_error` or `invalid_request_error`.
        kind: String,
        /// The API's error message.
        message: String,
        /// The `request-id` header, if the API returned one.
        request_id: Option<String>,
    },

    /// The model declined to classify the formula.
    #[error("the model declined to classify the formula (category: {})", category.as_deref().unwrap_or("unspecified"))]
    Refusal {
        /// The refusal category reported by the API, if any.
        category: Option<String>,
        /// The explanation reported by the API, if any.
        explanation: Option<String>,
    },

    /// The model hit `max_tokens` before finishing its answer.
    #[error("the model's response was truncated by max_tokens ({0}); increase max_tokens")]
    Truncated(u32),

    /// The model's response could not be interpreted as a classification.
    #[error("unexpected response from the model: {0}")]
    InvalidResponse(String),
}

/// A `Result` alias using this crate's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;
