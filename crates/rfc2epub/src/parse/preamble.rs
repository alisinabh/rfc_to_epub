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
        let content: Vec<&str> = raw
            .lines()
            .filter(|l| l.trim() != "---")
            .collect();
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
        assert_eq!(p.get("discussions-to"), Some("https://ethereum-magicians.org/t/1"));
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
        assert_eq!(p.get("Author"), Some("Pieter Wuille <pw@x> Jonas Nick <jn@x>"));
        assert_eq!(p.get("Status"), Some("Final"));
    }

    #[test]
    fn value_with_url_colon_keeps_full_value() {
        let p = Preamble::parse("discussions-to: https://example.com/t/42\n");
        assert_eq!(p.get("discussions-to"), Some("https://example.com/t/42"));
    }
}
