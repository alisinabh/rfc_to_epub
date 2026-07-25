//! The intermediate representation (IR) shared by every parser and consumed by
//! the renderer.
//!
//! Every parser — the xml2rfc-v3 parser, the plain-text fallback, and the
//! Markdown parser used for spec collections like EIPs/ERCs/BIPs — produces a
//! [`Document`]. The renderer only ever sees this model, so output quality is
//! defined here — most importantly the distinction between reflowable
//! [`Block::Paragraph`] text and verbatim [`Block::Artwork`] / [`Block::Code`]
//! that must stay monospaced and must never be reflowed on a small screen.
//!
//! The model is deliberately *format-neutral*: the only collection-specific
//! knowledge lives in [`Collection`] (how an id is spelled and where it lives on
//! the web). Everything else — sections, blocks, inlines — is shared.

/// A fully parsed document, ready to render.
#[derive(Debug, Clone, Default)]
pub struct Document {
    /// The document's identity, e.g. `DocId { collection: Rfc, number: 9110 }`
    /// (renders as `RFC 9110`) or `DocId { collection: Eip, number: 1559 }`
    /// (`EIP-1559`). `None` for a document parsed from an arbitrary source that
    /// carried no recognizable id.
    pub id: Option<DocId>,
    pub title: String,
    /// Short title used for running headers, if the document provides one.
    pub short_title: Option<String>,
    pub authors: Vec<Author>,
    /// Publication date as a human string, e.g. `"June 2022"`.
    pub date: Option<String>,
    /// Category / stream / status line, e.g. `"Internet Standard"`,
    /// `"Standards Track: Core"`, or `"Final"`. Generalizes the RFC `category`.
    pub status: Option<String>,
    /// Labeled relations to other documents, e.g. `("Obsoletes", [RFC 1234])`,
    /// `("Requires", [EIP-155])`, `("Replaces", [BIP 42])`. Generalizes the RFC
    /// obsoletes/updates lists.
    pub relations: Vec<Relation>,
    pub abstract_: Vec<Block>,
    /// Index keywords (from xml2rfc `<keyword>`), used for `<dc:subject>`.
    pub keywords: Vec<String>,
    /// Preamble fields that don't map onto a first-class slot (e.g. EIP
    /// `discussions-to`, `created`, `license`). Rendered as a metadata table on
    /// the title page, in insertion order.
    pub extra: Vec<(String, String)>,
    pub sections: Vec<Section>,
    /// Binary resources (images, rendered diagrams) referenced by the content
    /// and embedded into the EPUB. Populated by the fetch/asset layer.
    pub assets: Vec<Asset>,
    /// How this document was obtained/parsed, for provenance in the colophon.
    pub source: SourceKind,
}

impl Document {
    /// The document's numeric id, if any (e.g. `9110`). Convenience over
    /// [`Document::id`] for the common single-number case.
    pub fn number(&self) -> Option<u32> {
        self.id.map(|d| d.number)
    }

    /// The collection this document belongs to, if identified.
    pub fn collection(&self) -> Option<Collection> {
        self.id.map(|d| d.collection)
    }

    /// The display id, e.g. `"RFC 9110"` / `"EIP-1559"`, if identified.
    pub fn display_id(&self) -> Option<String> {
        self.id.map(|d| d.label())
    }

    /// The book title as shown in library shelves, e.g. `"RFC 9110: HTTP
    /// Semantics"`, falling back to the bare title when there is no id.
    pub fn book_title(&self) -> String {
        match &self.id {
            Some(id) => format!("{}: {}", id.label(), self.title),
            None => self.title.clone(),
        }
    }
}

/// A document's identity within a known [`Collection`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocId {
    pub collection: Collection,
    pub number: u32,
}

impl DocId {
    pub fn new(collection: Collection, number: u32) -> Self {
        Self { collection, number }
    }

    /// Display label, e.g. `"RFC 9110"` or `"EIP-1559"`.
    pub fn label(self) -> String {
        self.collection.label(self.number)
    }

    /// The canonical public URL for this document (used to resolve
    /// cross-document links, since an EPUB holds a single document).
    pub fn external_url(self) -> String {
        self.collection.external_url(self.number)
    }

    /// A stable identifier string for the EPUB `dc:identifier` (also the seed
    /// for the book UUID).
    pub fn urn(self) -> String {
        match self.collection {
            Collection::Rfc => format!("urn:ietf:rfc:{}", self.number),
            _ => self.external_url(),
        }
    }
}

/// The families of specification documents rfc2epub understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collection {
    /// IETF Request for Comments.
    Rfc,
    /// Ethereum Improvement Proposal.
    Eip,
    /// Ethereum Request for Comment (application-layer standards, split from
    /// EIPs in 2023).
    Erc,
    /// Bitcoin Improvement Proposal.
    Bip,
    /// Lightning Network BOLT.
    Bolt,
    /// Chain Agnostic Improvement Proposal.
    Caip,
}

