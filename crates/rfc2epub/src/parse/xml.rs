//! Parser for canonical xml2rfc **v3** ([RFC 7991]) documents.
//!
//! Published RFC XML is "prepped": cross-references carry a `derivedContent`
//! attribute with their display text, which we use directly. Section numbers
//! are computed structurally (numeric in `<middle>`, letters for `<back>`
//! appendices) so we don't depend on optional part-number attributes.
//!
//! [RFC 7991]: https://www.rfc-editor.org/rfc/rfc7991

use roxmltree::{Document as XmlDoc, Node};

use crate::error::{Error, Result};
use crate::model::{
    Author, Block, Collection, DefEntry, DocId, Document, Inline, List, Relation, Section,
    SourceKind, Table,
};

pub fn parse(body: &str, number: Option<u32>) -> Result<Document> {
    let xml = XmlDoc::parse(body)?;
    let rfc = xml.root_element();
    if rfc.tag_name().name() != "rfc" {
        return Err(Error::Parse("root element is not <rfc>".into()));
    }

    let mut doc = Document {
        source: SourceKind::Xml,
        ..Default::default()
    };

    let num = rfc
        .attribute("number")
        .and_then(|s| s.parse().ok())
        .or(number);
    doc.id = num.map(|n| DocId::new(Collection::Rfc, n));
    let obsoletes = parse_num_list(rfc.attribute("obsoletes"));
    if !obsoletes.is_empty() {
        doc.relations.push(Relation::new("Obsoletes", rfc_ids(&obsoletes)));
    }
    let updates = parse_num_list(rfc.attribute("updates"));
    if !updates.is_empty() {
        doc.relations.push(Relation::new("Updates", rfc_ids(&updates)));
    }
    doc.status = rfc.attribute("category").map(map_category);

    if let Some(front) = child(rfc, "front") {
        parse_front(front, &mut doc);
    }
    if let Some(middle) = child(rfc, "middle") {
        number_sections(&mut doc.sections, middle, Numbering::Numeric, "");
    }
    if let Some(back) = child(rfc, "back") {
        parse_back(back, &mut doc);
    }

    Ok(doc)
}

fn parse_front(front: Node, doc: &mut Document) {
    if let Some(title) = child(front, "title") {
        doc.title = title.text().unwrap_or_default().trim().to_string();
        doc.short_title = title.attribute("abbrev").map(str::to_string);
    }
    for author in front.children().filter(|n| n.has_tag_name("author")) {
        let name = author
            .attribute("fullname")
            .map(str::to_string)
            .unwrap_or_default();
        let organization = child(author, "organization")
            .and_then(|n| n.text())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if !name.is_empty() || organization.is_some() {
            doc.authors.push(Author { name, organization, link: None });
        }
    }
    if let Some(date) = child(front, "date") {
        let month = month_name(date.attribute("month").unwrap_or(""));
        let year = date.attribute("year").unwrap_or("");
        let s = format!("{month} {year}").trim().to_string();
        if !s.is_empty() {
            doc.date = Some(s);
        }
    }
    if let Some(abs) = child(front, "abstract") {
        doc.abstract_ = parse_blocks(abs);
    }
    for kw in front.children().filter(|n| n.has_tag_name("keyword")) {
        if let Some(text) = kw.text() {
            let text = text.trim();
            if !text.is_empty() {
                doc.keywords.push(text.to_string());
            }
        }
    }
}

fn parse_back(back: Node, doc: &mut Document) {
    // References sections.
    for refs in back.children().filter(|n| n.has_tag_name("references")) {
        if let Some(section) = parse_references(refs) {
            doc.sections.push(section);
        }
    }
    // Appendices are <section> children of <back>, lettered A, B, C…
    let appendices: Vec<Node> = back
        .children()
        .filter(|n| n.has_tag_name("section"))
        .collect();
    number_sections_slice(&mut doc.sections, &appendices, Numbering::Alpha, "");
}

