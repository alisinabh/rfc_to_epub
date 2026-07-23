//! Render verbatim monospace blocks (ASCII art, packet diagrams, code) as
//! standalone SVG images so they scale to fit narrow e-reader screens instead
//! of wrapping.
//!
//! The problem: RFC artwork is up to 72 monospace columns wide. At an e-reader's
//! default font size those columns don't fit a phone-sized screen, and reading
//! systems (notably Kindle) ignore `overflow-x` on `<pre>`, so the diagram gets
//! soft-wrapped into nonsense.
//!
//! The fix: lay the text out on a fixed character grid inside an SVG whose
//! intrinsic size is `cols × rows`. The SVG is written as its own file in the
//! EPUB and referenced with `<img>`; CSS `max-width: 100%; height: auto` then
//! shrinks the whole vector to the page width (and caps it at natural size on
//! wide screens). Because it is a vector, the monospace grid is preserved
//! exactly at any scale. Each line gets an explicit `textLength` with
//! `lengthAdjust="spacingAndGlyphs"`, so every column lands at the same x
//! regardless of the reading system's monospace metrics — keeping art aligned.
//!
//! Referenced (rather than inline) SVG is used deliberately: it is fully
//! EPUB3-conformant without manifest `properties="svg"` gymnastics, and it is
//! rendered more reliably across readers (Kindle included). Because an `<img>`
//! does not inherit the host document's `color`, the SVG carries its own
//! light/dark `<style>` so text stays legible in either theme.

use std::fmt::Write;

use html_escape::encode_text;

// Grid metrics in SVG user units. Only the ratio and the resulting natural
// pixel size matter; the block is scaled by the reader to fit.
const CHAR_W: f32 = 6.0; // monospace advance (~0.6em)
const LINE_H: f32 = 12.0; // line box height (~1.2em)
const FONT: f32 = 10.0;
const BASELINE: f32 = 9.0; // baseline offset within a line box
const PAD: f32 = 6.0; // padding between the grid and the card edge

/// Collects the SVG figures generated while rendering a document so the EPUB
/// assembler can write them as image resources.
#[derive(Default)]
pub struct Figures {
    /// `(path within OEBPS, svg file content)` pairs.
    pub items: Vec<(String, String)>,
}

impl Figures {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store an SVG document and return the OEBPS-relative href to reference it.
    pub fn add(&mut self, svg: String) -> String {
        let path = format!("figs/fig{}.svg", self.items.len() + 1);
        self.items.push((path.clone(), svg));
        path
    }
}

/// Rendering styles for a monospace block, differing only in how they relate to
/// the reader's theme (see [`crate::model::SvgMode`]).
#[derive(Clone, Copy)]
enum Style {
    /// Standalone SVG file with its own light "card" background and dark text.
    Card,
    /// Inline SVG whose text uses `currentColor`, following the host theme.
    Inline,
}

/// A complete standalone SVG **document** (with XML declaration and its own
/// light/dark card), suitable for writing as an image resource and referencing
/// via `<img>`. `None` if the block is empty after trimming.
pub fn card_svg(text: &str) -> Option<String> {
    compose(text, Style::Card)
}

/// An **inline** SVG fragment (no XML declaration, transparent background, text
/// painted with `currentColor`) to embed directly in a content document so the
/// diagram follows the reader's light/dark theme. `None` if empty.
pub fn inline_svg(text: &str) -> Option<String> {
    compose(text, Style::Inline)
}

