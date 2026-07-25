//! Parser for GitHub-flavored **Markdown** spec collections (EIPs, ERCs, BIPs
//! in Markdown, BOLTs, CAIPs) into the shared [`Document`] IR.
//!
//! Structure: [`comrak`] gives us a full AST; we transform it into our IR rather
//! than ever touching its HTML output. Two things make the mapping faithful to
//! how GitHub renders these repos:
//!
//! * **Headings become sections.** ATX heading levels are folded into a nested
//!   [`Section`] tree, and each section id is a *GitHub-compatible* slug
//!   (comrak's [`Anchorizer`]) so that in-document `#anchor` cross-references
//!   resolve unchanged.
//! * **The preamble is RFC 822.** The `---` frontmatter (EIP/ERC/CAIP) — or the
//!   indented code-fence preamble (Markdown BIPs) — is parsed by
//!   [`preamble`](super::preamble) and mapped per collection.
//!
//! Fenced code is syntax-highlighted ([`crate::highlight`]); `$…$`/`$$…$$` math
//! becomes MathML ([`crate::mathml`]); images become [`Block::Figure`] /
//! [`Inline::Image`] carrying their original source path for the asset layer to
//! resolve; footnotes are collected into a trailing section.

use std::sync::OnceLock;

use comrak::nodes::{AstNode, ListType, NodeValue, TableAlignment};
use comrak::{parse_document, Anchorizer, Arena, Options};
use regex::Regex;

use crate::error::{Error, Result};
use crate::model::{
    Alignment, Block, Collection, DefEntry, Document, Inline, List, Section, SourceKind, Table,
};

/// Parse a Markdown document. `collection` is a hint from the caller (the CLI
/// knows it from the requested id); when `None` it is inferred from the
/// preamble. `number` is a fallback id number.
pub fn parse(body: &str, collection: Option<Collection>, number: Option<u32>) -> Result<Document> {
    let arena = Arena::new();
    let root = parse_document(&arena, body, &options());

    // --- Pass 1: split root children into preamble / headings / blocks, and
    // collect footnote definitions. ---
    let mut preamble: Option<super::preamble::Preamble> = None;
    let mut items: Vec<Item> = Vec::new();
    let mut footnotes: Vec<(String, Vec<Block>)> = Vec::new();
    let mut seen_content = false;

    // The converter needs the collection for link resolution; infer a
    // provisional one from a `---` frontmatter first, refine after.
    let conv = Converter { collection };

    for node in root.children() {
        let ast = node.data();
        match &ast.value {
            NodeValue::FrontMatter(raw) => {
                preamble = Some(super::preamble::Preamble::parse(raw));
            }
            NodeValue::FootnoteDefinition(fd) => {
                footnotes.push((fd.name.clone(), conv.blocks_of(node)));
            }
            NodeValue::Heading(h) => {
                seen_content = true;
                items.push(Item::Heading {
                    level: h.level,
                    title: text_of(node),
                });
            }
            NodeValue::CodeBlock(cb)
                if !seen_content && preamble.is_none() && looks_like_preamble(&cb.literal) =>
            {
                // Markdown BIPs carry the preamble in a leading indented fence.
                preamble = Some(super::preamble::Preamble::parse(&cb.literal));
            }
            _ => {
                seen_content = true;
                drop(ast); // release the borrow before recursive conversion
                if let Some(block) = conv.block_from_node(node) {
                    items.push(Item::Block(block));
                }
            }
        }
    }

    let preamble = preamble.unwrap_or_default();
    let collection = infer_collection(&preamble).or(collection);
    // Re-resolve links with the settled collection if it changed the inference.
    let conv = Converter { collection };
    let _ = conv; // link resolution already ran; collection only affects branding now

    let mut doc = Document {
        source: SourceKind::Markdown,
        ..Default::default()
    };
    super::preamble::apply_preamble(&mut doc, &preamble, collection, number);

    // --- Build the section tree from headings. ---
    let (mut sections, title_from_h1) = build_sections(items, &doc.title);
    if doc.title.is_empty() {
        doc.title = title_from_h1
            .or_else(|| doc.id.map(|d| d.label()))
            .unwrap_or_else(|| "Untitled".into());
    }

    // Footnotes become a trailing "Footnotes" section (definitions anchored at
    // `fn-{name}` so [`Inline::FootnoteRef`] resolves across content files).
    if !footnotes.is_empty() {
        let entries: Vec<DefEntry> = footnotes
            .into_iter()
            .enumerate()
            .map(|(i, (name, description))| DefEntry {
                anchor: Some(format!("fn-{name}")),
                term: vec![Inline::text(format!("{}.", i + 1))],
                description,
            })
            .collect();
        sections.push(Section {
            number: None,
            title: "Footnotes".into(),
            id: "footnotes".into(),
            blocks: vec![Block::DefinitionList(entries)],
            subsections: Vec::new(),
        });
    }

    if sections.is_empty() {
        return Err(Error::Parse("markdown document had no content".into()));
    }
    doc.sections = sections;
    Ok(doc)
}

