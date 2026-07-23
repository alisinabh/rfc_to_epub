//! Parsers that turn a source string into the [`Document`](crate::model::Document) IR.

mod text;
mod xml;

use crate::error::Result;
use crate::model::{Document, SourceKind};

/// Parse `body` using the parser selected by `kind`.
pub fn parse(body: &str, kind: SourceKind, number: Option<u32>) -> Result<Document> {
    match kind {
        SourceKind::Xml => xml::parse(body, number),
        _ => text::parse(body, number),
    }
}