fn compose(text: &str, style: Style) -> Option<String> {
    // Trim trailing whitespace per line (never affects the diagram) and drop
    // leading/trailing blank rows.
    let mut lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    while lines.first().is_some_and(|l| l.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return None;
    }

    let cols = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(1)
        .max(1);
    let rows = lines.len();
    let pad = match style {
        Style::Card => PAD,
        Style::Inline => 0.0,
    };
    let width = cols as f32 * CHAR_W + 2.0 * pad;
    let height = rows as f32 * LINE_H + 2.0 * pad;

    let mut svg = String::new();
    match style {
        // A self-contained background "card" keeps the diagram legible
        // regardless of the reader's page color: a referenced (`<img>`) SVG is
        // rendered in an isolated context, so host CSS and even
        // `prefers-color-scheme` may not reach it (e.g. Apple Books renders it
        // light-mode always). The default colors are therefore always a
        // readable light card; the media query is a progressive enhancement for
        // readers that do propagate the dark theme.
        Style::Card => {
            let _ = write!(
                svg,
                concat!(
                    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n",
                    "<svg xmlns=\"http://www.w3.org/2000/svg\" ",
                    "width=\"{w:.0}\" height=\"{h:.0}\" viewBox=\"0 0 {w:.0} {h:.0}\" ",
                    "preserveAspectRatio=\"xMidYMid meet\" role=\"img\">",
                    "<style>.bg{{fill:#f6f6f6}}",
                    "text{{font-family:monospace;font-size:{font:.0}px;",
                    "white-space:pre;fill:#1a1a1a}}",
                    "@media(prefers-color-scheme:dark){{.bg{{fill:#1e1e1e}}",
                    "text{{fill:#e6e6e6}}}}</style>",
                    "<rect class=\"bg\" x=\"0\" y=\"0\" width=\"{w:.0}\" height=\"{h:.0}\" rx=\"4\"/>",
                ),
                w = width,
                h = height,
                font = FONT,
            );
        }
        // Inline SVG follows the reader theme via `currentColor` (inherited from
        // the document text color), so no background is drawn.
        Style::Inline => {
            let _ = write!(
                svg,
                concat!(
                    "<svg xmlns=\"http://www.w3.org/2000/svg\" ",
                    "width=\"{w:.0}\" height=\"{h:.0}\" viewBox=\"0 0 {w:.0} {h:.0}\" ",
                    "preserveAspectRatio=\"xMidYMid meet\" role=\"img\" ",
                    "style=\"max-width:100%;height:auto\">",
                    "<style>text{{font-family:monospace;font-size:{font:.0}px;",
                    "white-space:pre;fill:currentColor}}</style>",
                ),
                w = width,
                h = height,
                font = FONT,
            );
        }
    }

    for (i, line) in lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        let n = line.chars().count();
        let y = pad + i as f32 * LINE_H + BASELINE;
        let text_len = n as f32 * CHAR_W;
        let _ = write!(
            svg,
            "<text xml:space=\"preserve\" x=\"{pad:.0}\" y=\"{y:.1}\" \
             textLength=\"{tl:.1}\" lengthAdjust=\"spacingAndGlyphs\">{content}</text>",
            pad = pad,
            y = y,
            tl = text_len,
            content = encode_text(line),
        );
    }

    svg.push_str("</svg>");
    Some(svg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_sizes_grid_to_widest_line_and_row_count() {
        let art = "abc\n+----------+"; // widest line is 12 columns, 2 rows
        let svg = card_svg(art).unwrap();
        assert!(svg.contains(&format!("width=\"{:.0}\"", 12.0 * CHAR_W + 2.0 * PAD)));
        assert!(svg.contains(&format!("height=\"{:.0}\"", 2.0 * LINE_H + 2.0 * PAD)));
    }

    #[test]
    fn card_has_readable_default_and_dark_enhancement() {
        let svg = card_svg("x").unwrap();
        // Default (always applied) is a light card with dark text.
        assert!(svg.starts_with("<?xml"));
        assert!(svg.contains("fill:#f6f6f6")); // card background
        assert!(svg.contains("fill:#1a1a1a")); // text
        assert!(svg.contains("<rect class=\"bg\""));
        // Progressive dark-theme enhancement for readers that propagate it.
        assert!(svg.contains("prefers-color-scheme:dark"));
    }

    #[test]
    fn inline_follows_theme_with_currentcolor_and_no_card() {
        let svg = inline_svg("x").unwrap();
        assert!(!svg.starts_with("<?xml")); // inline fragment, no XML decl
        assert!(svg.contains("fill:currentColor"));
        assert!(!svg.contains("<rect")); // transparent, no background
        // No fixed padding in inline mode.
        assert!(svg.contains(&format!("width=\"{:.0}\"", 1.0 * CHAR_W)));
    }

    #[test]
    fn uses_textlength_per_line_for_column_alignment() {
        let svg = card_svg("ab\nabcd").unwrap();
        assert!(svg.contains(&format!("textLength=\"{:.1}\"", 2.0 * CHAR_W)));
        assert!(svg.contains(&format!("textLength=\"{:.1}\"", 4.0 * CHAR_W)));
        assert!(svg.contains("lengthAdjust=\"spacingAndGlyphs\""));
    }

    #[test]
    fn escapes_xml_special_characters() {
        let svg = card_svg("a < b & c > d").unwrap();
        assert!(svg.contains("a &lt; b &amp; c &gt; d"));
    }

    #[test]
    fn empty_after_trimming_yields_none() {
        assert!(card_svg("\n\n   \n").is_none());
        assert!(inline_svg("\n\n   \n").is_none());
    }

    #[test]
    fn allocates_sequential_figure_paths() {
        let mut figs = Figures::new();
        assert_eq!(figs.add("a".into()), "figs/fig1.svg");
        assert_eq!(figs.add("b".into()), "figs/fig2.svg");
        assert_eq!(figs.items.len(), 2);
    }
}
