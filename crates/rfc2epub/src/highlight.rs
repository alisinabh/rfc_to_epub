//! Build-time syntax highlighting.
//!
//! EPUB readers don't run JavaScript, so highlighting has to happen when the
//! book is built (pandoc's model). We emit **class-based** `<span>` markup — the
//! CSS classes are Sublime scope atoms (`keyword`, `string`, `comment`, …) — and
//! ship a matching stylesheet in the EPUB (see [`crate::render::css`]). No RGB
//! colors are baked into the markup, so the same output adapts to light, dark,
//! and grayscale e-ink themes, mirroring the `currentColor` trick used for
//! inline SVG diagrams.
//!
//! Grammars come from [`two_face`] (syntect's defaults *plus* extras like
//! Solidity and TypeScript that matter for ERCs).

use std::sync::OnceLock;

use syntect::html::{ClassStyle, ClassedHTMLGenerator};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// The syntax set: syntect's bundled defaults plus two-face's extras
/// (`Solidity`, `TypeScript`, …). The **newlines** variant is required by
/// [`ClassedHTMLGenerator::parse_html_for_line_which_includes_newline`]. Loaded
/// once (it deserializes a ~1 MiB blob).
fn syntaxes() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(two_face::syntax::extra_newlines)
}

/// Map common fence-language aliases to a token syntect can resolve.
fn normalize_lang(lang: &str) -> &str {
    match lang.trim().to_ascii_lowercase().as_str() {
        "sh" | "shell" | "console" | "zsh" | "bash" => "bash",
        "yml" => "yaml",
        "js" => "javascript",
        "ts" => "typescript",
        "rs" => "rust",
        "py" => "python",
        "solidity" | "sol" => "sol",
        _ => lang.trim(),
    }
}

/// Highlight `code` written in language token `lang`, returning an XHTML
/// fragment of nested `<span class="…">` runs (no `<pre>`/`<code>` wrapper — the
/// renderer adds that). Returns `None` when the language is unrecognized or
/// highlighting fails, so the caller can fall back to an unhighlighted block.
pub fn highlight(code: &str, lang: &str) -> Option<String> {
    if lang.trim().is_empty() {
        return None;
    }
    let ss = syntaxes();
    let syntax = ss.find_syntax_by_token(normalize_lang(lang))?;
    let mut generator =
        ClassedHTMLGenerator::new_with_class_style(syntax, ss, ClassStyle::Spaced);
    for line in LinesWithEndings::from(code) {
        // A malformed line aborts highlighting; the caller then renders plain.
        generator
            .parse_html_for_line_which_includes_newline(line)
            .ok()?;
    }
    Some(generator.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_known_language_into_scope_classes() {
        let html = highlight("let x = 1;\n", "rust").expect("rust is known");
        // Class-based spans, scope atoms as classes, no inline color styles.
        assert!(html.contains("<span"));
        assert!(html.contains("class="));
        assert!(!html.to_lowercase().contains("style=\"color"));
        // `let` is a storage keyword in the Rust grammar.
        assert!(html.contains("storage"));
    }

    #[test]
    fn solidity_is_available_via_two_face() {
        let html = highlight("contract C {}\n", "solidity").expect("solidity present");
        assert!(html.contains("<span"));
    }

    #[test]
    fn unknown_language_returns_none() {
        assert!(highlight("whatever\n", "not-a-real-language-xyz").is_none());
        assert!(highlight("whatever\n", "").is_none());
    }

    #[test]
    fn escapes_html_metacharacters() {
        let html = highlight("let y = a < b && c > d;\n", "rust").unwrap();
        assert!(!html.contains("a < b"));
        assert!(html.contains("&lt;") && html.contains("&gt;") && html.contains("&amp;"));
    }
}