/// comrak options: enable exactly the GFM extensions we map, plus frontmatter
/// and math. We consume the AST directly, so no HTML-render options matter.
fn options() -> Options<'static> {
    let mut o = Options::default();
    o.extension.table = true;
    o.extension.strikethrough = true;
    o.extension.autolink = true;
    o.extension.tasklist = true;
    o.extension.footnotes = true;
    o.extension.description_lists = true;
    o.extension.front_matter_delimiter = Some("---".to_string());
    o.extension.math_dollars = true;
    o.extension.math_code = true;
    o.extension.math_latex = true;
    o.extension.alerts = true;
    o
}

/// A top-level document item: a heading (section boundary) or a content block.
enum Item {
    Heading { level: u8, title: String },
    Block(Block),
}

/// Stateless (per-document) converter carrying only the collection, used for
/// resolving cross-document links.
struct Converter {
    collection: Option<Collection>,
}

impl Converter {
    fn blocks_of<'a>(&self, node: &'a AstNode<'a>) -> Vec<Block> {
        node.children()
            .filter_map(|c| self.block_from_node(c))
            .collect()
    }

    fn block_from_node<'a>(&self, node: &'a AstNode<'a>) -> Option<Block> {
        let ast = node.data();
        match &ast.value {
            NodeValue::Paragraph => {
                drop(ast);
                Some(self.paragraph(node))
            }
            // A heading nested inside a container (list/quote) is not a section;
            // degrade it to a bold paragraph so its text survives.
            NodeValue::Heading(_) => {
                drop(ast);
                Some(Block::Paragraph(vec![Inline::Strong(
                    self.inlines_of(node),
                )]))
            }
            NodeValue::CodeBlock(cb) => Some(code_block(&cb.info, &cb.literal)),
            NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
                drop(ast);
                Some(Block::Quote(self.blocks_of(node)))
            }
            NodeValue::Alert(a) => {
                let label = a
                    .title
                    .clone()
                    .unwrap_or_else(|| a.alert_type.default_title().to_string());
                drop(ast);
                let mut blocks = vec![Block::Paragraph(vec![Inline::Strong(vec![Inline::text(
                    label,
                )])])];
                blocks.extend(self.blocks_of(node));
                Some(Block::Aside(blocks))
            }
            NodeValue::List(nl) => {
                let ordered = matches!(nl.list_type, ListType::Ordered);
                drop(ast);
                Some(Block::List(self.list(node, ordered)))
            }
            NodeValue::Item(_) | NodeValue::TaskItem(_) => None, // handled by list()
            NodeValue::DescriptionList => {
                drop(ast);
                Some(self.description_list(node))
            }
            NodeValue::Table(_) => {
                drop(ast);
                Some(Block::Table(self.table(node)))
            }
            NodeValue::ThematicBreak => Some(Block::ThematicBreak),
            NodeValue::HtmlBlock(h) => html_block(&h.literal),
            _ => None,
        }
    }

    /// A paragraph, with two special cases GitHub renders as blocks: a lone
    /// image becomes a figure; a lone display-math node becomes block math.
    fn paragraph<'a>(&self, node: &'a AstNode<'a>) -> Block {
        let kids: Vec<&'a AstNode<'a>> = node.children().collect();
        if kids.len() == 1 {
            let ast = kids[0].data();
            match &ast.value {
                NodeValue::Image(nl) => {
                    let caption =
                        (!nl.title.is_empty()).then(|| vec![Inline::text(nl.title.clone())]);
                    return Block::Figure {
                        resource: nl.url.clone(),
                        alt: text_of(kids[0]),
                        caption,
                    };
                }
                NodeValue::Math(nm) if nm.display_math => {
                    return math_block(&nm.literal);
                }
                _ => {}
            }
        }
        Block::Paragraph(self.inlines_of(node))
    }

    fn list<'a>(&self, node: &'a AstNode<'a>, ordered: bool) -> List {
        let mut items = Vec::new();
        for child in node.children() {
            let ast = child.data();
            match &ast.value {
                NodeValue::Item(_) => {
                    drop(ast);
                    items.push(self.blocks_of(child));
                }
                NodeValue::TaskItem(nti) => {
                    let checked = nti.symbol.is_some();
                    drop(ast);
                    let mut blocks = self.blocks_of(child);
                    let mark = if checked { "\u{2611} " } else { "\u{2610} " };
                    match blocks.first_mut() {
                        Some(Block::Paragraph(inl)) => inl.insert(0, Inline::text(mark)),
                        _ => blocks.insert(0, Block::Paragraph(vec![Inline::text(mark)])),
                    }
                    items.push(blocks);
                }
                _ => {}
            }
        }
        List { ordered, items }
    }

    fn description_list<'a>(&self, node: &'a AstNode<'a>) -> Block {
        let mut entries = Vec::new();
        for item in node.children() {
            if !matches!(item.data().value, NodeValue::DescriptionItem(_)) {
                continue;
            }
            let mut term = Vec::new();
            let mut description = Vec::new();
            for part in item.children() {
                match &part.data().value {
                    NodeValue::DescriptionTerm => term = self.inlines_of(part),
                    NodeValue::DescriptionDetails => description = self.blocks_of(part),
                    _ => {}
                }
            }
            entries.push(DefEntry {
                anchor: None,
                term,
                description,
            });
        }
        Block::DefinitionList(entries)
    }

    fn table<'a>(&self, node: &'a AstNode<'a>) -> Table {
        let mut table = Table::default();
        if let NodeValue::Table(nt) = &node.data().value {
            table.align = nt.alignments.iter().map(map_alignment).collect();
        }
        for row in node.children() {
            let is_header = matches!(row.data().value, NodeValue::TableRow(true));
            let cells: Vec<Vec<Inline>> = row
                .children()
                .filter(|c| matches!(c.data().value, NodeValue::TableCell))
                .map(|c| self.inlines_of(c))
                .collect();
            if is_header {
                table.head = cells;
            } else if !cells.is_empty() {
                table.rows.push(cells);
            }
        }
        table
    }

    fn inlines_of<'a>(&self, node: &'a AstNode<'a>) -> Vec<Inline> {
        let mut out = Vec::new();
        for c in node.children() {
            self.inline_from_node(c, &mut out);
        }
        out
    }

    fn inline_from_node<'a>(&self, node: &'a AstNode<'a>, out: &mut Vec<Inline>) {
        let ast = node.data();
        match &ast.value {
            NodeValue::Text(t) => out.push(Inline::Text(t.to_string())),
            NodeValue::SoftBreak => out.push(Inline::text(" ")),
            NodeValue::LineBreak => out.push(Inline::LineBreak),
            NodeValue::Code(nc) => out.push(Inline::Code(nc.literal.clone())),
            NodeValue::Emph => {
                drop(ast);
                out.push(Inline::Emph(self.inlines_of(node)));
            }
            NodeValue::Strong => {
                drop(ast);
                out.push(Inline::Strong(self.inlines_of(node)));
            }
            NodeValue::Strikethrough => {
                drop(ast);
                out.push(Inline::Strikethrough(self.inlines_of(node)));
            }
            NodeValue::Link(nl) => {
                let url = nl.url.clone();
                drop(ast);
                let text = self.inlines_of(node);
                out.push(self.resolve_link(&url, text));
            }
            NodeValue::Image(nl) => out.push(Inline::Image {
                resource: nl.url.clone(),
                alt: text_of(node),
            }),
            NodeValue::Math(nm) => out.push(inline_math(&nm.literal)),
            NodeValue::FootnoteReference(fr) => out.push(Inline::FootnoteRef {
                name: fr.name.clone(),
                number: fr.ix as usize,
            }),
            NodeValue::HtmlInline(s) => {
                if is_br(s) {
                    out.push(Inline::LineBreak);
                }
                // Other raw inline tags (<sup>, <sub>, …) are dropped; their
                // text content arrives as sibling Text nodes.
            }
            // Any other inline container: keep its text, drop the wrapper.
            _ => {
                drop(ast);
                for c in node.children() {
                    self.inline_from_node(c, out);
                }
            }
        }
    }

    /// Resolve a link. Same-document `#anchors` become cross-references (their
    /// GitHub slugs match our section ids); relative links to *other* documents
    /// in a known collection become absolute web links (an EPUB holds one doc).
    fn resolve_link(&self, url: &str, text: Vec<Inline>) -> Inline {
        let _ = self.collection; // reserved for future collection-aware resolution
        resolve_link(url, text)
    }
}

