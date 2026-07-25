//! An ordered RFC-822-style `key: value` preamble parser.
//!
//! EIP-1 *specifies* EIP/ERC preambles as RFC 822 headers (they only happen to
//! also parse as YAML), and BIP-2/BIP-3 use the same shape. A real YAML engine
//! buys nothing and chokes on the edge cases (`requires: 20, 155`,
//! `author: Name (@handle) <email>`), so we parse the header block directly.
//!
//! The parser is deliberately lenient — unknown keys are preserved, never a hard
//! error — because historical files carry stray fields. It handles:
//!   * `---` frontmatter delimiters (EIP/ERC/CAIP), stripped if present;
//!   * a common leading indent (BIP preambles indent every line two spaces),
//!     dedented before parsing;
//!   * multi-line values continued by deeper indentation (BIP `Authors`,
//!     `Discussion`).
//!
//! It also owns the shared **preamble → [`Document`] mapping**
//! ([`apply_preamble`]): the Markdown parser and the MediaWiki (BIP) parser both
//! feed their parsed header block here so title-page metadata, authors, status,
//! relations, and the leftover metadata table are populated identically
//! regardless of source format.

use std::sync::OnceLock;

use regex::Regex;

use crate::model::{Author, Block, Collection, DocId, Document, Inline, Relation};

/// A parsed preamble: ordered `(key, value)` pairs, keys as written.
#[derive(Debug, Clone, Default)]
pub struct Preamble {
    pub fields: Vec<(String, String)>,
}

impl Preamble {
    /// Parse a preamble block. Handles `---` delimiters and a common leading
    /// indent automatically.
    pub fn parse(raw: &str) -> Self {
        // Drop `---` fence lines, then dedent by the shared leading indent so
        // BIP's two-space-indented block is treated like an un-indented one
        // while still detecting *deeper*-indented continuation lines.
        let content: Vec<&str> = raw.lines().filter(|l| l.trim() != "---").collect();
        let min_indent = content
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);

        let mut fields: Vec<(String, String)> = Vec::new();
        let mut cur: Option<(String, String)> = None;

        for line in content {
            // Dedent by the common indent. `get` guards both a too-short line and
            // a cut that would fall inside a multi-byte char (never a panic).
            let dedented = line.get(min_indent..).unwrap_or_else(|| line.trim_start());
            if dedented.trim().is_empty() {
                continue;
            }

            let is_continuation = dedented.starts_with(' ') || dedented.starts_with('\t');
            let trimmed = dedented.trim();

            // A continuation line (deeper indent) extends the current value.
            if is_continuation {
                append_continuation(&mut cur, trimmed);
                continue;
            }

            // A `key: value` line, where the key is a single token (no spaces).
            if let Some(idx) = dedented.find(':') {
                let key = dedented[..idx].trim();
                if !key.is_empty() && !key.contains(char::is_whitespace) {
                    let val = dedented[idx + 1..].trim().to_string();
                    if let Some(prev) = cur.take() {
                        fields.push(prev);
                    }
                    cur = Some((key.to_string(), val));
                    continue;
                }
            }
            // No usable key: treat as a continuation of the previous value.
            append_continuation(&mut cur, trimmed);
        }
        if let Some(prev) = cur {
            fields.push(prev);
        }
        Preamble { fields }
    }

    /// Look up a field value case-insensitively.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    /// Whether the preamble carries any of the given keys.
    pub fn has_any(&self, keys: &[&str]) -> bool {
        keys.iter().any(|k| self.get(k).is_some())
    }
}

fn append_continuation(cur: &mut Option<(String, String)>, text: &str) {
    if let Some((_, v)) = cur.as_mut() {
        if !v.is_empty() {
            v.push(' ');
        }
        v.push_str(text);
    }
}

// ---------------------------------------------------------------------------
// Preamble → Document mapping (shared by the Markdown and MediaWiki parsers)
// ---------------------------------------------------------------------------

/// Map a parsed preamble onto document metadata. The document's id comes from
/// the collection's id key (`eip`/`bip`/`caip`) or the caller's `number`; title,
/// status, date, description (→ abstract), authors, and `Requires`/`Replaces`
/// relations are mapped to first-class slots, and every other field lands in the
/// `extra` metadata table in insertion order.
pub(crate) fn apply_preamble(
    doc: &mut Document,
    pre: &Preamble,
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
        "eip",
        "erc",
        "bip",
        "caip",
        "title",
        "status",
        "created",
        "date",
        "description",
        "author",
        "authors",
        "requires",
    ];
    for (k, v) in &pre.fields {
        let kl = k.to_ascii_lowercase();
        if MAPPED.contains(&kl.as_str()) || kl == "replaces" || v.trim().is_empty() {
            continue;
        }
        doc.extra.push((titlecase_key(k), v.trim().to_string()));
    }
}

/// Parse an author line into individual authors. Handles both the EIP style
/// (`Name (@github) <email>`, comma-separated) and the BIP style (multiple
/// `Name <email>` entries space-joined from a multi-line preamble value). A
/// GitHub handle becomes a profile link; otherwise an email becomes a `mailto:`.
pub(crate) fn parse_authors(s: &str) -> Vec<Author> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // A name (greedy, but never crossing a bracket or comma into the next
        // author), then an optional `(@handle)` and an optional `<email>`.
        // Iterating this splits comma-separated *and* space-joined author lists.
        Regex::new(r"([^<>(),]+)(?:\(([^)]*)\))?\s*(?:<([^>]*)>)?").expect("valid regex")
    });
    re.captures_iter(s)
        .filter_map(|c| {
            let name = c.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            if name.is_empty() {
                return None;
            }
            let handle = c.get(2).map(|m| m.as_str().trim());
            let email = c
                .get(3)
                .map(|m| m.as_str().trim())
                .filter(|e| !e.is_empty());
            let link = handle
                .filter(|h| h.starts_with('@'))
                .map(|h| format!("https://github.com/{}", h.trim_start_matches('@')))
                .or_else(|| email.map(|e| format!("mailto:{e}")));
            Some(Author {
                name: name.to_string(),
                organization: None,
                link,
            })
        })
        .collect()
}

