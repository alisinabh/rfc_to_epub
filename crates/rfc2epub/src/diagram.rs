//! Fill rendered SVG into [`Block::Diagram`] nodes (mermaid) as a post-parse
//! transform.
//!
//! The Markdown parser maps a ` ```mermaid ` fence to
//! `Block::Diagram { svg: "", source }`; this pass walks the IR and renders each
//! empty diagram to an SVG **in process** with [`merman`] (a pure-Rust,
//! parity-focused Mermaid engine), using its *resvg-safe* pipeline so the output
//! carries no `<foreignObject>` — which Kindle- and `resvg`-class readers drop.
//! A diagram that fails to render (or slips a `<foreignObject>` through) is left
//! with an empty `svg`, so the renderer falls back to showing the diagram's
//! source verbatim: **a diagram never fails the build.**
//!
//! In-process rendering is behind the default `mermaid` Cargo feature. The
//! source is first normalized ([`normalize_mermaid`]) to shed authoring quirks
//! (a `%%` comment before the diagram-type line breaks mermaid). When `online`
//! is set, a diagram merman can't handle (e.g. its sequence-diagram parser
//! rejects actor names with spaces, which real mermaid accepts) falls back to
//! the **Kroki** service — opt-in because it sends the diagram to a third party.
//! Anything still unrendered falls back to the verbatim source: a diagram never
//! fails the build. Kept out of the pure `parse_source` path so parsing stays a
//! self-contained transform; it runs from [`crate::convert`] and the CLI's
//! local-file path instead.

use crate::model::{Block, Document, Section};

/// Render every not-yet-rendered [`Block::Diagram`] in `doc` to SVG in place.
/// `online` enables the opt-in Kroki fallback for diagrams the in-process engine
/// (merman) can't render.
pub fn resolve(doc: &mut Document, online: bool) {
    let mut counter = 0usize;
    for section in &mut doc.sections {
        walk_section(section, &mut counter, online);
    }
    walk_blocks(&mut doc.abstract_, &mut counter, online);
}

fn walk_section(section: &mut Section, counter: &mut usize, online: bool) {
    walk_blocks(&mut section.blocks, counter, online);
    for sub in &mut section.subsections {
        walk_section(sub, counter, online);
    }
}

fn walk_blocks(blocks: &mut [Block], counter: &mut usize, online: bool) {
    for block in blocks {
        match block {
            Block::Diagram { svg, source } if svg.trim().is_empty() => {
                *counter += 1;
                if let Some(rendered) = render_diagram(source, *counter, online) {
                    *svg = rendered;
                }
            }
            Block::List(list) => {
                for item in &mut list.items {
                    walk_blocks(item, counter, online);
                }
            }
            Block::DefinitionList(entries) => {
                for entry in entries {
                    walk_blocks(&mut entry.description, counter, online);
                }
            }
            Block::Aside(inner) | Block::Quote(inner) => walk_blocks(inner, counter, online),
            _ => {}
        }
    }
}

/// Render one mermaid `source` to an inline SVG string, or `None` to fall back
/// to the verbatim source. Tries the in-process engine (merman) first, then —
/// only when `online` is set — the real-mermaid Kroki service, which covers
/// merman's parity gaps (e.g. sequence-diagram actor names containing spaces).
fn render_diagram(source: &str, index: usize, online: bool) -> Option<String> {
    let normalized = normalize_mermaid(source);
    if let Some(svg) = render_merman(&normalized, index) {
        return Some(svg);
    }
    if online {
        return render_kroki(&normalized);
    }
    None
}

/// In-process render via merman (only with the `mermaid` feature), using its
/// resvg-safe pipeline.
#[cfg(feature = "mermaid")]
fn render_merman(source: &str, index: usize) -> Option<String> {
    use merman::render::HeadlessRenderer;

    let svg = HeadlessRenderer::new()
        .with_diagram_id(&format!("rfc2epub-diagram-{index}"))
        .render_svg_resvg_safe_sync(source)
        .ok()??;
    accept_svg(&svg)
}

