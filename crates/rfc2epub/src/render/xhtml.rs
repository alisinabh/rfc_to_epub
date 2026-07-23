//! Render IR nodes to XHTML (EPUB content documents must be well-formed XML).

use std::collections::HashMap;
use std::fmt::Write;

use html_escape::{encode_double_quoted_attribute, encode_text};

use super::svg::{card_svg, inline_svg, Figures};
use crate::model::{Block, Document, Inline, List, Section, SvgMode, Table};

/// Maps an anchor id to the content file that contains it, so an
/// [`Inline::XRef`] can be resolved to a working in-book hyperlink.
pub type Anchors = HashMap<String, String>;

/// Mutable state threaded through rendering: the diagram theme mode, the figure
/// collector (populated only in [`SvgMode::Card`]), and the doc-wide anchor map
/// used to resolve cross-references across content files.
pub struct Ctx<'a> {
    pub mode: SvgMode,
    pub figs: &'a mut Figures,
    pub anchors: &'a Anchors,
    /// Whether to honour [`Block::PageBreak`] markers from the original
    /// pagination. When false, they render as nothing.
    pub page_breaks: bool,
}

/// Wrap page `inner` in a complete, well-formed XHTML document.
pub fn page(title: &str, inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" xml:lang="en" lang="en">
<head>
<meta charset="utf-8"/>
<title>{title}</title>
<link rel="stylesheet" type="text/css" href="style.css"/>
</head>
<body>
{inner}
</body>
</html>"#,
        title = encode_text(title),
        inner = inner,
    )
}

/// The title / cover page for the book.
pub fn titlepage(doc: &Document, ctx: &mut Ctx) -> String {
    let mut s = String::new();
    s.push_str("<section class=\"titlepage\" epub:type=\"titlepage\">\n");
    if let Some(n) = doc.number {
        let _ = writeln!(s, "<p class=\"rfc-number\">RFC {n}</p>");
    }
    let _ = writeln!(s, "<h1>{}</h1>", encode_text(&doc.title));

    s.push_str("<div class=\"meta\">\n");
    if let Some(cat) = &doc.category {
        let _ = writeln!(s, "<p>{}</p>", encode_text(cat));
    }
    if !doc.authors.is_empty() {
        let names: Vec<String> = doc
            .authors
            .iter()
            .map(|a| encode_text(&a.name).to_string())
            .collect();
        let _ = writeln!(s, "<p>{}</p>", names.join(", "));
    }
    if let Some(date) = &doc.date {
        let _ = writeln!(s, "<p>{}</p>", encode_text(date));
    }
    if !doc.obsoletes.is_empty() {
        let _ = writeln!(s, "<p>Obsoletes: {}</p>", join_nums(&doc.obsoletes));
    }
    if !doc.updates.is_empty() {
        let _ = writeln!(s, "<p>Updates: {}</p>", join_nums(&doc.updates));
    }
    s.push_str("</div>\n");

    if !doc.abstract_.is_empty() {
        s.push_str("<div class=\"abstract\">\n<h2>Abstract</h2>\n");
        render_blocks(&doc.abstract_, &mut s, ctx);
        s.push_str("</div>\n");
    }

    s.push_str("<p class=\"colophon\">Converted from the ");
    s.push_str(match doc.source {
        crate::model::SourceKind::Xml => "canonical xml2rfc source",
        crate::model::SourceKind::Text => "published plain-text rendering",
        crate::model::SourceKind::Unknown => "source",
    });
    s.push_str(" by rfc2epub.</p>\n");
    s.push_str("</section>\n");

    page(&doc.title, &s)
}

/// Render one top-level section (with its whole subtree) as a chapter page.
pub fn section_page(section: &Section, ctx: &mut Ctx) -> String {
    let mut s = String::new();
    render_section(section, 1, &mut s, ctx);
    page(&display_heading(section), &s)
}

/// A full-bleed cover page displaying the cover image.
pub fn cover_page(image_href: &str) -> String {
    let inner = format!(
        "<div class=\"cover\"><img src=\"{}\" alt=\"Cover\"/></div>",
        encode_double_quoted_attribute(image_href),
    );
    page("Cover", &inner)
}

