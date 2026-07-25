# Deferred work: roadmap after Markdown spec-collection support

*Written 2026-07-24, after landing Markdown support for EIPs/ERCs/BIPs/CAIPs
(commit `218d0cd`). This is the follow-up plan for the pieces the research doc
(`markdown-support-research.md`) intentionally left for later milestones. Each
item lists **current state**, **why it was deferred**, a **concrete approach**
with the exact integration points in today's code, an **effort** estimate, and
**risks**.*

## Status (updated after the follow-up implementation pass)

Most of this roadmap has now **shipped**. What remains genuinely deferred is the
`--math svg` mode (item 4) and two of the polish items (6a GIF→PNG, 6c manifest
properties). See the per-item notes below.

## Priority roadmap

| # | Item | Unlocks | Effort | Status |
|---|------|---------|--------|--------|
| 1 | MediaWiki BIP parser | ~93% of BIPs (Taproot 341, SegWit 141, BIP-32/39…) | M–L | ✅ **Done** — `parse/mediawiki.rs` |
| 2 | In-process mermaid (merman) | 3 ERCs today; future diagram-heavy specs | M | ✅ **Done** — `diagram.rs`, default `mermaid` feature (MSRV → 1.95) |
| 3 | BOLT fetching | Lightning BOLTs (parser already works) | S | ✅ **Done** — `model::bolt_filename` + `fetch::fetch_bolt` |
| 4 | `--math svg` mode | Reliable math on Kindle | M | ⏸ **Deferred** — no dependable pure-Rust LaTeX→SVG engine (`ratex` is 0.0.1) |
| 5 | Raw-HTML `<table>` → real tables | ~40 EIP files with inline tables | S–M | ✅ **Done** — `markdown::parse_html_table` + `<details>`→aside |
| 6 | GIF → PNG frame-1, untagged-fence sniffing, manifest `mathml`/`svg` property | Polish / epubcheck | S each | ◐ **Partial** — 6b (fence sniffing) done; 6a/6c deferred |

The IR and pipeline were built so every item below is *additive* — none required
reworking `model.rs` or the renderer, and none did.

---

## 1. MediaWiki BIP parser

> **✅ Done.** Implemented as `parse/mediawiki.rs` (hand-rolled subset, route B).
> `parse::parse` dispatches `SourceKind::Mediawiki` to it; the preamble → metadata
> mapping was extracted into `parse/preamble.rs::apply_preamble` and is shared with
> the Markdown parser; `lib::convert` now runs `assets::resolve` for any source with
> an `asset_base` (Markdown *and* MediaWiki). Inline `<ref>`/`<code>`/`<nowiki>` are
> lifted into placeholders before emphasis pairing so a stray `''` never swallows a
> `<ref>`. Validated end-to-end against real BIPs (341, 141, 32, 39, 173, 152, 340).

**Current state.** `fetch::fetch_bip` probes `bip-NNNN.md` then
`bip-NNNN.mediawiki` and tags the latter `SourceKind::Mediawiki`.
`parse::parse` then returns `Error::Unsupported("MediaWiki sources are not yet
supported …")`. So the fetch/plumbing is done; only the parser is missing. The
14 Markdown BIPs already convert.

**Why deferred.** 196 of 210 BIPs are `.mediawiki`; a real MediaWiki parser is a
whole third parser. The research doc (§3.2) recommends shipping Markdown first
(option A) and doing MediaWiki as its own milestone (option B).

**Approach — hand-rolled subset parser (`parse/mediawiki.rs`).** Symmetric with
`parse/text.rs` ("parse a quirky legacy format into the IR"), emitting the same
`Document`. Target *exactly* the construct census the research verified across
all 196 files (§1.2) — the set is small and closed (no templates, no `<math>`):

| Construct | Count | Maps to |
|---|---|---|
| `<pre>…</pre>` (preamble + code) | 196 | preamble via `preamble::Preamble::parse`; else `Block::Code` |
| `{\| … \|}` wiki tables | 111 | `Block::Table` |
| `<code>` / `<tt>` | 68 / 34 | `Inline::Code` |
| `<ref>` footnotes | 53 | `Inline::FootnoteRef` + trailing Footnotes section |
| raw `<img src="bip-NNNN/foo.png">` | 18 | `Block::Figure` (asset layer already handles it) |
| `<source lang="…">` | 12 | `Block::HighlightedCode` (reuse `highlight::highlight`) |
| `[[File:…]]` | 10 | `Block::Figure` |
| `== Heading ==`, `'''bold'''`, `''italic''`, `[url label]`, `* / #` lists | — | sections / `Strong` / `Emph` / `Link` / `List` |

**Integration points (exact):**
1. `crates/rfc2epub/src/parse.rs`: add `pub mod mediawiki;` and replace the
   `SourceKind::Mediawiki => Err(Error::Unsupported(...))` arm with
   `mediawiki::parse(body, Some(Collection::Bip), number)`.
