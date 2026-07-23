//! The intermediate representation (IR) shared by every parser and consumed by
//! the renderer.
//!
//! Both the xml2rfc-v3 parser and the plain-text fallback parser produce a
//! [`Document`]. The renderer only ever sees this model, so output quality is
//! defined here — most importantly the distinction between reflowable
//! [`Block::Paragraph`] text and verbatim [`Block::Artwork`] / [`Block::Code`]
//! that must stay monospaced and must never be reflowed on a small screen.

/// A fully parsed RFC, ready to render.
#[derive(Debug, Clone, Default)]
pub struct Document {
    /// RFC number, e.g. `9110`. `None` for a document parsed from an arbitrary
    /// source that carried no number.
    pub number: Option<u32>,
    pub title: String,
    /// Short title used for running headers, if the document provides one.
    pub short_title: Option<String>,
    pub authors: Vec<Author>,
    /// Publication date as a human string, e.g. `"June 2022"`.
    pub date: Option<String>,
    /// Stream / category line, e.g. `"Internet Standard"` or `"Informational"`.
    pub category: Option<String>,
    /// RFCs this one obsoletes.
    pub obsoletes: Vec<u32>,
    /// RFCs this one updates.
    pub updates: Vec<u32>,
    pub abstract_: Vec<Block>,
    /// Index keywords (from xml2rfc `<keyword>`), used for `<dc:subject>`.
    pub keywords: Vec<String>,
    pub sections: Vec<Section>,
    /// How this document was obtained/parsed, for provenance in the colophon.
    pub source: SourceKind,
}

/// Which input format produced a [`Document`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceKind {
    /// Parsed from canonical xml2rfc v3.
    Xml,
    /// Reconstructed from the published plain-text rendering.
    Text,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default)]
pub struct Author {
    pub name: String,
    pub organization: Option<String>,
}

/// How ASCII-art / code diagrams are rendered with respect to the reader theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SvgMode {
    /// Inline SVG that paints text with `currentColor`, so diagrams follow the
    /// reader's light/dark theme. The default: it looks best in practice,
    /// including on Kindle. Caveat: inline SVG is not strictly EPUB3-conformant
    /// without a manifest `svg` property (which `epub-builder` cannot emit), so
    /// a strict `epubcheck` will complain even though readers render it fine.
    #[default]
    Inline,
    /// Referenced SVG images that draw their own light "card" behind dark text.
    /// Fully EPUB3-conformant and self-contained, but does not follow the
    /// reader's dark theme (the card stays light). Use this if a particular
    /// reader mishandles inline SVG or you need `epubcheck`-clean output.
    Card,
}

/// A (possibly nested) document section. Appendices are just sections whose
/// [`Section::number`] is an appendix label like `"A"`.
#[derive(Debug, Clone, Default)]
pub struct Section {
    /// Section number/label as displayed, e.g. `"3.2"` or `"A"`. `None` for
    /// unnumbered sections (abstract-like or boilerplate).
    pub number: Option<String>,
    pub title: String,
    /// Stable anchor id used for cross-references and the nav TOC.
    pub id: String,
    pub blocks: Vec<Block>,
    pub subsections: Vec<Section>,
}

/// A block-level element.
#[derive(Debug, Clone)]
pub enum Block {
    /// Reflowable prose.
    Paragraph(Vec<Inline>),
    /// ASCII art, packet diagrams, tables-as-art — verbatim, monospaced, never
    /// reflowed. Rendered inside a horizontally scrollable `<pre>`.
    Artwork(String),
    /// Source code / formal syntax — verbatim monospace with an optional
    /// language hint.
    Code { text: String, language: Option<String> },
    /// A bullet or numbered list.
    List(List),
    /// A definition list (term/description pairs), e.g. xml2rfc `<dl>`.
    DefinitionList(Vec<(Vec<Inline>, Vec<Block>)>),
    /// A real data table (xml2rfc `<table>`), as opposed to art.
    Table(Table),
    /// An aside / note callout.
    Aside(Vec<Block>),
    /// A blockquote.
    Quote(Vec<Block>),
}

#[derive(Debug, Clone)]
pub struct List {
    pub ordered: bool,
    /// Each item is a sequence of blocks so lists can nest and hold paragraphs.
    pub items: Vec<Vec<Block>>,
}

#[derive(Debug, Clone, Default)]
pub struct Table {
    pub head: Vec<Vec<Inline>>,
    pub rows: Vec<Vec<Vec<Inline>>>,
}

/// Inline (phrasing) content.
#[derive(Debug, Clone)]
pub enum Inline {
    Text(String),
    /// Emphasised text (`<em>`).
    Emph(Vec<Inline>),
    /// Strong text (`<strong>`) — also used for BCP 14 keywords like MUST.
    Strong(Vec<Inline>),
    /// Inline monospace (`<tt>` / `<sourcecode>` inline).
    Code(String),
    /// A hyperlink to an external URI (`<eref>`).
    Link { text: Vec<Inline>, href: String },
    /// An internal cross-reference (`<xref>`); resolved to an in-book anchor
    /// when the target is local, otherwise shown as plain text.
    XRef { text: Vec<Inline>, target: String },
}

impl Inline {
    /// Convenience constructor for a plain text run.
    pub fn text(s: impl Into<String>) -> Self {
        Inline::Text(s.into())
    }
}