/// Flatten block content to plain text (used for `<dc:description>`).
pub fn plain_text(blocks: &[Block]) -> String {
    let mut parts = Vec::new();
    for b in blocks {
        if let Block::Paragraph(inlines) = b {
            let mut s = String::new();
            collect_inline_text(inlines, &mut s);
            let s = s.trim();
            if !s.is_empty() {
                parts.push(s.to_string());
            }
        }
    }
    parts.join("\n\n")
}

fn collect_inline_text(inlines: &[Inline], out: &mut String) {
    for i in inlines {
        match i {
            Inline::Text(t) => out.push_str(t),
            Inline::Code(t) => out.push_str(t),
            Inline::Emph(inner)
            | Inline::Strong(inner)
            | Inline::Link { text: inner, .. }
            | Inline::XRef { text: inner, .. } => collect_inline_text(inner, out),
        }
    }
}

/// Heading text as shown, e.g. `"3.2. Overview"`.
fn display_heading(section: &Section) -> String {
    match &section.number {
        Some(n) => format!("{n}. {}", section.title),
        None => section.title.clone(),
    }
}

fn render_section(section: &Section, level: usize, out: &mut String, ctx: &mut Ctx) {
    let h = level.clamp(1, 6);
    let _ = write!(
        out,
        "<section id=\"{}\">\n<h{h}>{}</h{h}>\n",
        encode_double_quoted_attribute(&section.id),
        encode_text(&display_heading(section)),
    );
    render_blocks(&section.blocks, out, ctx);
    for sub in &section.subsections {
        render_section(sub, level + 1, out, ctx);
    }
    out.push_str("</section>\n");
}

fn render_blocks(blocks: &[Block], out: &mut String, ctx: &mut Ctx) {
    for b in blocks {
        render_block(b, out, ctx);
    }
}

fn render_block(block: &Block, out: &mut String, ctx: &mut Ctx) {
    match block {
        Block::Paragraph(inlines) => {
            out.push_str("<p>");
            render_inlines(inlines, out, ctx.anchors);
            out.push_str("</p>\n");
        }
        Block::Artwork(text) => emit_figure(text, "artwork", out, ctx),
        Block::Code { text, language } => {
            let cls = match language {
                Some(l) => format!("sourcecode language-{}", sanitize_class(l)),
                None => "sourcecode".to_string(),
            };
            emit_figure(text, &cls, out, ctx);
        }
        Block::List(list) => render_list(list, out, ctx),
        Block::DefinitionList(items) => {
            out.push_str("<dl>\n");
            for entry in items {
                match &entry.anchor {
                    Some(id) => {
                        let _ = write!(
                            out,
                            "<dt id=\"{}\">",
                            encode_double_quoted_attribute(id)
                        );
                    }
                    None => out.push_str("<dt>"),
                }
                render_inlines(&entry.term, out, ctx.anchors);
                out.push_str("</dt>\n<dd>");
                render_blocks(&entry.description, out, ctx);
                out.push_str("</dd>\n");
            }
            out.push_str("</dl>\n");
        }
        Block::Table(table) => render_table(table, out, ctx.anchors),
        Block::Aside(blocks) => {
            out.push_str("<aside>\n");
            render_blocks(blocks, out, ctx);
            out.push_str("</aside>\n");
        }
        Block::Quote(blocks) => {
            out.push_str("<blockquote>\n");
            render_blocks(blocks, out, ctx);
            out.push_str("</blockquote>\n");
        }
        Block::PageBreak => {
            if ctx.page_breaks {
                out.push_str("<div class=\"page-break\"></div>\n");
            }
        }
    }
}

/// Emit a monospace block as a scalable SVG so wide diagrams fit narrow screens
/// without wrapping. In [`SvgMode::Card`] the SVG is written as a referenced
/// image (its own light card); in [`SvgMode::Inline`] it is embedded inline and
/// follows the reader's theme via `currentColor`.
fn emit_figure(text: &str, class: &str, out: &mut String, ctx: &mut Ctx) {
    match ctx.mode {
        SvgMode::Card => {
            let Some(svg) = card_svg(text) else {
                return;
            };
            let href = ctx.figs.add(svg);
            let _ = writeln!(
                out,
                "<figure class=\"{}\"><img src=\"{}\" alt=\"diagram\"/></figure>",
                class,
                encode_double_quoted_attribute(&href),
            );
        }
        SvgMode::Inline => {
            let Some(svg) = inline_svg(text) else {
                return;
            };
            let _ = writeln!(out, "<figure class=\"{class}\">{svg}</figure>");
        }
    }
}