2. New `parse/mediawiki.rs` with `pub fn parse(body, collection, number) ->
   Result<Document>`. The BIP preamble sits inside the first `<pre>…</pre>` as
   RFC-822 headers — extract that block and feed it straight to
   `preamble::Preamble::parse` (already handles the indentation), then reuse the
   BIP branch of `markdown::apply_preamble` (extract that mapping into a shared
   `preamble`-level helper so both parsers call it).
3. Assets: after parsing, the `lib::convert` path must run `assets::resolve` for
   Mediawiki too (today it's gated on `SourceKind::Markdown` — widen to "has an
   `asset_base`"). BIP asset refs are `bip-NNNN/foo.png` relative to the repo
   root; `fetch_bip` already sets `asset_base = BIP_BASE`.

**Alternative — `parse-wiki-text-2` 0.2** for the wikitext skeleton + custom
handling of the embedded HTML-ish constructs (`<pre>`, `<source>`, `<ref>`,
`<img>`). Less code, but adds a middling-maintenance dep and the HTML-construct
handling is still on us. The verified, closed construct set makes the hand-rolled
route (B) tractable and dependency-free — prefer it.

**Effort:** comparable to `parse/text.rs` (~600 lines). **Risks:** wiki-table
(`{|`) parsing is the fiddliest; footnote (`<ref>`) numbering must match the
Markdown footnote scheme already in the renderer.

---

## 2. In-process mermaid rendering (merman)

> **✅ Done.** Implemented as `diagram.rs`, a post-parse pass run from
> `lib::convert` and the CLI `--input` path (keeping `parse_source` pure). Uses
> `merman`'s `render_svg_resvg_safe_sync` (export-safe, no `<foreignObject>`) with
> a guardrail that rejects any `<foreignObject>` that slips through. Behind the
> **default `mermaid` Cargo feature** (bumped workspace MSRV 1.85 → 1.95, merman's
> floor); `--no-default-features` falls back to verbatim source. Sources are
> normalized first (a `%%` comment before the diagram-type line breaks mermaid).
> The **Kroki** network fallback (step 2 of the chain) is implemented as opt-in
> (`--online-mermaid` / `Options.mermaid_online`) for diagrams merman can't parse
> — e.g. erc-5883's sequence diagram uses actor names with spaces, which real
> mermaid accepts but merman rejects. `mmdc` was not needed. Validated on erc-7715
> (merman) and erc-5883 (Kroki).

**Current state.** `parse/markdown.rs::code_block` maps a ` ```mermaid ` fence to
`Block::Diagram { svg: String::new(), source }`. The renderer
(`render/xhtml.rs::render_block`) already branches: non-empty `svg` → inline
`<figure class="diagram">`, empty `svg` → `emit_figure(source, "diagram")` (the
source shown verbatim). **So the only missing piece is filling `svg`.**

**Why deferred.** `merman` 0.8-alpha is a large, alpha dependency (ported
dagre/fCoSE layout engines) for the 3 ERC files that use mermaid today. The
research doc calls the code-block fallback "defensible" as a first cut.

**Approach.** Add a `mermaid` Cargo feature (off by default) and a
`render/diagram.rs` (or a post-parse pass) that calls merman's **export-safe SVG
mode** (no `foreignObject`). Wire it as a fallback chain so a diagram never fails
the build:
1. `merman` in-process, export-safe SVG → fill `Block::Diagram.svg`.
2. Optional network fallback: **Kroki** `POST https://kroki.io/mermaid/svg`
   (opt-in flag), prepending `%%{init: {"flowchart": {"htmlLabels": false},
   "htmlLabels": false}}%%` to force label-safe output.
3. Optional `--mermaid-backend=cli` shelling out to `mmdc` if on PATH.
4. Terminal fallback: today's verbatim-source rendering.

**Guardrails.** After any render, scan the SVG for `<foreignObject` and reject it
(Kindle/resvg drop those labels). The inline-SVG that comes back flows through
the same `SvgMode` machinery as artwork.

**Integration points:** best done as a post-parse transform in `lib::convert`
(walk the IR for `Block::Diagram` with empty `svg`, fill it) rather than inside
the parser, so it's cleanly feature-gated and the pure `parse_source` path stays
network-free. Pin the alpha version.

**Effort:** M (mostly plumbing + the fallback chain). **Risks:** merman is
alpha — parity gaps on dense diagrams; keep the code-block fallback wired.

---

## 3. BOLT fetching

> **✅ Done.** Route (a): a stable number→filename map, `model::bolt_filename`
> (also used by `Collection::external_url` for correct cross-ref links), plus
> `fetch::fetch_bolt` against the bolts repo root as `asset_base`. Validated on
> bolt-11 and bolt-3.

**Current state.** `Collection::Bolt` exists and the Markdown parser handles
BOLT files fine (plain GFM, no frontmatter). `fetch::fetch` returns
`Error::Unsupported("fetching BOLTs is not yet supported; use --input")`.

**Why deferred.** BOLT filenames embed a name (`00-introduction.md`,
`11-payment-encoding.md`), so a number alone can't build the raw URL, and
`raw.githubusercontent.com` can't be listed.

**Approach.** Two cheap options: (a) hardcode the small, stable BOLT number→
filename map (there are ~12) in `fetch.rs`; or (b) fetch the repo's `README.md`
once and parse its BOLT table for the filename. Prefer (a) for determinism, with
a comment to update it when BOLTs are added. `asset_base` = the bolts repo root.

**Effort:** S. **Risks:** the map goes stale when new BOLTs land (low-churn).

---

## 4. `--math svg` mode (Kindle-reliable math)

> **⏸ Deferred (blocked on a dependency).** The only pure-Rust LaTeX→SVG engine,
> RaTeX, is published as `ratex` **0.0.1** — far too immature to depend on. The
> approach below (a `--math <mathml|svg>` flag → `Options.math_mode` threaded into
> the parser, SVG math flowing through the existing inline-SVG machinery) still
> holds; it just needs a dependable engine first. MathML Core remains the default
> and is correct for Apple Books / Kobo / KOReader / Calibre.

**Current state.** Math is always MathML Core (`mathml.rs`), which renders in
Apple Books/Kobo/KOReader/Calibre but is inconsistent on Kindle.

**Why deferred.** MathML is the correct default; SVG math is only needed if
real-device Kindle testing disappoints. The corpus math is simple (Enhanced
Typesetting's best case), so start with MathML.

**Approach.** Add a `--math <mathml|svg>` CLI flag → `Options.math_mode`, threaded
into the Markdown parser. In `svg` mode, `mathml.rs` produces an SVG instead
(via **RaTeX**, the pure-Rust KaTeX engine, or hand layout) and `Block::Math` /
`Inline::Math` render through the existing inline-SVG machinery (like artwork),
with the LaTeX kept as the title/alt. Keep MathML the default.

**Effort:** M (RaTeX integration + a second render path). **Risks:** RaTeX
maturity; SVG math loses selectable text.

---

## 5. Raw-HTML `<table>` → real tables

> **✅ Done.** `markdown::html_block` now tries `parse_html_table` (via `roxmltree`,
> after light sanitizing — self-closing void tags, entity/`&` neutralization) and
> emits `Block::Table`, failing soft to tag-stripping. `<details>`/`<summary>` map
> to `Block::Aside` (handling comrak's habit of splitting `<details>` across
> multiple HTML blocks). Cell inlines reuse the Markdown link resolver.

**Current state.** `parse/markdown.rs::html_block` strips tags to text (`<sup>`,
`<sub>`, `<br>` inline cases are handled; block-level `<table>`/`<details>`
degrade to a text paragraph). ~40 EIP files embed a raw `<table>`.

**Approach.** In `html_block`, attempt to parse a well-formed `<table>` fragment
with `roxmltree` (already a dependency) and emit `Block::Table`; on any parse
failure, fall back to today's tag-stripping. Map `<details>`/`<summary>` →
`Block::Aside`. Keep it best-effort — "don't try to be a browser" (research §3.1).

**Effort:** S–M. **Risks:** raw HTML isn't always well-formed XML; the
roxmltree attempt must fail soft.

---

## 6. Smaller polish

> **◐ Partial.** 6b (untagged-fence language sniffing) is **done** —
> `markdown::sniff_language` conservatively routes obvious JSON and Solidity fences
> to the highlighter. 6a (GIF→PNG) and 6c (manifest properties) remain **deferred**
> — both low value (GIF is a valid EPUB3 core media type; the manifest-property gap
> is an `epub-builder` limitation readers don't care about).

- **GIF → PNG frame-1.** `assets.rs` currently passes GIFs through as
  `image/gif` (a valid EPUB3 core media type, so this is fine). If a reader
  mishandles animated GIFs, convert frame 1 to PNG at embed time.
- **Untagged-fence language sniffing.** 1914 EIP / 3839 ERC fences are untagged
  and render as verbatim SVG. Optionally sniff obvious JSON/Solidity in
  `code_block` and route to `highlight::highlight`.
- **Manifest `mathml` / `svg` property.** `epub-builder` can't emit content-
  document manifest properties, so a strict `epubcheck` warns on inline SVG and
  MathML pages (readers render both fine — see the inline-SVG note in the README).
  Tackling this means either post-processing the generated OPF or switching EPUB
  builders; low value given real-reader behavior.

---

## Suggested order

Items 1, 2, 3, 5, and 6b have been implemented (see the status markers above).
What's left, in priority order:

1. **`--math svg`** — only once a dependable pure-Rust LaTeX→SVG engine exists
   (RaTeX is 0.0.1 today) and if Kindle testing proves MathML inadequate.
2. Remaining polish (6a GIF→PNG, 6c manifest `mathml`/`svg` properties) as they
   surface on real documents.

Note on merman: pinned at `0.7.0` (its `render` feature pulls `merman-render`
0.7.0). It is alpha-lineage — watch for parity gaps on dense diagrams; the
verbatim-source fallback stays wired for anything that fails to render.
