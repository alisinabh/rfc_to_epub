//! # rfc2epub
//!
//! Convert IETF RFCs — and Markdown-based spec collections (Ethereum EIPs/ERCs,
//! Bitcoin BIPs, CAIPs) — into clean, reflowable EPUB files for e-readers.
//!
//! The pipeline is: **fetch** a source (per collection) → **parse** it into the
//! shared [`model::Document`] IR → optionally **resolve** image assets →
//! **render** that IR to XHTML → **assemble** an EPUB. A single IR produced by
//! several parsers and consumed by one renderer keeps output quality uniform.

pub mod assets;
pub mod error;
pub mod fetch;
pub mod highlight;
pub mod mathml;
pub mod model;
pub mod parse;
pub mod render;

use std::path::{Path, PathBuf};

pub use error::{Error, Result};
pub use fetch::{DocSpec, SourcePref};
pub use model::Document;

/// Options controlling a conversion.
#[derive(Debug, Clone)]
pub struct Options {
    /// Which source format to prefer (RFC only).
    pub source: SourcePref,
    /// Directory for cached downloads. `None` disables caching.
    pub cache_dir: Option<PathBuf>,
    /// How diagrams are rendered with respect to the reader's theme.
    pub svg_mode: model::SvgMode,
    /// Reproduce the source document's original pagination as EPUB page breaks.
    /// Only affects plain-text sources (xml2rfc/Markdown have no page concept).
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

/// Fetch and convert the document identified by `spec`, writing an EPUB to
/// `output`.
pub fn convert(spec: DocSpec, output: &Path, opts: &Options) -> Result<()> {
    let fetched = fetch::fetch(spec, opts.source, opts.cache_dir.as_deref())?;
    let mut doc = match fetched.kind {
        model::SourceKind::Markdown => {
            parse::parse_markdown(&fetched.body, fetched.collection, Some(fetched.number))?
        }
        kind => parse::parse(&fetched.body, kind, Some(fetched.number))?,
    };
    // Markdown documents may reference images; download and embed them.
    if fetched.kind == model::SourceKind::Markdown {
        if let Some(base) = &fetched.asset_base {
            assets::resolve(&mut doc, base, opts.cache_dir.as_deref(), fetched.collection);
        }
    }
    let bytes = render::to_epub(&doc, opts.svg_mode, opts.page_breaks)?;
    std::fs::write(output, bytes).map_err(|source| Error::Io {
        path: output.to_path_buf(),
        source,
    })
}

/// Fetch RFC `number` and write an EPUB to `output` (a thin wrapper over
/// [`convert`]).
pub fn convert_rfc(number: u32, output: &Path, opts: &Options) -> Result<()> {
    convert(DocSpec::new(model::Collection::Rfc, number), output, opts)
}

/// Parse an already-loaded source string into a [`Document`] without touching
/// the network. `kind` selects the parser; Markdown infers its collection from
/// the preamble.
pub fn parse_source(
    body: &str,
    kind: model::SourceKind,
    number: Option<u32>,
) -> Result<Document> {
    parse::parse(body, kind, number)
}
