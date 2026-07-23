//! Assemble a [`Document`] into an EPUB using `epub-builder`.
//!
//! Structure: a title/cover page, an auto-generated table of contents, then one
//! content document per top-level section (which keeps the nav tidy and lets
//! e-readers jump between sections).

use epub_builder::{EpubBuilder, EpubContent, ReferenceType, TocElement, ZipLibrary};

use super::svg::Figures;
use super::xhtml::{Anchors, Ctx};
use super::{cover, css, xhtml};
use crate::error::Result;
use crate::model::{Block, Document, Section, SvgMode};

/// Filename of the content document for top-level section `index`.
fn section_file(index: usize) -> String {
    format!("s{index:03}.xhtml")
}

pub fn build(doc: &Document, svg_mode: SvgMode) -> Result<Vec<u8>> {
    let mut builder = EpubBuilder::new(ZipLibrary::new()?)?;

    builder
        .metadata("title", book_title(doc))?
        .metadata("lang", "en")?
        .metadata(
            "generator",
            concat!("rfc2epub ", env!("CARGO_PKG_VERSION")),
        )?;
    for author in &doc.authors {
        builder.metadata("author", &author.name)?;
    }
    let description = xhtml::plain_text(&doc.abstract_);
    if !description.is_empty() {
        builder.metadata("description", description)?;
    }
    for keyword in &doc.keywords {
        builder.metadata("subject", keyword)?;
    }
    if let Some(n) = doc.number {
        // Deterministic, stable book id derived from the RFC's canonical URN.
        let urn = format!("urn:ietf:rfc:{n}");
        let uuid = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, urn.as_bytes());
        builder.set_uuid(uuid);
    }

    builder.stylesheet(css::STYLESHEET.as_bytes())?;

    // Cover: rasterized PNG for the shelf thumbnail plus a full-page cover
    // document as the first thing the reader sees. Skipped if rendering fails.
    if let Some(png) = cover::cover_png(doc) {
        builder.add_cover_image("cover.png", png.as_slice(), "image/png")?;
        builder.add_content(
            EpubContent::new("cover.xhtml", xhtml::cover_page("cover.png").as_bytes())
                .title("Cover")
                .reftype(ReferenceType::Cover),
        )?;
    }

    // In Card mode, diagrams are rendered to SVG image files referenced from the
    // content documents; collect them here and write them as resources
    // afterwards. In Inline mode the collector stays empty.
    let mut figs = Figures::new();
    // Doc-wide map of anchor id -> content file, so cross-references resolve to
    // working hyperlinks across the per-section files.
    let anchors = build_anchors(doc);
    let mut ctx = Ctx {
        mode: svg_mode,
        figs: &mut figs,
        anchors: &anchors,
    };

    // Title page.
    let title_html = xhtml::titlepage(doc, &mut ctx);
    builder.add_content(
        EpubContent::new("title.xhtml", title_html.as_bytes())
            .title("Title")
            .reftype(ReferenceType::TitlePage),
    )?;

    // Auto-generated inline TOC, placed after the title page.
    builder.inline_toc();

    // One page per top-level section, each carrying its subsection tree as
    // nested TOC entries so the nav mirrors the document's full hierarchy.
    for (i, section) in doc.sections.iter().enumerate() {
        let html = xhtml::section_page(section, &mut ctx);
        let filename = section_file(i);
        let mut content = EpubContent::new(&filename, html.as_bytes())
            .title(toc_title(section))
            .reftype(ReferenceType::Text);
        for child in toc_children(section, &filename) {
            content = content.child(child);
        }
        builder.add_content(content)?;
    }

    // Write each generated SVG figure as an image resource (Card mode only).
    for (path, svg) in &figs.items {
        builder.add_resource(path, svg.as_bytes(), "image/svg+xml")?;
    }

    let mut out = Vec::new();
    builder.generate(&mut out)?;
    Ok(out)
}

