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
    Alignment, Author, Block, Collection, DefEntry, DocId, Document, Inline, List, Relation,
    Section, SourceKind, Table,
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
    apply_preamble(&mut doc, &preamble, collection, number);

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
        node.children().filter_map(|c| self.block_from_node(c)).collect()
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
                Some(Block::Paragraph(vec![Inline::Strong(self.inlines_of(node))]))
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
                    let caption = (!nl.title.is_empty()).then(|| vec![Inline::text(nl.title.clone())]);
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
            entries.push(DefEntry { anchor: None, term, description });
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
        let url = url.trim();
        if let Some(frag) = url.strip_prefix('#') {
            return Inline::XRef { text, target: frag.to_string() };
        }
        if let Some(href) = rewrite_doc_link(url) {
            return Inline::Link { text, href };
        }
        let _ = self.collection;
        Inline::Link { text, href: url.to_string() }
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
                title: if title.is_empty() { "Document".into() } else { title.into() },
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
            Item::Heading { level, title: htitle } => {
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
        if pre.get("category").is_some_and(|c| c.eq_ignore_ascii_case("ERC")) {
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

fn apply_preamble(
    doc: &mut Document,
    pre: &super::preamble::Preamble,
    collection: Option<Collection>,
    number: Option<u32>,
) {
    if pre.fields.is_empty() {
        doc.id = collection.zip(number).map(|(c, n)| DocId::new(c, n));
        return;
    }

    // Id number: the collection's id key, else the caller's number.
    let id_key = match collection {
        Some(Collection::Bip) => "bip",
        Some(Collection::Caip) => "caip",
        _ => "eip",
    };
    let num = pre
        .get(id_key)
        .and_then(|v| v.trim().parse::<u32>().ok())
        .or(number);
    doc.id = collection.zip(num).map(|(c, n)| DocId::new(c, n));

    if let Some(t) = pre.get("title") {
        doc.title = t.trim().to_string();
    }
    // Lifecycle status (Draft/Review/Final/…) — the prominent badge.
    if let Some(s) = pre.get("status") {
        doc.status = Some(s.trim().to_string());
    }
    // Publication date.
    if let Some(d) = pre.get("created").or_else(|| pre.get("date")) {
        doc.date = Some(d.trim().to_string());
    }
    // One-line description → abstract.
    if let Some(desc) = pre.get("description") {
        let desc = desc.trim();
        if !desc.is_empty() {
            doc.abstract_ = vec![Block::Paragraph(vec![Inline::text(desc)])];
        }
    }
    // Authors.
    if let Some(a) = pre.get("author").or_else(|| pre.get("authors")) {
        doc.authors = parse_authors(a);
    }
    // Relations to other documents.
    let own = collection.unwrap_or(Collection::Eip);
    for (label, key) in [("Requires", "requires"), ("Replaces", "replaces")] {
        if let Some(v) = pre.get(key) {
            let targets = parse_id_list(v, own);
            if !targets.is_empty() {
                doc.relations.push(Relation::new(label, targets));
            }
        }
    }

    // Everything else → the metadata table, skipping keys mapped above.
    const MAPPED: [&str; 12] = [
        "eip", "erc", "bip", "caip", "title", "status", "created", "date", "description",
        "author", "authors", "requires",
    ];
    for (k, v) in &pre.fields {
        let kl = k.to_ascii_lowercase();
        if MAPPED.contains(&kl.as_str()) || kl == "replaces" || v.trim().is_empty() {
            continue;
        }
        doc.extra.push((titlecase_key(k), v.trim().to_string()));
    }
}

/// Parse an EIP-style author line: `Name (@github) <email>`, comma-separated.
fn parse_authors(s: &str) -> Vec<Author> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|entry| {
            let handle = extract_between(entry, '(', ')');
            let email = extract_between(entry, '<', '>');
            // The name is the text before the first '(' or '<'.
            let name_end = entry
                .find(['(', '<'])
                .unwrap_or(entry.len());
            let name = entry[..name_end].trim().to_string();
            let link = handle
                .filter(|h| h.starts_with('@'))
                .map(|h| format!("https://github.com/{}", h.trim_start_matches('@')))
                .or_else(|| email.map(|e| format!("mailto:{e}")));
            Author { name, organization: None, link }
        })
        .filter(|a| !a.name.is_empty())
        .collect()
}

