//! Fallback parser for the published **plain-text** rendering of an RFC, used
//! for older documents that have no xml2rfc source.
//!
//! Plain text is lossy: there is no markup, only 72-column ASCII with page
//! furniture (running headers, `[Page N]` footers, form feeds) and layout done
//! with spaces. Reconstruction is therefore heuristic. The one hard rule is
//! **when a block is ambiguous, keep it verbatim** — reflowing a packet diagram
//! or table destroys it, whereas leaving a paragraph unwrapped is merely ugly.
//!
//! What we do:
//! 1. Strip page furniture (form feeds, `[Page N]`, running headers).
//! 2. Pull the title from the centered front block.
//! 3. Split the body on column-0 numbered/appendix headings (ignoring the
//!    dotted table-of-contents entries).
//! 4. Within each section, group blank-line-separated blocks and classify each
//!    as reflowable prose or verbatim art.

use regex::Regex;
use std::sync::OnceLock;

use crate::error::{Error, Result};
use crate::model::{Block, Document, Inline, Section, SourceKind};

pub fn parse(body: &str, number: Option<u32>) -> Result<Document> {
    let mut doc = Document {
        number,
        source: SourceKind::Text,
        ..Default::default()
    };

    // Title from the raw front block (before we strip furniture, which would
    // remove the centered header lines we need).
    doc.title = extract_title(body).unwrap_or_else(|| {
        number
            .map(|n| format!("RFC {n}"))
            .unwrap_or_else(|| "RFC".into())
    });

    let lines = strip_furniture(body, &doc.title);
    if lines.is_empty() {
        return Err(Error::Parse("no content after stripping page furniture".into()));
    }

    doc.sections = split_sections(&lines);
    if doc.sections.is_empty() {
        // No headings recognised: emit the whole body as one section so the
        // conversion still produces something readable.
        doc.sections.push(Section {
            number: None,
            title: doc.title.clone(),
            id: "body".into(),
            blocks: parse_blocks(&lines),
            subsections: Vec::new(),
        });
    }

    Ok(doc)
}

/// The centered, usually all-caps title in the first page's header block.
fn extract_title(body: &str) -> Option<String> {
    let mut title_parts = Vec::new();
    for line in body.lines().take(30) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !title_parts.is_empty() {
                // First blank run after we started collecting ends the title.
                break;
            }
            continue;
        }
        // Centered = substantial leading indent. Skip the left-aligned
        // "RFC: 791" / "Network Working Group" administrative lines.
        let indent = leading_spaces(line);
        if indent < 8 {
            continue;
        }
        if is_admin_line(trimmed) {
            continue;
        }
        title_parts.push(titlecase(trimmed));
        // A single strong title line is enough; keep collecting only adjacent
        // centered lines until a blank (handled above).
    }
    if title_parts.is_empty() {
        return None;
    }
    // Use the first centered line as the primary title.
    Some(title_parts.remove(0))
}

fn is_admin_line(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.starts_with("rfc")
        || l.starts_with("request for comments")
        || l.starts_with("network working group")
        || l.starts_with("internet-draft")
        || l.starts_with("obsoletes")
        || l.starts_with("updates")
        || l.starts_with("category")
        || l.starts_with("issn")
}

/// Remove form feeds, `[Page N]` footers, and running headers, returning the
/// surviving content lines with runs of blank lines collapsed.
fn strip_furniture(body: &str, title: &str) -> Vec<String> {
    let footer = regex(r"^\s*\[Page\s+\S+\]\s*$", &FOOTER);
    let std_header = regex(r"^\s*RFC\s+\d+\s+.*\b(19|20)\d{2}\s*$", &STD_HEADER);
    let draft_header = regex(r"^\s*Internet-Draft\b.*$", &DRAFT_HEADER);
    // Lone right-aligned date used as a header in old RFCs, e.g. "September 1981".
    let date_header = regex(r"^\s{20,}[A-Z][a-z]+\s+\d{4}\s*$", &DATE_HEADER);
    let title_lc = title.to_ascii_lowercase();

    let mut out: Vec<String> = Vec::new();
    // Set right after we drop the running doc-title header, so we can also drop
    // the running section-name line that follows it on continuation pages.
    let mut expect_running_name = false;
    for raw in body.lines() {
        if raw.contains('\u{000C}') {
            continue;
        }
        let line = raw.to_string();
        let trimmed = line.trim();

        if footer.is_match(&line)
            || std_header.is_match(&line)
            || draft_header.is_match(&line)
            || date_header.is_match(&line)
            || is_toc_line(&line)
        {
            expect_running_name = false;
            continue;
        }
        // Running title header: the document title alone on a column-0 line.
        if leading_spaces(&line) == 0 && trimmed.eq_ignore_ascii_case(&title_lc) {
            expect_running_name = true;
            continue;
        }
        // The short section-name line that trails the running title header.
        if expect_running_name {
            expect_running_name = false;
            if leading_spaces(&line) == 0
                && !trimmed.is_empty()
                && trimmed.len() < 40
                && !trimmed.contains(char::is_numeric)
            {
                continue;
            }
        }

        // Collapse multiple blank lines (page gaps) into one.
        if trimmed.is_empty() {
            if out.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
                continue;
            }
            out.push(String::new());
        } else {
            out.push(line.trim_end().to_string());
        }
    }
    while out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        out.pop();
    }
    out
}