/// Resolve a link URL to an inline (the collection-independent core of
/// [`Converter::resolve_link`], reused by the raw-HTML table pass): a `#anchor`
/// becomes a cross-reference, a relative other-document link rewrites to its
/// canonical web URL, and anything else stays an external link.
fn resolve_link(url: &str, text: Vec<Inline>) -> Inline {
    let url = url.trim();
    if let Some(frag) = url.strip_prefix('#') {
        return Inline::XRef {
            text,
            target: frag.to_string(),
        };
    }
    if let Some(href) = rewrite_doc_link(url) {
        return Inline::Link { text, href };
    }
    Inline::Link {
        text,
        href: url.to_string(),
    }
}

/// Fold heading-delimited items into a nested section tree. Returns the sections
/// and, when the document is wrapped in a single leading title heading, that
/// heading's text (a title fallback). `title` (from the preamble) lets us drop a
/// duplicate leading H1.
fn build_sections(items: Vec<Item>, title: &str) -> (Vec<Section>, Option<String>) {
    let levels: Vec<u8> = items
        .iter()
        .filter_map(|i| match i {
            Item::Heading { level, .. } => Some(*level),
            _ => None,
        })
        .collect();
    let Some(&min) = levels.iter().min() else {
        // No headings at all: one section holding everything.
        let blocks: Vec<Block> = items
            .into_iter()
            .filter_map(|i| match i {
                Item::Block(b) => Some(b),
                _ => None,
            })
            .collect();
        if blocks.is_empty() {
            return (Vec::new(), None);
        }
        return (
            vec![Section {
                number: None,
                title: if title.is_empty() {
                    "Document".into()
                } else {
                    title.into()
                },
                id: "body".into(),
                blocks,
                subsections: Vec::new(),
            }],
            None,
        );
    };

    // Promote past a single wrapping *title* heading (`# Title` above `## …`) so
    // the real chapters are its children, not one giant chapter. Restricted to a
    // leading level-1 heading: a lone deeper heading is a real section, not a
    // document title.
    let count_at_min = levels.iter().filter(|&&l| l == min).count();
    let first_is_min = matches!(items.first(), Some(Item::Heading { level, .. }) if *level == min);
    let has_deeper = levels.iter().any(|&l| l > min);
    let promote = min == 1 && count_at_min == 1 && first_is_min && has_deeper;
    let top = if promote { min + 1 } else { min };

    let mut anchorizer = Anchorizer::new();
    let mut title_from_h1 = None;
    let mut leading: Vec<Block> = Vec::new();
    let mut flat: Vec<(usize, Section)> = Vec::new();

    for item in items {
        match item {
            Item::Heading {
                level,
                title: htitle,
            } => {
                if level < top {
                    // The promoted wrapper heading: title source, not a section.
                    if title_from_h1.is_none() && !htitle.is_empty() {
                        title_from_h1 = Some(htitle);
                    }
                    continue;
                }
                let id = anchorizer.anchorize(&htitle);
                let depth = (level as usize).saturating_sub(top as usize) + 1;
                flat.push((
                    depth,
                    Section {
                        number: None,
                        title: htitle,
                        id,
                        blocks: Vec::new(),
                        subsections: Vec::new(),
                    },
                ));
            }
            Item::Block(b) => match flat.last_mut() {
                Some((_, sec)) => sec.blocks.push(b),
                None => leading.push(b),
            },
        }
    }

    let mut roots = nest_sections(flat);
    if !leading.is_empty() {
        // Content before the first chapter (rare for spec files).
        roots.insert(
            0,
            Section {
                number: None,
                title: "Overview".into(),
                id: "overview".into(),
                blocks: leading,
                subsections: Vec::new(),
            },
        );
    }
    (roots, title_from_h1)
}

