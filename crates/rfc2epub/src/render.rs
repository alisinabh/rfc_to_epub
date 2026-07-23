//! Rendering the [`Document`](crate::model::Document) IR to an EPUB.

mod cover;
mod css;
mod epub;
mod svg;
mod xhtml;

use crate::error::Result;
use crate::model::{Document, SvgMode};

/// Render `doc` to a complete EPUB file, returned as bytes.
pub fn to_epub(doc: &Document, svg_mode: SvgMode) -> Result<Vec<u8>> {
    epub::build(doc, svg_mode)
}