/// Turn a `<references>` element into a plain section whose body is a list of
/// bibliographic entries.
fn parse_references(refs: Node) -> Option<Section> {
    let title = child(refs, "name")
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "References".to_string());

    let mut items: Vec<DefEntry> = Vec::new();
    // References may be grouped (Normative / Informative) in nested <references>.
    collect_references(refs, &mut items);

    if items.is_empty() {
        return None;
    }
    Some(Section {
        number: None,
        id: refs
            .attribute("anchor")
            .map(str::to_string)
            .unwrap_or_else(|| "references".into()),
        title,
        blocks: vec![Block::DefinitionList(items)],
        subsections: Vec::new(),
    })
}

fn collect_references(refs: Node, items: &mut Vec<DefEntry>) {
    for r in refs.children().filter(|n| n.has_tag_name("reference")) {
        let anchor = r.attribute("anchor").unwrap_or("");
        let term = vec![Inline::text(format!("[{anchor}]"))];
        let id = (!anchor.is_empty()).then(|| anchor.to_string());
        let mut text = String::new();
        // Authors + title + seriesInfo, best-effort.
        if let Some(front) = child(r, "front") {
            if let Some(t) = child(front, "title").and_then(|n| n.text()) {
                text.push_str(t.trim());
            }
        }
        for si in r.descendants().filter(|n| n.has_tag_name("seriesInfo")) {
            if let (Some(name), Some(val)) =
                (si.attribute("name"), si.attribute("value"))
            {
                text.push_str(&format!(". {name} {val}"));
            }
        }
        let description = if let Some(target) = r.attribute("target") {
            vec![
                Block::Paragraph(vec![Inline::text(text)]),
                Block::Paragraph(vec![Inline::Link {
                    text: vec![Inline::text(target.to_string())],
                    href: target.to_string(),
                }]),
            ]
        } else {
            vec![Block::Paragraph(vec![Inline::text(text)])]
        };
        items.push(DefEntry { anchor: id, term, description });
    }
    // Recurse into nested <references> groups.
    for nested in refs.children().filter(|n| n.has_tag_name("references")) {
        collect_references(nested, items);
    }
}

#[derive(Clone, Copy)]
enum Numbering {
    Numeric,
    Alpha,
}

/// Number and parse the `<section>` children of `parent`.
fn number_sections(out: &mut Vec<Section>, parent: Node, style: Numbering, prefix: &str) {
    let sections: Vec<Node> = parent
        .children()
        .filter(|n| n.has_tag_name("section"))
        .collect();
    number_sections_slice(out, &sections, style, prefix);
}

fn number_sections_slice(
    out: &mut Vec<Section>,
    sections: &[Node],
    style: Numbering,
    prefix: &str,
) {
    for (i, &node) in sections.iter().enumerate() {
        let label = match style {
            Numbering::Numeric => (i + 1).to_string(),
            Numbering::Alpha => alpha_label(i),
        };
        let number = if prefix.is_empty() {
            label
        } else {
            format!("{prefix}.{label}")
        };
        out.push(parse_section(node, number));
    }
}

fn parse_section(node: Node, number: String) -> Section {
    let title = child(node, "name")
        .map(|n| n.text().unwrap_or_default().trim().to_string())
        .unwrap_or_default();
    let id = node
        .attribute("anchor")
        .map(str::to_string)
        .unwrap_or_else(|| format!("section-{number}"));

    let blocks = parse_blocks(node);

    let mut subsections = Vec::new();
    // Subsections are always numeric under their parent's prefix.
    number_sections(&mut subsections, node, Numbering::Numeric, &number);

    Section {
        number: Some(number),
        title,
        id,
        blocks,
        subsections,
    }
}

