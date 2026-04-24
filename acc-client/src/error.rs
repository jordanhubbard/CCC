use acc_model::ApiError;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by `acc-client`.
///
/// Variants for known server semantics (`Conflict`, `Locked`, `NotFound`,
/// `Unauthorized`, `AtCapacity`) let callers pattern-match without parsing
/// the `ApiError` body. Everything else is lumped into `Api { status, body }`.
#[derive(Debug, Error)]
pub enum Error {
    #[error("HTTP {status}: {body:?}")]
    Api { status: u16, body: ApiError },

    #[error("resource conflict: {0:?}")]
    Conflict(ApiError),

    #[error("resource locked: {0:?}")]
    Locked(ApiError),

    #[error("not found: {0:?}")]
    NotFound(ApiError),

    #[error("unauthorized: {0:?}")]
    Unauthorized(ApiError),

    #[error("agent at capacity: {0:?}")]
    AtCapacity(ApiError),

    #[error("transport error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("malformed JSON response: {0}")]
    MalformedJson(#[from] serde_json::Error),

    #[error("no API token found: set ACC_TOKEN, pass explicitly, or add to ~/.acc/.env")]
    NoToken,

    #[error("invalid token characters")]
    InvalidToken,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Extract the HTTP status code for errors that carry one. Transport,
    /// JSON, auth, and I/O errors return `None`.
    pub fn status_code(&self) -> Option<u16> {
        match self {
            Error::Api { status, .. } => Some(*status),
            Error::Unauthorized(_) => Some(401),
            Error::NotFound(_) => Some(404),
            Error::Conflict(_) => Some(409),
            Error::Locked(_) => Some(423),
            Error::AtCapacity(_) => Some(429),
            Error::Http(_)
            | Error::MalformedJson(_)
            | Error::NoToken
            | Error::InvalidToken
            | Error::Io(_) => None,
        }
    }

    /// Build a typed error from an HTTP response body. Called by per-endpoint
    /// code after a non-2xx status.
    pub(crate) fn from_response(status: u16, body_bytes: &[u8]) -> Self {
        let body: ApiError = serde_json::from_slice(body_bytes).unwrap_or_else(|_| ApiError {
            error: format!("http_{status}"),
            message: Some(String::from_utf8_lossy(body_bytes).into_owned()),
            extra: Default::default(),
        });
        match status {
            401 => Error::Unauthorized(body),
            404 => Error::NotFound(body),
            409 => Error::Conflict(body),
            423 => Error::Locked(body),
            429 => Error::AtCapacity(body),
            _ => Error::Api { status, body },
        }
    }
}
