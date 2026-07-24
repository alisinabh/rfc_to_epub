//! LaTeX → MathML Core conversion (via [`math_core`]).
//!
//! EPUB reading systems that support math (Apple Books, Kobo's kepub renderer,
//! KOReader, Calibre) render **MathML Core** — the subset browsers actually
//! implement — so we convert each formula at build time and embed the `<math>`
//! element directly in the XHTML. The original LaTeX is preserved in an
//! `alttext` attribute for readers that fall back to it (and for accessibility).
//!
//! Conversion never fails the build: on any error the function returns `None`
//! and the caller renders the LaTeX source as inline code instead.

use std::sync::OnceLock;

use math_core::{LatexToMathML, MathCoreConfig, MathDisplay};

/// The converter is stateless for our purposes (we use per-call local state, so
/// equation numbering restarts each formula) and is built once.
fn converter() -> &'static LatexToMathML {
    static C: OnceLock<LatexToMathML> = OnceLock::new();
    C.get_or_init(|| {
        let config = MathCoreConfig {
            // XHTML consumed as XML needs the MathML namespace on `<math>`.
            xml_namespace: true,
            ..Default::default()
        };
        LatexToMathML::new(config).expect("default math-core config is valid")
    })
}

/// Convert a LaTeX math string to a `<math>…</math>` MathML Core fragment, with
/// the LaTeX source preserved as `alttext`. `display` selects block (`$$…$$`)
/// vs. inline (`$…$`) math. Returns `None` on conversion failure.
pub fn latex_to_mathml(src: &str, display: bool) -> Option<String> {
    let disp = if display {
        MathDisplay::Block
    } else {
        MathDisplay::Inline
    };
    let result = converter().convert_with_local_state(src, disp).ok()?;
    Some(with_alttext(&result.mathml, src))
}

/// Splice an escaped `alttext` attribute into the leading `<math` tag (math-core
/// does not emit one). Output always begins with the literal `<math`.
fn with_alttext(mathml: &str, latex: &str) -> String {
    match mathml.strip_prefix("<math") {
        Some(rest) => {
            let esc = latex
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;");
            format!("<math alttext=\"{esc}\"{rest}")
        }
        None => mathml.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_inline_math_with_alttext() {
        let out = latex_to_mathml("x^2 + y^2", false).expect("valid latex");
        assert!(out.starts_with("<math alttext=\""));
        assert!(out.contains("</math>"));
        // Inline math must not carry display="block".
        assert!(!out.contains("display=\"block\""));
        // MathML namespace present for XHTML-as-XML.
        assert!(out.contains("xmlns=\"http://www.w3.org/1998/Math/MathML\""));
    }

    #[test]
    fn display_math_is_block() {
        let out = latex_to_mathml("\\frac{1}{2}", true).expect("valid latex");
        assert!(out.contains("display=\"block\""));
    }

    #[test]
    fn alttext_is_escaped() {
        let out = latex_to_mathml("a < b", false).expect("valid latex");
        assert!(out.contains("alttext=\"a &lt; b\""));
    }

    #[test]
    fn invalid_latex_returns_none() {
        // An unterminated group is a hard parse error.
        assert!(latex_to_mathml("\\frac{1", false).is_none());
    }
}