fn book_title(doc: &Document) -> String {
    match doc.number {
        Some(n) => format!("RFC {n}: {}", doc.title),
        None => doc.title.clone(),
    }
}

fn toc_title(section: &Section) -> String {
    match &section.number {
        Some(n) => format!("{n}. {}", section.title),
        None => section.title.clone(),
    }
}

/// Nested TOC entries for a top-level section's descendants, all pointing at
/// in-file anchors within `file`.
fn toc_children(section: &Section, file: &str) -> Vec<TocElement> {
    section
        .subsections
        .iter()
        .map(|sub| {
            let mut el = TocElement::new(format!("{file}#{}", sub.id), toc_title(sub));
            for grandchild in toc_children(sub, file) {
                el = el.child(grandchild);
            }
            el
        })
        .collect()
}

/// Build the doc-wide anchor map (anchor id -> content file). Every section id
/// (at any depth) and every bibliography-entry anchor is registered against the
/// top-level section file it lives in.
fn build_anchors(doc: &Document) -> Anchors {
    let mut map = Anchors::new();
    for (i, section) in doc.sections.iter().enumerate() {
        let file = section_file(i);
        register_section(section, &file, &mut map);
    }
    map
}

fn register_section(section: &Section, file: &str, map: &mut Anchors) {
    map.insert(section.id.clone(), file.to_string());
    for block in &section.blocks {
        register_block(block, file, map);
    }
    for sub in &section.subsections {
        register_section(sub, file, map);
    }
}

fn register_block(block: &Block, file: &str, map: &mut Anchors) {
    match block {
        Block::DefinitionList(items) => {
            for entry in items {
                if let Some(anchor) = &entry.anchor {
                    map.insert(anchor.clone(), file.to_string());
                }
                for b in &entry.description {
                    register_block(b, file, map);
                }
            }
        }
        Block::List(list) => {
            for item in &list.items {
                for b in item {
                    register_block(b, file, map);
                }
            }
        }
        Block::Aside(blocks) | Block::Quote(blocks) => {
            for b in blocks {
                register_block(b, file, map);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DefEntry;

    fn section(number: &str, id: &str, subs: Vec<Section>) -> Section {
        Section {
            number: Some(number.into()),
            title: format!("Section {number}"),
            id: id.into(),
            blocks: Vec::new(),
            subsections: subs,
        }
    }

    #[test]
    fn anchors_map_sections_and_refs_to_their_files() {
        let mut doc = Document::default();
        // s000: section "1" with nested "1.1".
        doc.sections.push(section("1", "intro", vec![section("1.1", "purpose", vec![])]));
        // s001: a references section carrying a bibliography anchor.
        let mut refs = section("2", "references", vec![]);
        refs.blocks.push(Block::DefinitionList(vec![DefEntry {
            anchor: Some("RFC2119".into()),
            term: Vec::new(),
            description: Vec::new(),
        }]));
        doc.sections.push(refs);

        let map = build_anchors(&doc);
        assert_eq!(map.get("intro").map(String::as_str), Some("s000.xhtml"));
        assert_eq!(map.get("purpose").map(String::as_str), Some("s000.xhtml"));
        assert_eq!(map.get("references").map(String::as_str), Some("s001.xhtml"));
        assert_eq!(map.get("RFC2119").map(String::as_str), Some("s001.xhtml"));
    }

    #[test]
    fn toc_children_nest_and_point_at_in_file_anchors() {
        let sec = section("1", "intro", vec![section("1.1", "purpose", vec![section("1.1.1", "deep", vec![])])]);
        let children = toc_children(&sec, "s000.xhtml");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].url, "s000.xhtml#purpose");
        assert_eq!(children[0].title, "1.1. Section 1.1");
        // Grandchild present and nested one level deeper.
        assert_eq!(children[0].children.len(), 1);
        assert_eq!(children[0].children[0].url, "s000.xhtml#deep");
    }
}