/// Split content lines into top-level sections at column-0 numbered/appendix
/// headings. Preamble before the first heading becomes a leading section.
fn split_sections(lines: &[String]) -> Vec<Section> {
    // Gather (index, number, title) for each heading.
    let mut heads: Vec<(usize, Option<String>, String)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some((num, title)) = parse_heading(line, i, lines) {
            heads.push((i, num, title));
        }
    }

    let mut sections = Vec::new();

    // Preamble (status of memo, abstract, etc.) before the first heading.
    let first = heads.first().map(|h| h.0).unwrap_or(lines.len());
    let preamble = &lines[..first];
    if preamble.iter().any(|l| !l.trim().is_empty()) {
        let blocks = parse_blocks(preamble);
        if !blocks.is_empty() {
            sections.push(Section {
                number: None,
                title: "Front Matter".into(),
                id: "front-matter".into(),
                blocks,
                subsections: Vec::new(),
            });
        }
    }

    // Build a flat list of sections, then nest by number depth.
    let mut flat: Vec<Section> = Vec::new();
    for (h, &(start, ref num, ref title)) in heads.iter().enumerate() {
        let end = heads.get(h + 1).map(|n| n.0).unwrap_or(lines.len());
        let body = &lines[start + 1..end];
        let id = match num {
            Some(n) => format!("section-{n}"),
            None => slug(title),
        };
        flat.push(Section {
            number: num.clone(),
            title: title.clone(),
            id,
            blocks: parse_blocks(body),
            subsections: Vec::new(),
        });
    }

    sections.extend(nest_sections(flat));
    sections
}

/// Fold a flat, in-order section list into a tree using the dotted-number depth
/// (`3` is a parent of `3.1`). Sections without numbers attach at top level.
fn nest_sections(flat: Vec<Section>) -> Vec<Section> {
    let mut roots: Vec<Section> = Vec::new();
    for section in flat {
        let depth = section
            .number
            .as_deref()
            .map(|n| n.split('.').count())
            .unwrap_or(1);
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
        // No parent to attach to; keep it rather than lose content.
        siblings.push(section);
    }
}

/// Recognise a heading line. Returns `(number, title)`; `number` is `None` for
/// unnumbered headings like `Appendix A`.
///
/// Headings appear in two old-RFC styles: **centered** numbered top-level
/// headings (indented, often all-caps) and **column-0** numbered subsections.
/// Both must be preceded by a blank line; centered ones must also be followed
/// by a blank line (or be all-caps) to avoid catching enumerated list items or
/// sentences that begin with a number.
fn parse_heading(line: &str, idx: usize, lines: &[String]) -> Option<(Option<String>, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || !preceded_by_blank(idx, lines) {
        return None;
    }
    if is_toc_line(line) {
        return None;
    }
    let indent = leading_spaces(line);

    let numbered = regex(r"^(\d+(?:\.\d+)*)\.?\s+(\S.*)$", &HEAD_NUM);
    if let Some(c) = numbered.captures(trimmed) {
        let title = c[2].trim().to_string();
        if title.ends_with('.') || title.len() > 70 {
            return None;
        }
        // Real heading titles start with a letter; this rejects packet-diagram
        // bit rulers like "0   1   2   3" and field lines like "0. -- flags".
        if !title.starts_with(|ch: char| ch.is_ascii_alphabetic()) {
            return None;
        }
        if indent > 0 {
            let all_caps = !title.chars().any(|ch| ch.is_ascii_lowercase());
            let followed_blank = lines
                .get(idx + 1)
                .map(|l| l.trim().is_empty())
                .unwrap_or(true);
            if !all_caps && !followed_blank {
                return None;
            }
        }
        return Some((Some(c[1].to_string()), titlecase_heading(&title)));
    }

    let appendix = regex(r"^(?:APPENDIX|Appendix)\s+([A-Z0-9]+)[.:]?\s*(.*)$", &HEAD_APX);
    if let Some(c) = appendix.captures(trimmed) {
        let label = &c[1];
        let rest = c[2].trim();
        let title = if rest.is_empty() {
            format!("Appendix {label}")
        } else {
            format!("Appendix {label}: {}", titlecase_heading(rest))
        };
        return Some((None, title));
    }

    None
}