/// Nest a flat, in-order `(depth, Section)` list into a tree.
fn nest_sections(flat: Vec<(usize, Section)>) -> Vec<Section> {
    let mut roots: Vec<Section> = Vec::new();
    for (depth, section) in flat {
        insert_at_depth(&mut roots, section, depth);
    }
    roots
}

fn insert_at_depth(siblings: &mut Vec<Section>, section: Section, depth: usize) {
    if depth <= 1 {
        siblings.push(section);
        return;
    }
    if let Some(parent) = siblings.last_mut() {
        insert_at_depth(&mut parent.subsections, section, depth - 1);
    } else {
        siblings.push(section);
    }
}

// ---------------------------------------------------------------------------
// Preamble → Document mapping
// ---------------------------------------------------------------------------

fn infer_collection(pre: &super::preamble::Preamble) -> Option<Collection> {
    if pre.get("eip").is_some() {
        // ERC files keep the `eip:` key but carry `category: ERC`.
        if pre
            .get("category")
            .is_some_and(|c| c.eq_ignore_ascii_case("ERC"))
        {
            Some(Collection::Erc)
        } else {
            Some(Collection::Eip)
        }
    } else if pre.get("bip").is_some() {
        Some(Collection::Bip)
    } else if pre.get("caip").is_some() {
        Some(Collection::Caip)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Block/inline helpers
// ---------------------------------------------------------------------------

/// A fenced code block: math fences → MathML, mermaid → a diagram (rendered as
/// its source until a diagram backend is wired in), a recognized language →
/// highlighted, otherwise verbatim.
fn code_block(info: &str, literal: &str) -> Block {
    let lang = info.split([' ', '\t', ',']).next().unwrap_or("").trim();
    let code = literal.strip_suffix('\n').unwrap_or(literal);

    if lang.eq_ignore_ascii_case("math") {
        return math_block(code);
    }
    if lang.eq_ignore_ascii_case("mermaid") {
        // Mermaid renders in-process only when the `mermaid` feature fills the
        // `svg` post-parse (see `crate::diagram`); the empty `svg` here is that
        // extension point, and the renderer falls back to the verbatim source.
        return Block::Diagram {
            svg: String::new(),
            source: code.to_string(),
        };
    }
    // The fence's declared language, or — for the many untagged fences in the
    // EIP/ERC corpus — an obvious one sniffed from the content.
    let sniffed = lang.is_empty().then(|| sniff_language(code)).flatten();
    let effective = if !lang.is_empty() {
        Some(lang)
    } else {
        sniffed
    };
    if let Some(l) = effective {
        if let Some(html) = crate::highlight::highlight(code, l) {
            return Block::HighlightedCode {
                language: l.to_string(),
                html,
            };
        }
    }
    Block::Code {
        text: code.to_string(),
        language: (!lang.is_empty()).then(|| lang.to_string()),
    }
}

/// Sniff an obvious language for an **untagged** fence so it can be highlighted
/// instead of rendered as verbatim artwork. Deliberately conservative — only
/// unmistakable JSON and Solidity are recognized; anything ambiguous stays a
/// plain code block.
fn sniff_language(code: &str) -> Option<&'static str> {
    let t = code.trim();
    let (Some(first), Some(last)) = (t.chars().next(), t.chars().last()) else {
        return None;
    };
    // JSON: a bracketed object/array that carries a quoted key or string.
    let bracketed = (first == '{' && last == '}') || (first == '[' && last == ']');
    if bracketed && (t.contains("\":") || t.contains("\" :") || (first == '[' && t.contains('"'))) {
        return Some("json");
    }
    // Solidity: the pragma is unmistakable; a leading contract/interface/library
    // declaration with a body is a strong signal too.
    if t.contains("pragma solidity") {
        return Some("solidity");
    }
    let decl = ["contract ", "interface ", "library ", "abstract contract "]
        .iter()
        .any(|k| t.starts_with(k));
    if decl && t.contains('{') {
        return Some("solidity");
    }
    None
}

fn math_block(latex: &str) -> Block {
    match crate::mathml::latex_to_mathml(latex, true) {
        Some(mathml) => Block::Math {
            mathml,
            source: latex.to_string(),
        },
        None => Block::Code {
            text: latex.to_string(),
            language: None,
        },
    }
}

fn inline_math(latex: &str) -> Inline {
    match crate::mathml::latex_to_mathml(latex, false) {
        Some(mathml) => Inline::Math {
            mathml,
            source: latex.to_string(),
        },
        None => Inline::Code(latex.to_string()),
    }
}

/// Best-effort handling of a raw HTML block. A well-formed `<table>` becomes a
/// real [`Block::Table`]; a `<details>`/`<summary>` disclosure becomes a
/// [`Block::Aside`]; anything else is stripped to text so nothing is lost (we
/// deliberately do not try to be a browser). Empty → dropped.
fn html_block(literal: &str) -> Option<Block> {
    let trimmed = literal.trim();
    let lower = trimmed.to_ascii_lowercase();

    // A real data table → a structured table (falls through to tag-stripping if
    // the fragment isn't well-formed enough to parse).
    if lower.contains("<table") {
        if let Some(table) = parse_html_table(trimmed) {
            return Some(Block::Table(table));
        }
    }
    // A disclosure widget → an aside. comrak often splits `<details>` across
    // several HTML blocks (its markdown body sits between them), so this handles
    // both a whole `<details>…</details>` and a lone opening `<summary>` tag.
    if lower.contains("<details") || lower.contains("<summary") {
        return details_aside(trimmed);
    }

    let text = strip_tags(trimmed);
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(Block::Paragraph(vec![Inline::text(text)]))
    }
}

