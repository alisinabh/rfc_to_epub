//! Generate a nice-looking cover image for the book.
//!
//! The cover is designed as an SVG (title, RFC number, authors, category, date)
//! and rasterized to PNG with `resvg`, because e-reader library/shelf views —
//! Kindle in particular — expect a raster cover for the thumbnail. Two subset
//! Roboto faces are bundled so rendering does not depend on system fonts.

use std::sync::Arc;

use html_escape::encode_text;
use resvg::{tiny_skia, usvg};

use crate::model::Document;

static FONT_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Roboto-Regular.ttf");
static FONT_BOLD: &[u8] = include_bytes!("../../assets/fonts/Roboto-Bold.ttf");

// Canvas: a common 1.6 e-book cover ratio at print-crisp resolution.
const W: f32 = 1600.0;
const H: f32 = 2560.0;
const MARGIN: f32 = 140.0;

// Palette.
const ACCENT: &str = "#5aa2ff";
const TITLE: &str = "#ffffff";
const SUBTLE: &str = "#cdd6e2";
const MUTED: &str = "#8b96a6";

/// Font sizes for the secondary cover elements. Vertical spacing is derived
/// from these, so bumping a size keeps its block correctly spaced.
#[derive(Clone, Copy)]
struct Sizes {
    author: f32,
    badge: f32,
    date: f32,
    footer: f32,
    footer_sub: f32,
}

impl Sizes {
    fn default_profile() -> Self {
        Sizes {
            author: 74.0,
            badge: 52.0,
            date: 68.0,
            footer: 70.0,
            footer_sub: 52.0,
        }
    }
}

/// Build and rasterize the cover, returning PNG bytes. `None` if rasterization
/// fails (the book is then produced without a cover rather than failing).
pub fn cover_png(doc: &Document) -> Option<Vec<u8>> {
    rasterize(&build_svg(doc, &Sizes::default_profile()))
}

fn rasterize(svg: &str) -> Option<Vec<u8>> {
    let mut fontdb = usvg::fontdb::Database::new();
    fontdb.load_font_data(FONT_REGULAR.to_vec());
    fontdb.load_font_data(FONT_BOLD.to_vec());

    let mut opt = usvg::Options {
        font_family: "Roboto".to_string(),
        ..Default::default()
    };
    opt.fontdb = Arc::new(fontdb);

    let tree = usvg::Tree::from_str(svg, &opt).ok()?;
    let size = tree.size().to_int_size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height())?;
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut pixmap.as_mut());
    pixmap.encode_png().ok()
}

