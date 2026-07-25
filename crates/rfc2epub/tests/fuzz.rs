//! Robustness fuzzing for the two hand-rolled, string-slicing parsers
//! (MediaWiki and Markdown): feed them large volumes of random token salad and
//! assert only that they never **panic** and always **terminate**. This guards
//! the scanning loops and byte-slicing against regressions (off-by-one slices,
//! UTF-8 boundary cuts, zero-width matches) as the parsers evolve.
//!
//! Deterministic (seeded xorshift) so it is reproducible and network-free like
//! the rest of the suite.

use rfc2epub::model::SourceKind;
use rfc2epub::parse_source;

/// Tiny deterministic PRNG (xorshift64) so runs are reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[(self.next() as usize) % xs.len()]
    }
}

/// Tokens exercising the MediaWiki parser's trickiest constructs: block markers,
/// the HTML-ish inline set, wiki tables, definition lists, emphasis runs, the
/// `\u{E000}` placeholder sentinel, bare angle brackets, and non-ASCII bytes
/// (to stress char-boundary slicing).
const WIKI_TOKENS: &[&str] = &[
    "<pre>",
    "</pre>",
    "<source lang=\"x\">",
    "<source>",
    "</source>",
    "<syntaxhighlight>",
    "</syntaxhighlight>",
    "<code>",
    "</code>",
    "<tt>",
    "</tt>",
    "<nowiki>",
    "</nowiki>",
    "<ref>",
    "</ref>",
    "<ref name=\"a\">",
    "<ref name=\"é\"/>",
    "<references/>",
    "<br>",
    "<sub>",
    "</sub>",
    "<pubkey>",
    "<C>",
    "<R-HASH>",
    "<img src=\"x\">",
    "<img src=é>",
    "{|",
    "|}",
    "|-",
    "|+",
    "||",
    "!!",
    "!",
    "|",
    "| style=\"a\" |",
    "==",
    "===",
    "=",
    "*",
    "#",
    ":",
    ";",
    "*#",
    "----",
    "[[",
    "]]",
    "[[File:",
    "[[Image:",
    "|thumb|",
    "'''",
    "''",
    "[https://x/",
    " lbl]",
    "[[#",
    "|BIP]]",
    "bip-0032.mediawiki",
    "; term : def",
    "&nbsp;",
    "&é;",
    "&amp;",
    "&lt;",
    "  BIP: 1",
    "  Title: T",
    "café",
    "é",
    "—",
    "\u{E000}0\u{E000}",
    "<",
    ">",
    " ",
    "\n",
    "x",
    "\t",
    "://",
];

/// Tokens exercising the Markdown parser's raw-HTML table / details path, fence
/// sniffing, math, mermaid, and GFM structure.
const MD_TOKENS: &[&str] = &[
    "<table>",
    "</table>",
    "<tr>",
    "</tr>",
    "<td>",
    "</td>",
    "<th>",
    "</th>",
    "<details>",
    "</details>",
    "<summary>",
    "</summary>",
    "<sup>",
    "</sup>",
    "<a href=\"./eip-20.md\">",
    "</a>",
    "<code>",
    "</code>",
    "<br>",
    "<br/>",
    "&amp;",
    "&nbsp;",
    "&é;",
    "&#233;",
    "&#xE9;",
    "&",
    "café",
    "é",
    "—",
    "```",
    "```mermaid",
    "```json",
    "```solidity",
    "\n",
    " ",
    "{",
    "}",
    "\"k\":",
    "pragma solidity",
    "contract C",
    "# ",
    "## ",
    "| a | b |",
    "|---|---|",
    "-",
    "*",
    ">",
    "$$",
    "$x$",
    "[^1]",
    "[link](#x)",
];

fn hammer(seed: u64, kind: SourceKind, tokens: &[&str], rounds: usize) {
    let mut rng = Rng(seed);
    for _ in 0..rounds {
        let n = (rng.next() as usize) % 40 + 1;
        let mut s = String::new();
        for _ in 0..n {
            s.push_str(rng.pick(tokens));
        }
        // The contract under test: parse must not panic and must return.
        let _ = parse_source(&s, kind, Some(1));
    }
}

#[test]
fn mediawiki_parser_never_panics_on_random_input() {
    hammer(
        0x1234_5678_9abc_def1,
        SourceKind::Mediawiki,
        WIKI_TOKENS,
        20_000,
    );
}

#[test]
fn markdown_parser_never_panics_on_random_input() {
    hammer(
        0xdead_beef_cafe_0007,
        SourceKind::Markdown,
        MD_TOKENS,
        20_000,
    );
}
