/// Errors that can occur during chat operations
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },

    #[error("SSE parse error: {0}")]
    Parse(String),
}