#[cfg(not(feature = "mermaid"))]
fn render_merman(_source: &str, _index: usize) -> Option<String> {
    None
}

/// Network fallback: POST the diagram to Kroki's Mermaid renderer (real
/// mermaid.js) and use the returned SVG. `None` on any failure so the build
/// never breaks over a diagram.
fn render_kroki(source: &str) -> Option<String> {
    let body = crate::fetch::http_post_text("https://kroki.io/mermaid/svg", source)?;
    accept_svg(&body)
}

/// Extract an embeddable `<svg>` fragment and enforce the `<foreignObject>`
/// guardrail (those labels vanish on Kindle / in resvg), returning `None` if the
/// output is unusable so the caller falls back to the verbatim source.
fn accept_svg(raw: &str) -> Option<String> {
    let svg = svg_fragment(raw)?;
    if svg.contains("<foreignObject") {
        return None;
    }
    Some(svg)
}

/// Normalize a mermaid source so common authoring quirks don't defeat the
/// parser. Chiefly: drop full-line `%%` comments — mermaid (merman *and* real
/// mermaid.js) reject a comment that appears *before* the diagram-type
/// declaration, which ERC-5883 has. `%%{init}%%` directives are kept, since they
/// are meaningful and legal there.
fn normalize_mermaid(source: &str) -> String {
    source
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            // Keep everything except a full-line plain `%%` comment.
            !t.starts_with("%%") || t.starts_with("%%{")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Trim any XML prolog / doctype so the SVG can be embedded inline in an XHTML
/// content document (which is itself an XML document). Returns `None` if there
/// is no `<svg` element.
fn svg_fragment(svg: &str) -> Option<String> {
    let start = svg.find("<svg")?;
    Some(svg[start..].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Block;

    fn diagram_doc() -> Document {
        let mut doc = Document::default();
        doc.sections.push(Section {
            number: None,
            title: "D".into(),
            id: "d".into(),
            blocks: vec![Block::Diagram {
                svg: String::new(),
                source: "flowchart TD\nA[Start] --> B[Done]".into(),
            }],
            subsections: Vec::new(),
        });
        doc
    }

    fn diagram_svg(doc: &Document) -> &str {
        match &doc.sections[0].blocks[0] {
            Block::Diagram { svg, .. } => svg,
            _ => unreachable!(),
        }
    }

    #[cfg(feature = "mermaid")]
    #[test]
    fn renders_mermaid_to_foreignobject_free_svg() {
        let mut doc = diagram_doc();
        resolve(&mut doc, false); // in-process only, no network
        let svg = diagram_svg(&doc);
        assert!(
            svg.starts_with("<svg"),
            "svg filled and prolog-trimmed: {svg:.40}"
        );
        assert!(
            !svg.contains("<foreignObject"),
            "resvg-safe output has no foreignObject"
        );
    }

    #[cfg(not(feature = "mermaid"))]
    #[test]
    fn without_feature_leaves_source_fallback() {
        let mut doc = diagram_doc();
        resolve(&mut doc, false);
        assert!(
            diagram_svg(&doc).is_empty(),
            "no rendering without the mermaid feature (and no online fallback)"
        );
    }

    #[test]
    fn normalize_strips_leading_comment_keeps_init_directive() {
        // A `%%` comment before the diagram type breaks mermaid; it must go.
        let out = normalize_mermaid("%% a note\n sequenceDiagram\n A->>B: hi");
        assert!(!out.contains("%% a note"));
        assert!(out.contains("sequenceDiagram"));
        // `%%{init}%%` directives are meaningful and kept.
        let out2 = normalize_mermaid("%%{init: {\"theme\":\"dark\"}}%%\nflowchart TD\n A-->B");
        assert!(out2.contains("%%{init"));
    }
}
