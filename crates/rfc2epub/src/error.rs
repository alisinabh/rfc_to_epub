//! Error type for the crate.

use std::path::PathBuf;

/// Errors produced while fetching, parsing, or rendering an RFC.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error fetching {url}: {source}")]
    Http {
        url: String,
        #[source]
        source: Box<ureq::Error>,
    },

    #[error("{0} was not found upstream")]
    NotFound(String),

    #[error("failed to read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not parse RFC XML: {0}")]
    Xml(#[from] roxmltree::Error),

    #[error("could not parse the document: {0}")]
    Parse(String),

    #[error("{0}")]
    Unsupported(String),

    #[error("failed to build EPUB: {0}")]
    Epub(String),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

impl From<epub_builder::Error> for Error {
    fn from(e: epub_builder::Error) -> Self {
        Error::Epub(e.to_string())
    }
}
