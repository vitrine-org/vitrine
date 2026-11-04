use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("invalid manifest: {0}")]
    Manifest(String),
    #[error("invalid settings schema: {0}")]
    Schema(String),
    #[error("unsafe or invalid archive: {0}")]
    Archive(String),
    #[error("{name} needs Vitrine {required} or later (this is {running})")]
    EngineTooOld {
        name: String,
        required: String,
        running: String,
    },
    #[error("not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, Error>;
