//! Assemble a [`Document`] into an EPUB using `epub-builder`.
//!
//! Structure: a title/cover page, an auto-generated table of contents, then one
//! content document per top-level section (which keeps the nav tidy and lets
//! e-readers jump between sections).

use epub_builder::{EpubBuilder, EpubContent, ReferenceType, ZipLibrary};

use super::svg::Figures;
use super::xhtml::Ctx;
use super::{cover, css, xhtml};
use crate::error::Result;
use crate::model::{Document, Section, SvgMode};

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
    let mut ctx = Ctx {
        mode: svg_mode,
        figs: &mut figs,
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

    // One page per top-level section.
    for (i, section) in doc.sections.iter().enumerate() {
        let html = xhtml::section_page(section, &mut ctx);
        let filename = format!("s{i:03}.xhtml");
        builder.add_content(
            EpubContent::new(&filename, html.as_bytes())
                .title(toc_title(section))
                .reftype(ReferenceType::Text),
        )?;
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
