//! # rfc2epub
//!
//! Convert IETF RFCs into clean, reflowable EPUB files for e-readers.
//!
//! The pipeline is: **fetch** a source (XML v3 preferred, plain-text fallback)
//! → **parse** it into the shared [`model::Document`] IR → **render** that IR
//! to XHTML → **assemble** an EPUB.

pub mod error;
pub mod fetch;
pub mod model;
pub mod parse;
pub mod render;

use std::path::{Path, PathBuf};

pub use error::{Error, Result};
pub use fetch::SourcePref;
pub use model::Document;

/// Options controlling a conversion.
#[derive(Debug, Clone)]
pub struct Options {
    /// Which source format to prefer.
    pub source: SourcePref,
    /// Directory for cached downloads. `None` disables caching.
    pub cache_dir: Option<PathBuf>,
    /// How diagrams are rendered with respect to the reader's theme.
    pub svg_mode: model::SvgMode,
    /// Reproduce the source document's original pagination as EPUB page breaks.
    /// Only affects plain-text sources (xml2rfc has no page concept).
    pub page_breaks: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            source: SourcePref::Auto,
            cache_dir: fetch::default_cache_dir(),
            svg_mode: model::SvgMode::default(),
            page_breaks: true,
        }
    }
}

/// Fetch RFC `number` and write an EPUB to `output`.
pub fn convert_rfc(number: u32, output: &Path, opts: &Options) -> Result<()> {
    let fetched = fetch::fetch_rfc(number, opts.source, opts.cache_dir.as_deref())?;
    let doc = parse::parse(&fetched.body, fetched.kind, Some(number))?;
    let bytes = render::to_epub(&doc, opts.svg_mode, opts.page_breaks)?;
    std::fs::write(output, bytes).map_err(|source| Error::Io {
        path: output.to_path_buf(),
        source,
    })
}

/// Parse an already-loaded source string into a [`Document`] without touching
/// the network. `kind` selects the parser.
pub fn parse_source(
    body: &str,
    kind: model::SourceKind,
    number: Option<u32>,
) -> Result<Document> {
    parse::parse(body, kind, number)
}