/// Parse a raw-HTML `<table>` fragment into a [`Table`] with [`roxmltree`],
/// after light sanitizing (closing void tags, decoding stray entities) so the
/// common EIP inline table parses as XML. Returns `None` on any failure so the
/// caller falls back to tag-stripping.
fn parse_html_table(html: &str) -> Option<Table> {
    let fragment = extract_element(html, "table")?;
    let xml = sanitize_html_fragment(&fragment);
    let doc = roxmltree::Document::parse(&xml).ok()?;
    let root = doc.root_element();

    let mut table = Table::default();
    for tr in root.descendants().filter(|n| n.has_tag_name("tr")) {
        let mut all_header = true;
        let mut cells = Vec::new();
        for cell in tr
            .children()
            .filter(|n| n.is_element() && (n.has_tag_name("td") || n.has_tag_name("th")))
        {
            if !cell.has_tag_name("th") {
                all_header = false;
            }
            let mut inlines = Vec::new();
            html_inlines(cell, &mut inlines);
            cells.push(inlines);
        }
        if cells.is_empty() {
            continue;
        }
        // The leading row is the head only when *every* cell is a `<th>`; a
        // key/value row (`<th>label</th><td>value</td>`) stays a body row so the
        // value isn't promoted into a header column.
        if all_header && table.head.is_empty() && table.rows.is_empty() {
            table.head = cells;
        } else {
            table.rows.push(cells);
        }
    }

    (!table.head.is_empty() || !table.rows.is_empty()).then_some(table)
}

/// Convert the inline content of an HTML element (a table cell or `<summary>`)
/// into IR inlines, mapping the small set of formatting tags that show up in
/// EIP tables and degrading unknown tags to their text.
fn html_inlines(node: roxmltree::Node, out: &mut Vec<Inline>) {
    for child in node.children() {
        if child.is_text() {
            if let Some(t) = child.text() {
                out.push(Inline::Text(t.to_string()));
            }
            continue;
        }
        if !child.is_element() {
            continue;
        }
        match child.tag_name().name().to_ascii_lowercase().as_str() {
            "code" | "tt" => out.push(Inline::Code(node_text(child))),
            "strong" | "b" => {
                let mut inner = Vec::new();
                html_inlines(child, &mut inner);
                out.push(Inline::Strong(inner));
            }
            "em" | "i" => {
                let mut inner = Vec::new();
                html_inlines(child, &mut inner);
                out.push(Inline::Emph(inner));
            }
            "del" | "s" | "strike" => {
                let mut inner = Vec::new();
                html_inlines(child, &mut inner);
                out.push(Inline::Strikethrough(inner));
            }
            "br" => out.push(Inline::LineBreak),
            "a" => {
                let href = child.attribute("href").unwrap_or_default();
                let mut inner = Vec::new();
                html_inlines(child, &mut inner);
                if inner.is_empty() {
                    inner.push(Inline::text(href));
                }
                out.push(resolve_link(href, inner));
            }
            // sup / sub / span / anything else: keep the text, drop the wrapper.
            _ => html_inlines(child, out),
        }
    }
}

