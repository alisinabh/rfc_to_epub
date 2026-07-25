//! Parsers that turn a source string into the [`Document`](crate::model::Document) IR.

pub mod markdown;
pub mod mediawiki;
pub mod preamble;
mod text;
mod xml;

use crate::error::Result;
use crate::model::{Collection, Document, SourceKind};

/// Parse `body` using the parser selected by `kind`. Markdown is parsed with no
/// collection hint (it is inferred from the preamble); use
/// [`parse_markdown`] when the collection is known. MediaWiki is only used by
/// Bitcoin BIPs, so it assumes [`Collection::Bip`].
pub fn parse(body: &str, kind: SourceKind, number: Option<u32>) -> Result<Document> {
    match kind {
        SourceKind::Xml => xml::parse(body, number),
        SourceKind::Markdown => markdown::parse(body, None, number),
        SourceKind::Mediawiki => mediawiki::parse(body, Some(Collection::Bip), number),
        SourceKind::Text | SourceKind::Unknown => text::parse(body, number),
    }
}

/// Parse a Markdown `body` with a known collection (from the requested id).
pub fn parse_markdown(body: &str, collection: Collection, number: Option<u32>) -> Result<Document> {
    markdown::parse(body, Some(collection), number)
}