/// Parse the block-level children of `parent` (skipping nested `<section>`s and
/// front-matter-only elements).
fn parse_blocks(parent: Node) -> Vec<Block> {
    let mut blocks = Vec::new();
    for n in parent.children().filter(Node::is_element) {
        match n.tag_name().name() {
            "t" => blocks.push(Block::Paragraph(trim_inlines(parse_inlines(n)))),
            "artwork" => blocks.push(Block::Artwork(verbatim(n))),
            "sourcecode" => blocks.push(Block::Code {
                text: verbatim(n),
                language: n.attribute("type").map(str::to_string),
            }),
            "figure" => blocks.extend(parse_blocks(n)),
            "ul" => blocks.push(Block::List(parse_list(n, false))),
            "ol" => blocks.push(Block::List(parse_list(n, true))),
            "dl" => blocks.push(parse_dl(n)),
            "table" => blocks.push(Block::Table(parse_table(n))),
            "aside" => blocks.push(Block::Aside(parse_blocks(n))),
            "blockquote" => blocks.push(Block::Quote(parse_blocks(n))),
            // name/section/iref/cref handled elsewhere or intentionally ignored.
            _ => {}
        }
    }
    blocks
}

fn parse_list(node: Node, ordered: bool) -> List {
    let items = node
        .children()
        .filter(|n| n.has_tag_name("li"))
        .map(|li| {
            // An <li> may hold block children or bare inline content.
            if li.children().any(|c| c.is_element() && is_block_tag(c)) {
                parse_blocks(li)
            } else {
                vec![Block::Paragraph(trim_inlines(parse_inlines(li)))]
            }
        })
        .collect();
    List { ordered, items }
}

fn parse_dl(node: Node) -> Block {
    let mut items = Vec::new();
    let mut pending_term: Option<Vec<Inline>> = None;
    for child in node.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "dt" => pending_term = Some(parse_inlines(child)),
            "dd" => {
                let term = pending_term.take().unwrap_or_default();
                items.push(DefEntry {
                    anchor: None,
                    term,
                    description: parse_blocks_or_inline(child),
                });
            }
            _ => {}
        }
    }
    Block::DefinitionList(items)
}

fn parse_blocks_or_inline(node: Node) -> Vec<Block> {
    if node.children().any(|c| c.is_element() && is_block_tag(c)) {
        parse_blocks(node)
    } else {
        vec![Block::Paragraph(trim_inlines(parse_inlines(node)))]
    }
}

fn parse_table(node: Node) -> Table {
    let mut table = Table::default();
    for thead in node.children().filter(|n| n.has_tag_name("thead")) {
        for tr in thead.children().filter(|n| n.has_tag_name("tr")) {
            table.head = tr
                .children()
                .filter(|c| c.has_tag_name("th") || c.has_tag_name("td"))
                .map(parse_inlines)
                .collect();
        }
    }
    for tbody in node.children().filter(|n| n.has_tag_name("tbody")) {
        for tr in tbody.children().filter(|n| n.has_tag_name("tr")) {
            let row = tr
                .children()
                .filter(|c| c.has_tag_name("td") || c.has_tag_name("th"))
                .map(parse_inlines)
                .collect();
            table.rows.push(row);
        }
    }
    table
}

fn parse_inlines(node: Node) -> Vec<Inline> {
    let mut out = Vec::new();
    for c in node.children() {
        if c.is_text() {
            if let Some(t) = c.text() {
                push_text(&mut out, t);
            }
        } else if c.is_element() {
            match c.tag_name().name() {
                "xref" => {
                    let text = xref_text(c);
                    out.push(Inline::XRef {
                        text,
                        target: c.attribute("target").unwrap_or("").to_string(),
                    });
                }
                "eref" => {
                    let href = c.attribute("target").unwrap_or("").to_string();
                    let text = xref_text(c);
                    let text = if text.is_empty() {
                        vec![Inline::text(href.clone())]
                    } else {
                        text
                    };
                    out.push(Inline::Link { text, href });
                }
                "strong" | "b" => out.push(Inline::Strong(parse_inlines(c))),
                "bcp14" => out.push(Inline::Strong(parse_inlines(c))),
                "em" | "i" => out.push(Inline::Emph(parse_inlines(c))),
                "tt" => out.push(Inline::Code(collect_text(c))),
                "sub" | "sup" | "spanx" => out.extend(parse_inlines(c)),
                // iref/cref and unknowns: drop the tag, keep any text.
                _ => out.extend(parse_inlines(c)),
            }
        }
    }
    out
}

