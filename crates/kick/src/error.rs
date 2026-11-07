/// Why a Kick request failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// No answer: no network, DNS, timeout.
    #[error("could not reach Kick: {0}")]
    Network(String),
    /// Kick refused the request (403), most likely its bot protection. Retrying soon will not
    /// help.
    #[error("Kick refused the request")]
    Blocked,
    /// Too many requests (429).
    #[error("Kick is rate limiting requests")]
    RateLimited,
    /// Any other error status.
    #[error("Kick answered {0}")]
    Status(u16),
    /// The answer was not the JSON expected: Kick changed something.
    #[error("Kick's answer could not be read: {0}")]
    Decode(String),
}

pub type Result<T> = std::result::Result<T, Error>;
