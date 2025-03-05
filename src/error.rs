use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Websocket error: {0}")]
    WebsocketError(String),

    #[error("Parsing error: {0}")]
    ParsingError(String),

    #[error("RPC error: {0}")]
    RpcError(String),

    #[error("API error: {0}")]
    ApiError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Environment variable error: {0}")]
    EnvVarError(#[from] std::env::VarError),

    #[error("General error: {0}")]
    General(String),
}

impl From<String> for Error {
    fn from(error: String) -> Self {
        Error::General(error)
    }
}

impl From<&str> for Error {
    fn from(error: &str) -> Self {
        Error::General(error.to_string())
    }
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Error::ApiError(error.to_string())
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for Error {
    fn from(error: tokio_tungstenite::tungstenite::Error) -> Self {
        Error::WebsocketError(error.to_string())
    }
}
