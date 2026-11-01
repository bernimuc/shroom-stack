use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot access {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("cannot decode {path}: {message}")]
    Decode { path: PathBuf, message: String },
    #[error("unsupported image {path}: {message}")]
    Unsupported { path: PathBuf, message: String },
    #[error("cannot write {path}: {message}")]
    Encode { path: PathBuf, message: String },
}

pub type Result<T> = std::result::Result<T, Error>;