/// Display text for an `<xref>`/`<eref>`: prefer explicit body text, else the
/// prepped `derivedContent` attribute.
fn xref_text(node: Node) -> Vec<Inline> {
    let inner = parse_inlines(node);
    if !inner.is_empty() {
        return inner;
    }
    if let Some(dc) = node.attribute("derivedContent") {
        if !dc.trim().is_empty() {
            return vec![Inline::text(dc.to_string())];
        }
    }
    Vec::new()
}

/// Collapse an element's whitespace-runs into single spaces while appending to
/// an inline vector.
fn push_text(out: &mut Vec<Inline>, raw: &str) {
    let collapsed = collapse_ws(raw);
    if !collapsed.is_empty() {
        out.push(Inline::Text(collapsed));
    }
}

fn collect_text(node: Node) -> String {
    let mut s = String::new();
    for d in node.descendants() {
        if d.is_text() {
            s.push_str(d.text().unwrap_or(""));
        }
    }
    collapse_ws(&s)
}

/// Verbatim text of an artwork/sourcecode block, trimmed of the leading/trailing
/// newlines that xml2rfc adds around the content, but otherwise byte-preserved.
fn verbatim(node: Node) -> String {
    let mut s = String::new();
    for d in node.descendants() {
        if d.is_text() {
            s.push_str(d.text().unwrap_or(""));
        }
    }
    s.trim_matches('\n').trim_end().to_string()
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::new();
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out
}

/// Trim a leading space from the first text run and a trailing space from the
/// last, so paragraphs don't start/end with the whitespace xml2rfc leaves
/// around indented `<t>` content.
fn trim_inlines(mut inlines: Vec<Inline>) -> Vec<Inline> {
    if let Some(Inline::Text(t)) = inlines.first_mut() {
        let trimmed = t.trim_start().to_string();
        if trimmed.is_empty() {
            inlines.remove(0);
        } else {
            *t = trimmed;
        }
    }
    if let Some(Inline::Text(t)) = inlines.last_mut() {
        let trimmed = t.trim_end().to_string();
        if trimmed.is_empty() {
            inlines.pop();
        } else {
            *t = trimmed;
        }
    }
    inlines
}

fn month_name(m: &str) -> &str {
    match m.trim() {
        "1" | "01" => "January",
        "2" | "02" => "February",
        "3" | "03" => "March",
        "4" | "04" => "April",
        "5" | "05" => "May",
        "6" | "06" => "June",
        "7" | "07" => "July",
        "8" | "08" => "August",
        "9" | "09" => "September",
        "10" => "October",
        "11" => "November",
        "12" => "December",
        other => other,
    }
}

fn is_block_tag(n: Node) -> bool {
    matches!(
        n.tag_name().name(),
        "t" | "ul"
            | "ol"
            | "dl"
            | "figure"
            | "artwork"
            | "sourcecode"
            | "table"
            | "aside"
            | "blockquote"
    )
}

fn child<'a, 'input>(node: Node<'a, 'input>, tag: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|n| n.has_tag_name(tag))
}

/// Wrap RFC numbers as same-collection [`DocId`]s for a [`Relation`].
fn rfc_ids(nums: &[u32]) -> Vec<DocId> {
    nums.iter().map(|&n| DocId::new(Collection::Rfc, n)).collect()
}

fn parse_num_list(attr: Option<&str>) -> Vec<u32> {
    attr.map(|s| {
        s.split(',')
            .filter_map(|p| p.trim().parse().ok())
            .collect()
    })
    .unwrap_or_default()
}

fn map_category(code: &str) -> String {
    match code {
        "std" => "Standards Track",
        "bcp" => "Best Current Practice",
        "info" => "Informational",
        "exp" => "Experimental",
        "historic" => "Historic",
        other => other,
    }
    .to_string()
}

/// A, B, …, Z, AA, AB, … for appendix labels.
fn alpha_label(i: usize) -> String {
    let mut n = i;
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (n % 26) as u8) as char);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    s
}