/// The concatenated text content of an element (for `<code>` cells).
fn node_text(node: roxmltree::Node) -> String {
    let mut s = String::new();
    for d in node.descendants() {
        if d.is_text() {
            if let Some(t) = d.text() {
                s.push_str(t);
            }
        }
    }
    s
}

/// Map a `<details>`/`<summary>` disclosure to an aside: the summary becomes a
/// bold lead line and any inline body text follows. Content-losing cases (a bare
/// closing `</details>`) yield `None`.
fn details_aside(html: &str) -> Option<Block> {
    let mut blocks = Vec::new();
    if let Some(summary) = extract_element_inner(html, "summary") {
        let summary = strip_tags(&summary);
        let summary = summary.trim();
        if !summary.is_empty() {
            blocks.push(Block::Paragraph(vec![Inline::Strong(vec![Inline::text(
                summary,
            )])]));
        }
    }
    // Body: the fragment minus the summary, stripped to text. Usually empty
    // (the markdown body arrives as separate blocks); non-empty for a fully
    // self-contained `<details>`.
    let without_summary = remove_element(html, "summary");
    let body = strip_tags(&without_summary);
    let body = body.trim();
    if !body.is_empty() {
        blocks.push(Block::Paragraph(vec![Inline::text(body)]));
    }
    (!blocks.is_empty()).then_some(Block::Aside(blocks))
}

/// Extract the first complete `<tag …>…</tag>` fragment (case-insensitive).
fn extract_element(html: &str, tag: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find(&format!("<{tag}"))?;
    let close = format!("</{tag}>");
    let close_start = lower.rfind(&close)?;
    if close_start < start {
        return None;
    }
    Some(html[start..close_start + close.len()].to_string())
}

/// The inner content of the first `<tag …>…</tag>` (case-insensitive), tags
/// excluded.
fn extract_element_inner(html: &str, tag: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = lower.find(&format!("<{tag}"))?;
    let inner_start = html[open..].find('>')? + open + 1;
    let close = format!("</{tag}>");
    let close_start = lower[inner_start..].find(&close)? + inner_start;
    Some(html[inner_start..close_start].to_string())
}

/// Remove every `<tag …>…</tag>` span (case-insensitive) from `html`.
fn remove_element(html: &str, tag: &str) -> String {
    let mut out = html.to_string();
    while let Some(frag) = extract_element(&out, tag) {
        out = out.replacen(&frag, " ", 1);
    }
    out
}

/// Make a raw-HTML fragment well-formed enough for an XML parser: self-close the
/// common void tags and neutralize entities `roxmltree` doesn't predefine.
fn sanitize_html_fragment(html: &str) -> String {
    static VOID: OnceLock<Regex> = OnceLock::new();
    let void = VOID.get_or_init(|| {
        Regex::new(r"(?i)<(br|hr|img|wbr|col|input)\b([^>]*?)/?>").expect("valid regex")
    });

    let mut s = html.to_string();
    // A handful of named entities XML doesn't know, mapped to their character.
    for (from, to) in [
        ("&nbsp;", "\u{00A0}"),
        ("&mdash;", "—"),
        ("&ndash;", "–"),
        ("&hellip;", "…"),
        ("&rarr;", "→"),
        ("&larr;", "←"),
        ("&times;", "×"),
        ("&middot;", "·"),
        ("&deg;", "°"),
        ("&le;", "≤"),
        ("&ge;", "≥"),
        ("&ne;", "≠"),
        ("&copy;", "©"),
        ("&reg;", "®"),
        ("&trade;", "™"),
    ] {
        if s.contains(from) {
            s = s.replace(from, to);
        }
    }
    let s = void.replace_all(&s, "<$1$2/>").into_owned();
    escape_bare_amp(&s)
}

/// Escape every `&` that is not the start of an XML-predefined entity
/// (`&amp;`/`&lt;`/`&gt;`/`&quot;`/`&apos;`) or a numeric character reference, so
/// stray ampersands in raw HTML don't fail the XML parse. The `regex` crate has
/// no look-around, so this is a small manual scan.
fn escape_bare_amp(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos..]; // starts with '&'
        if is_predefined_entity(after) {
            out.push('&');
        } else {
            out.push_str("&amp;");
        }
        rest = &after[1..];
    }
    out.push_str(rest);
    out
}