fn build_svg(doc: &Document, sizes: &Sizes) -> String {
    let content_w = W - 2.0 * MARGIN;

    // Adaptively size the title so long titles wrap to at most ~4 lines.
    let mut title_fs = 132.0_f32;
    let mut title_lines = wrap(&doc.title, content_w, title_fs);
    while title_lines.len() > 4 && title_fs > 72.0 {
        title_fs -= 8.0;
        title_lines = wrap(&doc.title, content_w, title_fs);
    }
    let title_lh = title_fs * 1.14;

    let mut s = String::new();
    s.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{W:.0}\" height=\"{H:.0}\" \
         viewBox=\"0 0 {W:.0} {H:.0}\">"
    ));

    // Background: a top-to-bottom deep-blue gradient.
    s.push_str(
        "<defs><linearGradient id=\"bg\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\">\
         <stop offset=\"0\" stop-color=\"#2b3a55\"/>\
         <stop offset=\"1\" stop-color=\"#1b2436\"/></linearGradient></defs>",
    );
    s.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{W:.0}\" height=\"{H:.0}\" fill=\"url(#bg)\"/>"
    ));

    // Accent tab + RFC number.
    s.push_str(&format!(
        "<rect x=\"{x:.0}\" y=\"210\" width=\"92\" height=\"10\" rx=\"5\" fill=\"{ACCENT}\"/>",
        x = MARGIN,
    ));
    if let Some(n) = doc.number {
        s.push_str(&text_line(
            &format!("RFC {n}"),
            MARGIN,
            306.0,
            48.0,
            true,
            ACCENT,
            Some(6.0),
        ));
    }

    // Title (possibly multiple lines).
    let mut y = 486.0 + title_fs;
    for line in &title_lines {
        s.push_str(&text_line(line, MARGIN, y, title_fs, true, TITLE, None));
        y += title_lh;
    }

    // Rule under the title, then a generous gap before the authors.
    y += 14.0;
    s.push_str(&format!(
        "<rect x=\"{x:.0}\" y=\"{y:.0}\" width=\"170\" height=\"6\" rx=\"3\" fill=\"{ACCENT}\"/>",
        x = MARGIN,
        y = y,
    ));
    y += 160.0;

    // Authors (cap the list so it never overflows). Spacing scales with size.
    for line in author_lines(doc) {
        s.push_str(&text_line(&line, MARGIN, y, sizes.author, false, SUBTLE, None));
        y += sizes.author * 1.42;
    }

    // Footer, anchored to the bottom.
    let footer_y = H - 130.0 - sizes.footer_sub * 1.3;
    s.push_str(&text_line("IETF", MARGIN, footer_y, sizes.footer, true, ACCENT, Some(3.0)));
    s.push_str(&text_line(
        "Internet Engineering Task Force",
        MARGIN,
        footer_y + sizes.footer_sub * 1.35,
        sizes.footer_sub,
        false,
        MUTED,
        None,
    ));

    // Category badge and date sit together just above the footer.
    let date_baseline = footer_y - 150.0;
    if let Some(date) = &doc.date {
        s.push_str(&text_line(date, MARGIN, date_baseline, sizes.date, false, MUTED, None));
    }
    if let Some(cat) = &doc.category {
        let label = cat.to_uppercase();
        let badge_h = sizes.badge * 2.0;
        // Padding inside the border, with a little extra on the right.
        let letter_spacing = 2.0;
        let text_w = label.chars().count() as f32 * (sizes.badge * 0.62 + letter_spacing);
        let left_pad = sizes.badge * 0.85;
        let right_pad = sizes.badge * 1.1;
        let badge_w = left_pad + text_w + right_pad;
        // Place the badge above the date (or at the date line if there's no date).
        let badge_bottom = if doc.date.is_some() {
            date_baseline - sizes.date - 34.0
        } else {
            date_baseline
        };
        let badge_y = badge_bottom - badge_h;
        s.push_str(&format!(
            "<rect x=\"{x:.0}\" y=\"{y:.0}\" width=\"{w:.0}\" height=\"{h:.0}\" rx=\"{r:.0}\" \
             fill=\"none\" stroke=\"{ACCENT}\" stroke-width=\"3\"/>",
            x = MARGIN,
            y = badge_y,
            w = badge_w,
            h = badge_h,
            r = badge_h / 2.0,
        ));
        s.push_str(&text_line(
            &label,
            MARGIN + left_pad,
            badge_y + badge_h * 0.66,
            sizes.badge,
            true,
            ACCENT,
            Some(letter_spacing),
        ));
    }

    s.push_str("</svg>");
    s
}

/// One `<text>` element.
fn text_line(
    text: &str,
    x: f32,
    y: f32,
    size: f32,
    bold: bool,
    fill: &str,
    letter_spacing: Option<f32>,
) -> String {
    let weight = if bold { "700" } else { "400" };
    let ls = letter_spacing
        .map(|v| format!(" letter-spacing=\"{v}\""))
        .unwrap_or_default();
    format!(
        "<text x=\"{x:.0}\" y=\"{y:.0}\" font-family=\"Roboto\" font-weight=\"{weight}\" \
         font-size=\"{size:.0}\" fill=\"{fill}\"{ls}>{content}</text>",
        content = encode_text(text),
    )
}

/// Author display lines, capped so a long author list can't overflow the cover.
fn author_lines(doc: &Document) -> Vec<String> {
    let names: Vec<&str> = doc
        .authors
        .iter()
        .map(|a| a.name.as_str())
        .filter(|n| !n.is_empty())
        .collect();
    if names.len() > 6 {
        let mut lines: Vec<String> = names[..5].iter().map(|s| s.to_string()).collect();
        lines.push("et al.".to_string());
        lines
    } else {
        names.iter().map(|s| s.to_string()).collect()
    }
}

/// Greedy word-wrap to fit `max_width` at `font_size`, using an approximate
/// average glyph advance for Roboto (slightly generous so text never overflows).
fn wrap(text: &str, max_width: f32, font_size: f32) -> Vec<String> {
    let char_w = font_size * 0.58;
    let max_chars = ((max_width / char_w).floor() as usize).max(1);

    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if candidate.chars().count() > max_chars && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            line = word.to_string();
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(text.to_string());
    }
    lines
}