fn render_list(list: &List, out: &mut String, ctx: &mut Ctx) {
    let tag = if list.ordered { "ol" } else { "ul" };
    let _ = writeln!(out, "<{tag}>");
    for item in &list.items {
        out.push_str("<li>");
        // Unwrap a single paragraph so list items read tightly.
        if let [Block::Paragraph(inlines)] = item.as_slice() {
            render_inlines(inlines, out, ctx.anchors);
        } else {
            render_blocks(item, out, ctx);
        }
        out.push_str("</li>\n");
    }
    let _ = writeln!(out, "</{tag}>");
}

fn render_table(table: &Table, out: &mut String, anchors: &Anchors) {
    out.push_str("<table>\n");
    if !table.head.is_empty() {
        out.push_str("<thead>\n<tr>");
        for cell in &table.head {
            out.push_str("<th>");
            render_inlines(cell, out, anchors);
            out.push_str("</th>");
        }
        out.push_str("</tr>\n</thead>\n");
    }
    out.push_str("<tbody>\n");
    for row in &table.rows {
        out.push_str("<tr>");
        for cell in row {
            out.push_str("<td>");
            render_inlines(cell, out, anchors);
            out.push_str("</td>");
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</tbody>\n</table>\n");
}

fn render_inlines(inlines: &[Inline], out: &mut String, anchors: &Anchors) {
    for i in inlines {
        render_inline(i, out, anchors);
    }
}

fn render_inline(inline: &Inline, out: &mut String, anchors: &Anchors) {
    match inline {
        Inline::Text(t) => out.push_str(&encode_text(t)),
        Inline::Emph(inner) => {
            out.push_str("<em>");
            render_inlines(inner, out, anchors);
            out.push_str("</em>");
        }
        Inline::Strong(inner) => {
            out.push_str("<strong>");
            render_inlines(inner, out, anchors);
            out.push_str("</strong>");
        }
        Inline::Code(t) => {
            let _ = write!(out, "<code>{}</code>", encode_text(t));
        }
        Inline::Link { text, href } => {
            let _ = write!(
                out,
                "<a href=\"{}\">",
                encode_double_quoted_attribute(href)
            );
            render_inlines(text, out, anchors);
            out.push_str("</a>");
        }
        Inline::XRef { text, target } => {
            // Resolve to an in-book hyperlink when the target anchor exists in
            // this document; otherwise fall back to plain display text.
            if let Some(file) = anchors.get(target) {
                let _ = write!(
                    out,
                    "<a href=\"{}\">",
                    encode_double_quoted_attribute(&format!("{file}#{target}"))
                );
                render_inlines(text, out, anchors);
                out.push_str("</a>");
            } else {
                render_inlines(text, out, anchors);
            }
        }
    }
}

fn join_nums(nums: &[u32]) -> String {
    nums.iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn sanitize_class(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(inline: &Inline, anchors: &Anchors) -> String {
        let mut out = String::new();
        render_inline(inline, &mut out, anchors);
        out
    }

    #[test]
    fn xref_resolves_to_in_book_link_when_target_known() {
        let mut anchors = Anchors::new();
        anchors.insert("purpose".into(), "s000.xhtml".into());
        let xref = Inline::XRef {
            text: vec![Inline::text("Section 1.1")],
            target: "purpose".into(),
        };
        assert_eq!(
            render(&xref, &anchors),
            "<a href=\"s000.xhtml#purpose\">Section 1.1</a>"
        );
    }

    #[test]
    fn xref_falls_back_to_plain_text_when_target_unknown() {
        let anchors = Anchors::new();
        let xref = Inline::XRef {
            text: vec![Inline::text("Section 2")],
            target: "missing".into(),
        };
        assert_eq!(render(&xref, &anchors), "Section 2");
    }

    fn render_pagebreak(page_breaks: bool) -> String {
        let anchors = Anchors::new();
        let mut figs = Figures::new();
        let mut ctx = Ctx {
            mode: SvgMode::Inline,
            figs: &mut figs,
            anchors: &anchors,
            page_breaks,
        };
        let mut out = String::new();
        render_block(&Block::PageBreak, &mut out, &mut ctx);
        out
    }

    #[test]
    fn page_break_emitted_only_when_enabled() {
        assert!(render_pagebreak(true).contains("class=\"page-break\""));
        assert_eq!(render_pagebreak(false), "");
    }
}