impl Collection {
    /// The lowercase token used in ids and filenames (`"rfc"`, `"eip"`, …).
    pub fn token(self) -> &'static str {
        match self {
            Collection::Rfc => "rfc",
            Collection::Eip => "eip",
            Collection::Erc => "erc",
            Collection::Bip => "bip",
            Collection::Bolt => "bolt",
            Collection::Caip => "caip",
        }
    }

    /// Parse a collection token case-insensitively (`"EIP"`, `"erc"` → …).
    pub fn from_token(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "rfc" => Some(Collection::Rfc),
            "eip" => Some(Collection::Eip),
            "erc" => Some(Collection::Erc),
            "bip" => Some(Collection::Bip),
            "bolt" => Some(Collection::Bolt),
            "caip" => Some(Collection::Caip),
            _ => None,
        }
    }

    /// The display label for a number, e.g. `"RFC 9110"` (space) vs.
    /// `"EIP-1559"` (hyphen), following each collection's house style.
    pub fn label(self, n: u32) -> String {
        match self {
            Collection::Rfc => format!("RFC {n}"),
            Collection::Eip => format!("EIP-{n}"),
            Collection::Erc => format!("ERC-{n}"),
            Collection::Bip => format!("BIP {n}"),
            Collection::Bolt => format!("BOLT {n}"),
            Collection::Caip => format!("CAIP-{n}"),
        }
    }

    /// The canonical public URL for a document number in this collection.
    pub fn external_url(self, n: u32) -> String {
        match self {
            Collection::Rfc => format!("https://www.rfc-editor.org/rfc/rfc{n}.html"),
            // ERCs are served from the EIPs site under the `eip-` path too.
            Collection::Eip | Collection::Erc => {
                format!("https://eips.ethereum.org/EIPS/eip-{n}")
            }
            // Best-effort: the on-disk extension (.md/.mediawiki) is unknown here.
            Collection::Bip => {
                format!("https://github.com/bitcoin/bips/blob/master/bip-{n:04}.mediawiki")
            }
            Collection::Bolt => match bolt_filename(n) {
                Some(f) => format!("https://github.com/lightning/bolts/blob/master/{f}"),
                None => format!("https://github.com/lightning/bolts/blob/master/{n:02}.md"),
            },
            Collection::Caip => format!("https://chainagnostic.org/CAIPs/caip-{n}"),
        }
    }
}

/// The on-disk filename for a Lightning BOLT number in the `lightning/bolts`
/// repo. BOLT files embed a title in their name (`11-payment-encoding.md`), so a
/// number alone can't build the raw URL — this is the stable, hand-maintained
/// map (update it when new BOLTs land). `None` for an unknown number, including
/// BOLT 6, which does not exist (its content folded into other BOLTs).
pub(crate) fn bolt_filename(n: u32) -> Option<&'static str> {
    Some(match n {
        0 => "00-introduction.md",
        1 => "01-messaging.md",
        2 => "02-peer-protocol.md",
        3 => "03-transactions.md",
        4 => "04-onion-routing.md",
        5 => "05-onchain.md",
        7 => "07-routing-gossip.md",
        8 => "08-transport.md",
        9 => "09-features.md",
        10 => "10-dns-bootstrap.md",
        11 => "11-payment-encoding.md",
        12 => "12-offer-encoding.md",
        _ => return None,
    })
}

/// A labeled set of related documents (`"Obsoletes"`, `"Requires"`, …).
#[derive(Debug, Clone)]
pub struct Relation {
    pub label: String,
    pub targets: Vec<DocId>,
}

impl Relation {
    pub fn new(label: impl Into<String>, targets: Vec<DocId>) -> Self {
        Self {
            label: label.into(),
            targets,
        }
    }
}

/// A binary resource embedded in the EPUB (an image or a rendered diagram).
#[derive(Debug, Clone)]
pub struct Asset {
    /// Path within the EPUB package, e.g. `"assets/eip-1/foo.png"`. Referenced
    /// by [`Block::Figure`] / [`Inline::Image`] via their `resource` field.
    pub path: String,
    /// MIME type, e.g. `"image/png"`.
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// Which input format produced a [`Document`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceKind {
    /// Parsed from canonical xml2rfc v3.
    Xml,
    /// Reconstructed from the published plain-text rendering.
    Text,
    /// Parsed from Markdown (GitHub-flavored), used by spec collections.
    Markdown,
    /// Parsed from MediaWiki (older BIPs).
    Mediawiki,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default)]