/// Whether `s` (which starts with `&`) begins with an XML-predefined named
/// entity or a numeric character reference.
fn is_predefined_entity(s: &str) -> bool {
    let body = &s[1..];
    let Some(semi) = body.find(';') else {
        return false;
    };
    if semi == 0 || semi > 8 {
        return false;
    }
    let ent = &body[..semi];
    if let Some(num) = ent.strip_prefix('#') {
        let num = num.strip_prefix(['x', 'X']).unwrap_or(num);
        return !num.is_empty() && num.chars().all(|c| c.is_ascii_hexdigit());
    }
    matches!(ent, "amp" | "lt" | "gt" | "quot" | "apos")
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    // Collapse whitespace runs.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_br(s: &str) -> bool {
    let t = s.trim().to_ascii_lowercase();
    matches!(t.as_str(), "<br>" | "<br/>" | "<br />")
}

fn map_alignment(a: &TableAlignment) -> Alignment {
    match a {
        TableAlignment::Left => Alignment::Left,
        TableAlignment::Center => Alignment::Center,
        TableAlignment::Right => Alignment::Right,
        TableAlignment::None => Alignment::None,
    }
}

/// A leading fenced code block whose content parses as a spec preamble (used to
/// detect the Markdown-BIP preamble fence).
fn looks_like_preamble(literal: &str) -> bool {
    super::preamble::Preamble::parse(literal).has_any(&["bip", "eip", "caip", "title"])
}

/// The flattened inline text of a node (headings, alt text, titles).
fn text_of<'a>(node: &'a AstNode<'a>) -> String {
    let mut s = String::new();
    collect_text(node, &mut s);
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collect_text<'a>(node: &'a AstNode<'a>, out: &mut String) {
    match &node.data().value {
        NodeValue::Text(t) => out.push_str(t),
        NodeValue::Code(nc) => out.push_str(&nc.literal),
        NodeValue::Math(nm) => out.push_str(&nm.literal),
        NodeValue::SoftBreak | NodeValue::LineBreak => out.push(' '),
        _ => {
            for c in node.children() {
                collect_text(c, out);
            }
        }
    }
}

