# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`rfc2epub` converts IETF RFCs and Markdown spec collections (Ethereum EIPs/ERCs,
Bitcoin BIPs, CAIPs) into clean, reflowable EPUB files for e-readers. The
README is the canonical description of behavior, output design, and rationale
(diagram rendering, SVG modes, cover generation) — read it before changing
user-facing output. This file covers what the README does not: build workflow
and the internal architecture across files.

## Commands

```bash
cargo build                       # build workspace
cargo run -p rfc2epub-cli -- 9110 # run the CLI (args after `--`)
cargo test                        # all tests
cargo test --test convert         # the end-to-end integration tests
cargo test --test convert xml_extracts_metadata_and_blocks  # a single test
cargo clippy --all-targets        # lints (clippy `all` is warn; unsafe_code is forbid)
cargo fmt
```

**MSRV is 1.95** (bumped from 1.85 for `merman`, which backs the default
`mermaid` feature). `cargo build --no-default-features` drops `merman` and renders
mermaid fences as verbatim source.

Integration tests in `crates/rfc2epub/tests/convert.rs` are **network-free**:
they feed inline source strings through `parse_source` / `render::to_epub`.
Keep new tests offline the same way — the fetch layer is not exercised in CI.

## Architecture

The pipeline is one line (`lib.rs::convert`):

```
fetch (per collection, cached) → parse → model::Document (IR) → [resolve image assets] → [render mermaid diagrams] → render → EPUB bytes
```

The design's load-bearing idea: **several parsers produce one shared IR
(`model::Document`) that one renderer consumes.** Output quality stays uniform
regardless of source, and new capabilities are meant to be *additive* — new
collections or parsers should not require touching `model.rs` or the renderer.

### The IR — `model.rs` (start here)

`Document` holds format-neutral structure: metadata (title, `authors`, `status`,
`relations`, `abstract_`, `extra` for unmapped preamble keys), a tree of
`Section`, and `Block`s. The central semantic distinction is reflowable
**prose** (`Block::Paragraph`, lists, tables) vs. verbatim **`Block::Artwork` /
`Block::Code`** — the latter is what gets rendered as fixed-grid SVG so ASCII
diagrams never wrap on narrow screens.

`Collection` (Rfc/Eip/Erc/Bip/Caip) is the *only* type that knows how an id is
spelled (`token`, `label`, `from_token`) and where it lives on the web
(`external_url`, `urn`). Adding a collection is mostly extending this enum plus a
fetch source.

### Parsers — `parse.rs` + `parse/`

`parse::parse` dispatches on `SourceKind`:
- `xml.rs` — xml2rfc v3 (modern RFCs). Highest fidelity: real sections, prose,
  artwork, code. RFC output must stay byte-for-byte stable.
- `text.rs` — plain-text RFC fallback. Heuristic reconstruction; carries page
  boundaries (the only source that does — drives `--no-page-breaks`).
- `markdown.rs` — comrak GFM AST for EIP/ERC/BIP/CAIP. Collection is inferred
  from the preamble when not supplied. Produces GitHub-compatible slugs so
  in-document `#section` links resolve to in-book anchors; cross-document links
  (`./eip-20.md`) rewrite to canonical web URLs.
- `mediawiki.rs` — hand-rolled MediaWiki *subset* for the ~93% of BIPs still in
  `.mediawiki` (Taproot, SegWit, BIP-32/39). Symmetric with `text.rs`: a quirky
  legacy format → the same IR. Targets the closed construct census (headings,
  `{|` wiki tables, `<ref>` footnotes, `<source>` code, `<pre>`, images,
  `'''bold'''`/`''italic''`/links). Extracts `<ref>`/`<code>`/`<nowiki>` into
  placeholders *before* emphasis pairing so a stray `''` never swallows a `<ref>`.
- `preamble.rs` — RFC-822 header parsing shared by Markdown/MediaWiki specs
  (frontmatter, or the BIP-3 indented header block / `<pre>` preamble). Also owns
  the shared **preamble → `Document` mapping** (`apply_preamble`, `parse_authors`)
  both the `markdown` and `mediawiki` parsers call.

### Render — `render.rs` + `render/`

`render::to_epub` → `epub.rs::build` assembles the EPUB (via `epub-builder`),
pulling from siblings: `xhtml.rs` (IR → XHTML), `css.rs` (embedded stylesheet,
light/dark/e-ink), `svg.rs` (fixed-char-grid diagrams; **`SvgMode` matters** —
`inline` is the default and tested-best on Kindle, `card` is epubcheck-clean —
do not flip the default), `cover.rs` (SVG→PNG cover via `resvg` + bundled Roboto).

### Supporting modules

- `fetch.rs` — `DocSpec`, per-collection sources, download cache. `assets.rs`
  downloads and embeds Markdown-referenced images (post-parse, pre-render).
- `highlight.rs` — syntect + two-face, class-based (no inline color).
- `mathml.rs` — LaTeX → MathML Core via `math-core`.
- `diagram.rs` — post-parse pass filling `Block::Diagram.svg`. Normalizes the
  source (drops a `%%` comment before the diagram-type line, which mermaid
  rejects), renders in-process via `merman` (export-safe SVG; rejects any
  `<foreignObject>`), then — only with `Options.mermaid_online` (`--online-mermaid`)
  — falls back to the Kroki service for diagrams merman can't parse (e.g. sequence
  actors with spaces). Behind the default `mermaid` feature; the Kroki path is not
  feature-gated. Runs from `lib::convert` and the CLI's `--input` path, so
  `parse_source` itself stays a pure transform.

## Conventions

- **Module files, not `mod.rs`**: a module `foo` with children is `foo.rs` +
  `foo/` (e.g. `parse.rs` + `parse/`). Never create `mod.rs`.
- `Cargo.lock` is committed (this workspace ships a binary).
- Public library API lives in `lib.rs` (`convert`, `convert_rfc`,
  `parse_source`, `Options`); the CLI in `crates/rfc2epub-cli` is a thin clap
  wrapper over it.

## Roadmap

`docs/deferred-work.md` tracks the remaining intentionally-deferred items. The
big ones have now shipped — **MediaWiki BIP parser**, **in-process mermaid**
(merman), **BOLT fetching**, **raw-HTML tables/`<details>`**, and **untagged-fence
sniffing**. Still deferred: **`--math svg`** (no dependable pure-Rust LaTeX→SVG
engine — `ratex` is 0.0.1) and small polish (GIF→PNG frame-1, manifest
`mathml`/`svg` properties). `docs/markdown-support-research.md` is the original
research. Check these before starting collection/parser work.