pub struct Author {
    pub name: String,
    pub organization: Option<String>,
    /// A link for the author — a GitHub profile (`https://github.com/handle`)
    /// or a `mailto:` — when the source carries one (e.g. EIP authors).
    pub link: Option<String>,
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
    /// reflowed. Rendered as a scalable SVG on a fixed character grid.
    Artwork(String),
    /// Source code / formal syntax — verbatim monospace with an optional
    /// language hint. The unhighlighted path (RFC ABNF, untagged fences).
    Code {
        text: String,
        language: Option<String>,
    },
    /// Pre-highlighted source code: an XHTML fragment of `<span class="…">`
    /// runs (produced by the syntax highlighter), wrapped in `<pre><code>` by
    /// the renderer. The syntax-aware counterpart of [`Block::Code`].
    HighlightedCode { language: String, html: String },
    /// A bullet or numbered list.
    List(List),
    /// A definition list (term/description pairs), e.g. xml2rfc `<dl>` and
    /// Markdown description lists, and also how bibliographies and footnote
    /// definitions are modelled (each entry may carry a [`DefEntry::anchor`] so
    /// cross-references can link to it).
    DefinitionList(Vec<DefEntry>),
    /// A real data table (xml2rfc `<table>` or a GFM table).
    Table(Table),
    /// An aside / note callout (also GFM alerts).
    Aside(Vec<Block>),
    /// A blockquote.
    Quote(Vec<Block>),
    /// A raster/vector image with alt text and an optional caption. `resource`
    /// names an [`Asset`] embedded in the EPUB.
    Figure {
        resource: String,
        alt: String,
        caption: Option<Vec<Inline>>,
    },
    /// A rendered diagram (SVG), flowing through the same SVG machinery as
    /// [`Block::Artwork`]. `source` keeps the diagram's text for fallback.
    Diagram { svg: String, source: String },
    /// Display math (`$$…$$`): MathML Core markup, with the LaTeX `source` kept
    /// for `alttext` / fallback.
    Math { mathml: String, source: String },
    /// A thematic break (`<hr>` / `---`).
    ThematicBreak,
    /// A page boundary from the original document's pagination. Recorded by the
    /// plain-text parser (xml2rfc has no page concept); the renderer forces a
    /// page break here unless page-break rendering is disabled.
    PageBreak,
}

/// One entry in a definition list, bibliography, or footnote list.
#[derive(Debug, Clone)]
pub struct DefEntry {
    /// Anchor id for this entry, so an [`Inline::XRef`] / [`Inline::FootnoteRef`]
    /// can resolve to it. Set for bibliography entries (from a
    /// `<reference anchor="…">`) and footnote definitions; `None` for ordinary
    /// definition lists.
    pub anchor: Option<String>,
    pub term: Vec<Inline>,
    pub description: Vec<Block>,
}

#[derive(Debug, Clone)]
pub struct List {
    pub ordered: bool,
    /// Each item is a sequence of blocks so lists can nest and hold paragraphs.
    pub items: Vec<Vec<Block>>,
}

/// Column alignment for a table cell (GFM tables carry these; RFC tables leave
/// the vector empty).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alignment {
    #[default]
    None,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Default)]
pub struct Table {
    pub head: Vec<Vec<Inline>>,
    pub rows: Vec<Vec<Vec<Inline>>>,
    /// Per-column alignment; empty means "unspecified for all columns".
    pub align: Vec<Alignment>,
}

impl Table {
    /// Alignment for column `i`, or [`Alignment::None`] when unspecified.
    pub fn column_align(&self, i: usize) -> Alignment {
        self.align.get(i).copied().unwrap_or_default()
    }
}

/// Inline (phrasing) content.
#[derive(Debug, Clone)]
pub enum Inline {
    Text(String),
    /// Emphasised text (`<em>`).
    Emph(Vec<Inline>),
    /// Strong text (`<strong>`) — also used for BCP 14 keywords like MUST.
    Strong(Vec<Inline>),
    /// Struck-through text (`<del>`, GFM `~~…~~`).
    Strikethrough(Vec<Inline>),
    /// Inline monospace (`<tt>` / `<code>`).
    Code(String),
    /// A hyperlink to an external URI (`<eref>` / Markdown link).
    Link {
        text: Vec<Inline>,
        href: String,
    },
    /// An internal cross-reference (`<xref>`); resolved to an in-book anchor
    /// when the target is local, otherwise shown as plain text.
    XRef {
        text: Vec<Inline>,
        target: String,
    },
    /// An inline image; `resource` names an [`Asset`] embedded in the EPUB.
    Image {
        resource: String,
        alt: String,
    },
    /// A footnote reference; renders as a superscript link to the footnote
    /// definition anchored at `fn-{name}`.
    FootnoteRef {
        name: String,
        number: usize,
    },
    /// Inline math (`$…$`): MathML Core markup with the LaTeX `source` kept.
    Math {
        mathml: String,
        source: String,
    },
    /// A hard line break (`<br/>`).
    LineBreak,
}

impl Inline {
    /// Convenience constructor for a plain text run.
    pub fn text(s: impl Into<String>) -> Self {
        Inline::Text(s.into())
    }
}