/// Rewrite a relative link to another collection document into an absolute web
/// link. `./eip-2718.md#foo` → `https://eips.ethereum.org/EIPS/eip-2718#foo`.
/// Shared with the MediaWiki parser, whose `[[bip-0032.mediawiki|BIP32]]`
/// cross-document wiki links resolve the same way.
pub(crate) fn rewrite_doc_link(url: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)^(?:\.{1,2}/)*(?:[\w.-]+/)*(eip|erc|bip|caip|bolt)-0*(\d+)(?:\.(?:md|mediawiki))?(#.*)?$")
            .expect("valid regex")
    });
    let caps = re.captures(url)?;
    let collection = Collection::from_token(&caps[1])?;
    let number: u32 = caps[2].parse().ok()?;
    let frag = caps.get(3).map(|m| m.as_str()).unwrap_or("");
    Some(format!("{}{frag}", collection.external_url(number)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sections_titles(doc: &Document) -> Vec<&str> {
        doc.sections.iter().map(|s| s.title.as_str()).collect()
    }

    #[test]
    fn promotes_past_a_single_wrapping_title_heading() {
        // `# Title` above `## A`/`## B`: chapters are the H2s, not one big H1.
        let md = "# The Whole Doc\n\n## Alpha\n\ntext a\n\n## Beta\n\ntext b\n";
        let doc = parse(md, None, None).unwrap();
        assert_eq!(sections_titles(&doc), vec!["Alpha", "Beta"]);
        // With no frontmatter title, the wrapper heading supplies the title.
        assert_eq!(doc.title, "The Whole Doc");
    }

    #[test]
    fn sibling_h1s_stay_top_level() {
        let md = "# One\n\na\n\n# Two\n\nb\n";
        let doc = parse(md, None, None).unwrap();
        assert_eq!(sections_titles(&doc), vec!["One", "Two"]);
    }

    #[test]
    fn nested_headings_build_subsection_tree() {
        let md = "## Top\n\n### Mid\n\n#### Deep\n\nx\n";
        let doc = parse(md, None, None).unwrap();
        assert_eq!(sections_titles(&doc), vec!["Top"]);
        assert_eq!(doc.sections[0].subsections[0].title, "Mid");
        assert_eq!(doc.sections[0].subsections[0].subsections[0].title, "Deep");
    }

    #[test]
    fn mermaid_fence_becomes_a_diagram_with_source() {
        let md = "## D\n\n```mermaid\ngraph TD; A-->B;\n```\n";
        let doc = parse(md, None, None).unwrap();
        let has = doc.sections.iter().any(|s| {
            s.blocks.iter().any(|b| {
                matches!(b, Block::Diagram { svg, source }
                if svg.is_empty() && source.contains("A-->B"))
            })
        });
        assert!(
            has,
            "mermaid should map to an (unrendered) Diagram carrying its source"
        );
    }

    #[test]
    fn task_list_items_get_checkbox_markers() {
        let md = "## L\n\n- [x] done\n- [ ] todo\n";
        let doc = parse(md, None, None).unwrap();
        let list = doc.sections[0].blocks.iter().find_map(|b| match b {
            Block::List(l) => Some(l),
            _ => None,
        });
        let list = list.expect("a list");
        // First item carries a checked mark, second an unchecked one.
        let first = format!("{:?}", list.items[0]);
        let second = format!("{:?}", list.items[1]);
        assert!(first.contains('\u{2611}'), "checked marker");
        assert!(second.contains('\u{2610}'), "unchecked marker");
    }

    #[test]
    fn rewrites_cross_document_links() {
        assert_eq!(
            rewrite_doc_link("./eip-2718.md").as_deref(),
            Some("https://eips.ethereum.org/EIPS/eip-2718"),
        );
        assert_eq!(
            rewrite_doc_link("../EIPS/eip-155.md#gas").as_deref(),
            Some("https://eips.ethereum.org/EIPS/eip-155#gas"),
        );
        assert_eq!(
            rewrite_doc_link("bip-0341.mediawiki").as_deref(),
            Some("https://github.com/bitcoin/bips/blob/master/bip-0341.mediawiki"),
        );
        // Not a doc link.
        assert_eq!(rewrite_doc_link("https://example.com/x"), None);
        assert_eq!(rewrite_doc_link("./contracts/Foo.sol"), None);
    }

    #[test]
    fn raw_html_table_becomes_a_real_table() {
        let html = "<table>\n<tr><th>Name</th><th>Value</th></tr>\n\
                    <tr><td>alpha</td><td><code>0x01</code></td></tr>\n\
                    <tr><td>beta &amp; co</td><td>2&nbsp;wei</td></tr>\n</table>";
        match html_block(html) {
            Some(Block::Table(t)) => {
                assert_eq!(t.head.len(), 2);
                assert_eq!(t.rows.len(), 2);
                // Header text, inline <code>, and entities survive.
                assert!(matches!(t.head[0].as_slice(), [Inline::Text(s)] if s == "Name"));
                assert!(matches!(t.rows[0][1].as_slice(), [Inline::Code(s)] if s == "0x01"));
                assert!(t.rows[1][0]
                    .iter()
                    .any(|i| matches!(i, Inline::Text(s) if s.contains('&'))));
            }
            other => panic!("expected a table, got {other:?}"),
        }
    }

    #[test]
    fn html_key_value_table_rows_stay_body_rows() {
        // A `<th>label</th><td>value</td>` row is NOT a header row — only an
        // all-`<th>` row is — so the value isn't promoted into a header column.
        let html = "<table>\n<tr><th>Name</th><td>alpha</td></tr>\n\
                    <tr><th>Age</th><td>30</td></tr>\n</table>";
        match html_block(html) {
            Some(Block::Table(t)) => {
                assert!(
                    t.head.is_empty(),
                    "no all-th header row: head = {:?}",
                    t.head
                );
                assert_eq!(t.rows.len(), 2, "both rows are body rows");
            }
            other => panic!("expected a table, got {other:?}"),
        }
    }

    #[test]
    fn malformed_html_table_falls_back_to_text() {
        // Not well-formed XML (stray unclosed tag with attributes) → tag-strip.
        match html_block("<table><tr><td>hi<td>there</table>") {
            Some(Block::Paragraph(inls)) => {
                assert!(matches!(inls.as_slice(), [Inline::Text(t)] if t == "hi there"));
            }
            other => panic!("expected a text paragraph fallback, got {other:?}"),
        }
        assert!(html_block("<br/>").is_none());
    }

    #[test]
    fn details_summary_becomes_an_aside() {
        // A lone opening tag (comrak splits the markdown body out): summary only.
        match html_block("<details><summary>Show proof</summary>") {
            Some(Block::Aside(blocks)) => {
                assert!(matches!(&blocks[0],
                    Block::Paragraph(inls) if matches!(inls.as_slice(),
                        [Inline::Strong(s)] if matches!(s.as_slice(), [Inline::Text(t)] if t == "Show proof"))));
            }
            other => panic!("expected an aside, got {other:?}"),
        }
        // A self-contained details keeps its body text too.
        match html_block("<details><summary>Title</summary>Body text here.</details>") {
            Some(Block::Aside(blocks)) => assert_eq!(blocks.len(), 2),
            other => panic!("expected an aside, got {other:?}"),
        }
    }

    #[test]
    fn sniffs_untagged_json_and_solidity_fences() {
        assert_eq!(sniff_language("{\n  \"a\": 1\n}"), Some("json"));
        assert_eq!(sniff_language("[\n  {\"x\": true}\n]"), Some("json"));
        assert_eq!(
            sniff_language("pragma solidity ^0.8.0;\ncontract C {}"),
            Some("solidity")
        );
        assert_eq!(
            sniff_language("interface IFoo {\n  function bar() external;\n}"),
            Some("solidity")
        );
        // Ambiguous / prose stays unsniffed.
        assert_eq!(sniff_language("just some words"), None);
        assert_eq!(sniff_language("{ not really json }"), None);
    }
}
