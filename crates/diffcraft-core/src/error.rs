use std::path::{Path, PathBuf};

/// Errors from the comparison engine.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} is too large ({size} bytes, limit {limit})", path.display())]
    TooLarge { path: PathBuf, size: u64, limit: u64 },
    #[error("image {}: {message}", path.display())]
    Image { path: PathBuf, message: String },
    #[error("{0}")]
    InvalidInput(String),
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    pub fn io(path: &Path, source: std::io::Error) -> Self {
        Error::Io { path: path.to_path_buf(), source }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