/// A heading is preceded by a blank line (or the start of the document).
fn preceded_by_blank(idx: usize, lines: &[String]) -> bool {
    idx == 0 || lines.get(idx - 1).map(|l| l.trim().is_empty()).unwrap_or(true)
}

/// Table-of-contents entries carry dot leaders (`....` or `. . .`).
fn is_toc_line(line: &str) -> bool {
    line.contains("....") || line.contains(". . .")
}

/// Split a run of lines into blocks and classify each.
fn parse_blocks(lines: &[String]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut cur: Vec<&str> = Vec::new();

    for line in lines {
        if line.trim().is_empty() {
            flush_block(&mut cur, &mut blocks);
        } else {
            cur.push(line);
        }
    }
    flush_block(&mut cur, &mut blocks);
    blocks
}

fn flush_block(cur: &mut Vec<&str>, blocks: &mut Vec<Block>) {
    if cur.is_empty() {
        return;
    }
    let block = if is_verbatim(cur) {
        Block::Artwork(dedent(cur))
    } else {
        Block::Paragraph(vec![Inline::text(reflow(cur))])
    };
    blocks.push(block);
    cur.clear();
}

/// Conservative art detector: any diagram characters, wide interior gaps, or
/// uneven indentation mark a block as verbatim.
fn is_verbatim(lines: &[&str]) -> bool {
    // Diagram / box-drawing / table rules.
    for l in lines {
        if l.contains('|')
            || l.contains("+-")
            || l.contains("-+")
            || l.contains("__")
            || l.contains('\\')
        {
            return true;
        }
    }
    // Interior column gaps (3+ spaces between content) after the common indent.
    let indent = min_indent(lines);
    for l in lines {
        let body: String = l.chars().skip(indent).collect();
        if body.trim_start().contains("   ") {
            return true;
        }
    }
    // Uneven left edges suggest structured layout (lists, definitions, art).
    let indents: Vec<usize> = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| leading_spaces(l))
        .collect();
    if let (Some(&mn), Some(&mx)) = (indents.iter().min(), indents.iter().max()) {
        if mx - mn > 4 {
            return true;
        }
    }
    false
}

/// Join prose lines into a single reflowable string.
fn reflow(lines: &[&str]) -> String {
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(l.trim());
    }
    // Collapse any double spaces introduced by joining.
    collapse_spaces(&out)
}

/// Preserve a verbatim block, removing only the shared left indent so wide art
/// has the best chance of fitting.
fn dedent(lines: &[&str]) -> String {
    let indent = min_indent(lines);
    lines
        .iter()
        .map(|l| l.chars().skip(indent).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

fn min_indent(lines: &[&str]) -> usize {
    lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| leading_spaces(l))
        .min()
        .unwrap_or(0)
}

fn leading_spaces(s: &str) -> usize {
    s.chars().take_while(|c| *c == ' ').count()
}

fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch == ' ' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

/// Best-effort title casing of an all-caps header line.
fn titlecase(s: &str) -> String {
    if s.chars().any(|c| c.is_ascii_lowercase()) {
        return s.to_string(); // already mixed case
    }
    s.split_whitespace()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => {
                    let rest: String = chars.as_str().to_ascii_lowercase();
                    format!("{}{}", first, rest)
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Headings are often ALL CAPS in old RFCs; soften them to title case.
fn titlecase_heading(s: &str) -> String {
    titlecase(s)
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

// Cached regexes.
static FOOTER: OnceLock<Regex> = OnceLock::new();
static STD_HEADER: OnceLock<Regex> = OnceLock::new();
static DRAFT_HEADER: OnceLock<Regex> = OnceLock::new();
static DATE_HEADER: OnceLock<Regex> = OnceLock::new();
static HEAD_NUM: OnceLock<Regex> = OnceLock::new();
static HEAD_APX: OnceLock<Regex> = OnceLock::new();

fn regex<'a>(pattern: &str, cell: &'a OnceLock<Regex>) -> &'a Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}
