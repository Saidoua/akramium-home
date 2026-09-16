use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found")]
    NotFound,
    #[error("sign in first")]
    Unauthorized,
    #[error("not allowed")]
    Forbidden,
    #[error("gone")]
    Gone,
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    #[error("too many attempts, wait {0} seconds")]
    TooMany(u64),
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn status(&self) -> StatusCode {
        match self {
            Error::NotFound => StatusCode::NOT_FOUND,
            Error::Unauthorized => StatusCode::UNAUTHORIZED,
            Error::Forbidden => StatusCode::FORBIDDEN,
            Error::Gone => StatusCode::GONE,
            Error::BadRequest(_) => StatusCode::BAD_REQUEST,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::TooMany(_) => StatusCode::TOO_MANY_REQUESTS,
            Error::Db(_) | Error::Io(_) | Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status = self.status();
        // Internal details stay in the log; the client sees a plain sentence.
        let message = match &self {
            Error::Db(_) | Error::Io(_) | Error::Internal(_) => {
                tracing::error!(error = %self, "request failed");
                "something went wrong on the server".to_string()
            }
            other => other.to_string(),
        };
        let body = serde_json::json!({ "error": message });
        let mut response = (status, axum::Json(body)).into_response();
        if let Error::TooMany(seconds) = self {
            response.headers_mut().insert("retry-after", seconds.to_string().parse().unwrap());
        }
        response
    }
}

impl From<tokio::task::JoinError> for Error {
    fn from(e: tokio::task::JoinError) -> Self {
        Error::Internal(format!("task failed: {e}"))
    }
}
