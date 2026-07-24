//! The single stylesheet embedded in every generated EPUB.
//!
//! Design goals: readable reflowable prose, and — crucially — monospaced,
//! horizontally scrollable blocks for ASCII art / packet diagrams / code so
//! they survive narrow e-reader screens instead of being reflowed into mush.

pub const STYLESHEET: &str = r#"
html { -webkit-text-size-adjust: 100%; }
body {
  font-family: Georgia, "Times New Roman", serif;
  line-height: 1.5;
  margin: 0 5%;
  hyphens: auto;
}
h1, h2, h3, h4, h5, h6 {
  font-family: Helvetica, Arial, sans-serif;
  line-height: 1.25;
  page-break-after: avoid;
}
h1 { font-size: 1.6em; margin: 1em 0 0.5em; }
h2 { font-size: 1.3em; margin: 1.2em 0 0.4em; }
h3 { font-size: 1.1em; }
p { margin: 0.6em 0; text-align: justify; }

/* Monospace diagrams/code are emitted as responsive SVG images that scale to
   fit the screen width without ever wrapping the grid. */
figure.artwork, figure.sourcecode {
  margin: 0.9em 0;
  text-align: center;
}
figure.artwork img, figure.sourcecode img,
figure.artwork svg, figure.sourcecode svg {
  display: block;
  margin: 0 auto;
  max-width: 100%;
  height: auto;
}

/* Fallback styling for any verbatim block still rendered as text. */
pre {
  font-family: "DejaVu Sans Mono", "Courier New", monospace;
  font-size: 0.72em;
  line-height: 1.2;
  white-space: pre;
  overflow-x: auto;
  background: #f6f6f6;
  padding: 0.5em 0.7em;
  border-radius: 4px;
  margin: 0.8em 0;
}
pre.artwork { background: #f0f4f8; }
code, tt { font-family: "DejaVu Sans Mono", "Courier New", monospace; font-size: 0.85em; }

/* BCP 14 keywords (MUST / SHOULD / …). */
.bcp14 { font-variant: small-caps; font-weight: bold; }

table { border-collapse: collapse; margin: 0.8em 0; font-size: 0.9em; }
th, td { border: 1px solid #bbb; padding: 0.25em 0.5em; text-align: left; }
th { background: #eee; }

dl { margin: 0.6em 0; }
dt { font-weight: bold; margin-top: 0.5em; }
dd { margin: 0 0 0.3em 1.5em; }

aside, blockquote {
  border-left: 3px solid #ccc;
  margin: 0.8em 0;
  padding: 0.1em 1em;
  color: #333;
}

a { color: inherit; text-decoration: underline; }

/* Reproduce the original document's pagination (plain-text sources only). */
.page-break {
  page-break-before: always;
  break-before: page;
  height: 0;
  margin: 0;
}

.cover { margin: 0; padding: 0; text-align: center; }
.cover img { max-width: 100%; height: auto; }

.titlepage { text-align: center; margin-top: 20%; }
.titlepage .doc-id { font-size: 1.1em; letter-spacing: 0.2em; color: #666; }
.titlepage h1 { font-size: 1.9em; }
.titlepage .meta { color: #555; font-size: 0.95em; margin-top: 1.5em; }
.colophon { color: #666; font-size: 0.85em; margin-top: 3em; }

/* Title-page metadata table (EIP discussions-to, created, license …). */
table.docmeta { margin: 1.2em auto; font-size: 0.85em; border-collapse: collapse; }
table.docmeta th, table.docmeta td {
  border: none; background: none; padding: 0.15em 0.6em; vertical-align: top;
}
table.docmeta th { text-align: right; color: #666; font-weight: 600; white-space: nowrap; }
table.docmeta td { text-align: left; }

/* Embedded images and rendered diagrams. */
figure.image, figure.diagram { margin: 1em 0; text-align: center; }
figure.image img, figure.diagram img, figure.diagram svg { max-width: 100%; height: auto; }
figure.image figcaption { font-size: 0.85em; color: #666; font-style: italic; margin-top: 0.4em; }
img.inline-image { max-width: 100%; height: auto; vertical-align: middle; }
.missing-image { color: #999; font-style: italic; text-align: center; }

/* Display math. MathML follows the reader theme via currentColor. */
.math-block { margin: 0.9em 0; overflow-x: auto; text-align: center; }
math { color: inherit; }

/* Footnotes: superscript refs and the end-of-book definition list. */
.footnote-ref { line-height: 1; }
.footnote-ref a { text-decoration: none; }
#footnotes { font-size: 0.92em; }

del { text-decoration: line-through; color: #888; }
hr { border: none; border-top: 1px solid #ccc; margin: 1.6em 0; }

/* Class-based syntax highlighting. Classes are Sublime scope atoms emitted by
   syntect (ClassStyle::Spaced): style the broad atom and it matches regardless
   of the trailing language atom. Colors degrade to distinct grays on e-ink. */
pre.highlight {
  font-family: "DejaVu Sans Mono", "Courier New", monospace;
  font-size: 0.75em;
  line-height: 1.35;
  white-space: pre;
  overflow-x: auto;
  background: #f6f8fa;
  padding: 0.6em 0.8em;
  border-radius: 4px;
  margin: 0.8em 0;
}
pre.highlight code { font-family: inherit; font-size: inherit; background: none; padding: 0; }
.highlight .comment { color: #6a737d; font-style: italic; }
.highlight .string { color: #032f62; }
.highlight .constant { color: #005cc5; }
.highlight .keyword, .highlight .storage { color: #d73a49; }
.highlight .entity { color: #6f42c1; }
.highlight .support { color: #005cc5; }
.highlight .variable { color: #e36209; }
.highlight .punctuation { color: #24292e; }
.highlight .invalid { color: #b31d28; }

@media (prefers-color-scheme: dark) {
  pre { background: #1e1e1e; }
  pre.artwork { background: #182028; }
  th { background: #333; }
  pre.highlight { background: #161b22; }
  .highlight .comment { color: #8b949e; }
  .highlight .string { color: #a5d6ff; }
  .highlight .constant, .highlight .support { color: #79c0ff; }
  .highlight .keyword, .highlight .storage { color: #ff7b72; }
  .highlight .entity { color: #d2a8ff; }
  .highlight .variable { color: #ffa657; }
  .highlight .punctuation { color: #c9d1d9; }
  hr { border-top-color: #444; }
}
"#;