/// Parse a comma-separated id-number list (`"2718, 155"`) into same-collection
/// [`DocId`]s.
pub(crate) fn parse_id_list(s: &str, collection: Collection) -> Vec<DocId> {
    s.split(',')
        .filter_map(|p| p.trim().parse::<u32>().ok())
        .map(|n| DocId::new(collection, n))
        .collect()
}

/// Title-case a preamble key for the metadata table: `"discussions-to"` →
/// `"Discussions-To"`, `"Layer"` → `"Layer"`.
pub(crate) fn titlecase_key(k: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_eip_frontmatter_in_order() {
        let raw = "---\n\
eip: 1559\n\
title: Fee market change\n\
description: A transaction fee overhaul\n\
author: Vitalik Buterin (@vbuterin), Eric Conner (@econoar)\n\
discussions-to: https://ethereum-magicians.org/t/1\n\
status: Final\n\
type: Standards Track\n\
category: Core\n\
created: 2019-04-13\n\
requires: 2718, 155\n\
---\n";
        let p = Preamble::parse(raw);
        assert_eq!(p.get("eip"), Some("1559"));
        assert_eq!(p.get("Title"), Some("Fee market change")); // case-insensitive
        assert_eq!(p.get("requires"), Some("2718, 155"));
        assert_eq!(
            p.get("discussions-to"),
            Some("https://ethereum-magicians.org/t/1")
        );
        // Order preserved.
        assert_eq!(p.fields.first().map(|(k, _)| k.as_str()), Some("eip"));
    }

    #[test]
    fn dedents_and_joins_bip_style_continuations() {
        // BIP preambles indent every line two spaces; Authors continues on the
        // next, deeper-indented line.
        let raw = "  BIP: 341\n  Title: Taproot\n  Author: Pieter Wuille <pw@x>\n          Jonas Nick <jn@x>\n  Status: Final\n";
        let p = Preamble::parse(raw);
        assert_eq!(p.get("BIP"), Some("341"));
        assert_eq!(p.get("Title"), Some("Taproot"));
        assert_eq!(
            p.get("Author"),
            Some("Pieter Wuille <pw@x> Jonas Nick <jn@x>")
        );
        assert_eq!(p.get("Status"), Some("Final"));
    }

    #[test]
    fn value_with_url_colon_keeps_full_value() {
        let p = Preamble::parse("discussions-to: https://example.com/t/42\n");
        assert_eq!(p.get("discussions-to"), Some("https://example.com/t/42"));
    }

    #[test]
    fn parse_authors_handles_eip_comma_style() {
        let authors = parse_authors("Alice (@alice), Bob <bob@example.com>, Carol");
        assert_eq!(authors.len(), 3);
        assert_eq!(authors[0].name, "Alice");
        assert_eq!(authors[0].link.as_deref(), Some("https://github.com/alice"));
        assert_eq!(authors[1].name, "Bob");
        assert_eq!(authors[1].link.as_deref(), Some("mailto:bob@example.com"));
        assert_eq!(authors[2].name, "Carol");
        assert_eq!(authors[2].link, None);
    }

    #[test]
    fn parse_authors_splits_bip_space_joined_entries() {
        // BIP preambles put each author on its own line; the preamble parser
        // joins those continuation lines with a space (no comma between them).
        let authors = parse_authors("Pieter Wuille <pw@x> Jonas Nick <jn@x> Anthony Towns <at@x>");
        assert_eq!(authors.len(), 3);
        assert_eq!(authors[0].name, "Pieter Wuille");
        assert_eq!(authors[0].link.as_deref(), Some("mailto:pw@x"));
        assert_eq!(authors[1].name, "Jonas Nick");
        assert_eq!(authors[2].name, "Anthony Towns");
    }

    #[test]
    fn apply_preamble_maps_bip_metadata_and_relations() {
        let pre = Preamble::parse(
            "  BIP: 341\n  Title: Taproot\n  Author: Pieter Wuille <pw@x>\n          Jonas Nick <jn@x>\n  Status: Final\n  Layer: Consensus (soft fork)\n  Created: 2020-01-19\n  Requires: 340, 342\n",
        );
        let mut doc = Document::default();
        apply_preamble(&mut doc, &pre, Some(Collection::Bip), Some(341));
        assert_eq!(doc.number(), Some(341));
        assert_eq!(doc.title, "Taproot");
        assert_eq!(doc.status.as_deref(), Some("Final"));
        assert_eq!(doc.date.as_deref(), Some("2020-01-19"));
        assert_eq!(doc.authors.len(), 2);
        let req = doc
            .relations
            .iter()
            .find(|r| r.label == "Requires")
            .unwrap();
        assert_eq!(
            req.targets.iter().map(|t| t.number).collect::<Vec<_>>(),
            vec![340, 342]
        );
        // Unmapped `Layer` lands in the metadata table, title-cased.
        assert!(doc
            .extra
            .iter()
            .any(|(k, v)| k == "Layer" && v == "Consensus (soft fork)"));
    }
}