fn extract_between(s: &str, open: char, close: char) -> Option<String> {
    let start = s.find(open)? + 1;
    let end = s[start..].find(close)? + start;
    Some(s[start..end].trim().to_string())
}

/// Parse a comma-separated id-number list (`"2718, 155"`) into same-collection
/// [`DocId`]s.
fn parse_id_list(s: &str, collection: Collection) -> Vec<DocId> {
    s.split(',')
        .filter_map(|p| p.trim().parse::<u32>().ok())
        .map(|n| DocId::new(collection, n))
        .collect()
}

fn titlecase_key(k: &str) -> String {
    // "discussions-to" -> "Discussions-To", "Layer" -> "Layer".
    k.split('-')
        .map(|part| {
            let mut ch = part.chars();
            match ch.next() {
                Some(f) => format!("{}{}", f.to_ascii_uppercase(), ch.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

// ---------------------------------------------------------------------------
// Block/inline helpers
// ---------------------------------------------------------------------------

/// A fenced code block: math fences → MathML, mermaid → a diagram (rendered as
/// its source until a diagram backend is wired in), a recognized language →
/// highlighted, otherwise verbatim.
fn code_block(info: &str, literal: &str) -> Block {
    let lang = info
        .split([' ', '\t', ','])
        .next()
        .unwrap_or("")
        .trim();
    let code = literal.strip_suffix('\n').unwrap_or(literal);

    if lang.eq_ignore_ascii_case("math") {
        return math_block(code);
    }
    if lang.eq_ignore_ascii_case("mermaid") {
        // No in-process mermaid backend yet: keep the source, render it as
        // artwork. The empty `svg` is the extension point for a real renderer.
        return Block::Diagram {
            svg: String::new(),
            source: code.to_string(),
        };
    }
    if !lang.is_empty() {
        if let Some(html) = crate::highlight::highlight(code, lang) {
            return Block::HighlightedCode { language: lang.to_string(), html };
        }
    }
    Block::Code {
        text: code.to_string(),
        language: (!lang.is_empty()).then(|| lang.to_string()),
    }
}

fn math_block(latex: &str) -> Block {
    match crate::mathml::latex_to_mathml(latex, true) {
        Some(mathml) => Block::Math { mathml, source: latex.to_string() },
        None => Block::Code { text: latex.to_string(), language: None },
    }
}

fn inline_math(latex: &str) -> Inline {
    match crate::mathml::latex_to_mathml(latex, false) {
        Some(mathml) => Inline::Math { mathml, source: latex.to_string() },
        None => Inline::Code(latex.to_string()),
    }
}

/// Best-effort handling of a raw HTML block: strip tags to text so nothing is
/// lost (we deliberately do not try to be a browser). Empty → dropped.
fn html_block(literal: &str) -> Option<Block> {
    let text = strip_tags(literal);
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(Block::Paragraph(vec![Inline::text(text)]))
    }
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
fn rewrite_doc_link(url: &str) -> Option<String> {
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
            s.blocks.iter().any(|b| matches!(b, Block::Diagram { svg, source }
                if svg.is_empty() && source.contains("A-->B")))
        });
        assert!(has, "mermaid should map to an (unrendered) Diagram carrying its source");
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
    fn strips_raw_html_block_to_text() {
        match html_block("<table><tr><td>hi</td><td>there</td></tr></table>") {
            Some(Block::Paragraph(inls)) => {
                assert!(matches!(inls.as_slice(), [Inline::Text(t)] if t == "hi there"));
            }
            other => panic!("expected a text paragraph, got {other:?}"),
        }
        assert!(html_block("<br/>").is_none());
    }
}
