# Research: Markdown support for EIPs, BIPs, and similar spec collections

*Researched 2026-07-24. Crate versions and repo facts verified against crates.io,
docs.rs, and by fetching raw files / full repo tarballs from GitHub.*

## Goal

Extend rfc2epub beyond IETF RFCs to markdown-based spec collections — Ethereum
EIPs/ERCs, Bitcoin BIPs, and later BOLTs/CAIPs — producing the same clean,
reflowable EPUBs. Markdown sources bring three new requirements the RFC
pipeline never had: real **images**, **mermaid diagrams**, and code blocks that
deserve **syntax highlighting**. The existing IR (`model::Document`) should be
generalized and shared, not forked.

## TL;DR recommendations

| Concern | Recommendation | Fallback / alternative |
|---|---|---|
| Markdown parsing | **comrak** (full AST, GFM, frontmatter node, GitHub-compatible header IDs) | pulldown-cmark (event stream, lighter) |
| Preamble parsing | Hand-rolled RFC-822 `key: value` parser (~30 lines) | gray_matter with custom engine |
| Syntax highlighting | **syntect + two-face**, class-based spans + our own grayscale/dark CSS | lumis (tree-sitter) if Solidity fidelity disappoints |
| Mermaid | **merman** (pure Rust, export-safe SVG mode) | Kroki POST API → plain code block |
| Math | **math-core** → MathML Core with LaTeX `alttext` | render source as code on conversion failure |
| BIP MediaWiki | Scope decision needed — see §3.2 (93% of BIPs are *not* markdown) | parse-wiki-text-2, or hand-rolled subset converter |

---

## 1. Source formats (verified against the real repos)

### 1.1 Ethereum EIPs and ERCs

- **EIPs**: `github.com/ethereum/EIPs`, files at `EIPS/eip-N.md` (941 files).
  Raw: `https://raw.githubusercontent.com/ethereum/EIPs/master/EIPS/eip-N.md`.
- **ERCs split out in 2023**: `github.com/ethereum/ERCs`, files at
  `ERCS/erc-N.md` (608 files). The EIPs repo keeps 365 tombstone stubs whose
  frontmatter is `status: Moved` and whose body links to the new location.
  **Fetcher rule**: try `EIPS/eip-N.md`; on `status: Moved`, refetch
  `ERCS/erc-N.md` from the ERCs repo.
- **Cross-repo path gotcha**: inside the ERCs repo, links and asset refs still
  use the `eip-` prefix (`../assets/eip-7401/…`, `./eip-N.md` — 2869
  occurrences, zero `erc-` ones) but the on-disk paths are `erc-`-prefixed.
  The converter must rewrite `eip-N` → `erc-N` when resolving paths within the
  ERCs repo (the Jekyll site build does the same).
- **Preamble**: `---`-delimited, specified by EIP-1 as *RFC 822 headers* (not
  YAML, though it happens to parse as YAML). Fields, in mandated order: `eip`,
  `title`, `description`, `author`, `discussions-to`, `status`,
  `last-call-deadline`, `type`, `category`, `created`, `requires`,
  `withdrawal-reason`. ERC files still use the `eip:` key. Author format:
  `Name (@github) <email>` variants, comma-separated on one line.
- **Content** (full-corpus grep): GFM tables in ~310 files, footnotes in 18,
  inline HTML (`<sup>`, `<details>`, raw `<table>`…) in ~40, LaTeX math in 20
  (11 EIPs, 9 ERCs — see §2.4). Images as `![alt](../assets/eip-N/foo.png)` — mix of PNG (139), SVG (58),
  JPG/JPEG (20), GIF (3), sometimes in nested subdirs. Cross-refs are relative
  (`[EIP-2718](./eip-2718.md)`).
- **Code fence languages** (fence counts, EIPs / ERCs): untagged 1914/3839,
  `python` ~487/13, `solidity` ~34/1769 (dominant in ERCs), `json` 115/215,
  `yaml` 3/137, `typescript` 56/65, `javascript` 33/103, shell 75/12, plus
  stragglers (`go`, `rust`, `cpp`, `asm`, `abnf`) and case-variants
  (`Solidity`, `JSON`) — treat tags case-insensitively.
- **Mermaid**: zero uses in ethereum/EIPs; exactly 3 ERC files use
  ` ```mermaid ` fences (erc-5883, erc-7638, erc-7715). Nice-to-have, not
  critical path.

### 1.2 Bitcoin BIPs — the format surprise

**196 of 210 BIPs are `.mediawiki`; only 14 are `.md`** (bip-0003, -0054,
-0077, -0095, -0346, -0348, -0349, -0379, -0434, -0442, -0446, -0448, -0449,
-0451). BIP 3 (the current process, replacing BIP 2) allows both formats and
new BIPs trend toward markdown, but "markdown support" alone covers ~7% of
BIPs. This is the main scope decision — see §3.2.

- Raw URL: `https://raw.githubusercontent.com/bitcoin/bips/master/bip-NNNN.{md,mediawiki}`
  (number zero-padded to 4; probe `.md` then `.mediawiki`).
- **Preamble**: RFC 822-style headers inside `<pre>…</pre>` (mediawiki) or a
  code fence (md), each line indented 2 spaces, multi-line values (Authors,
  Discussion) continued by deeper indentation. BIP-3 fields: `BIP`, `Layer`,
  `Title`, `Authors`, `Deputies`, `Status`, `Type`, `Assigned`, `License`,
  `Discussion`, `Version`, `Requires`, `Replaces`, `Proposed-Replacement`;
  older files use BIP-2 vocabulary (`Author` singular, `Created`,
  `Comments-URI`, `Post-History`, `Superseded-By`).
- **MediaWiki constructs actually used** (file counts across all 196): `<pre>`
  196, `{|` wiki tables 111, `<code>` 68, `<ref>` footnotes 53, `<tt>` 34,
  raw `<img src="bip-NNNN/foo.png">` 18, `<source lang="…">` 12 (mostly
  python), `[[File:…]]` 10, `<math>` **0**, templates `{{…}}` none. Standard
  `== Heading ==`, `'''bold'''`, `[url label]`, `*` lists. Assets live in
  `bip-XXXX/` dirs referenced relative to repo root.

### 1.3 Future targets (one line each)

- **BOLTs** (lightning/bolts): plain GFM, `NN-name.md`, no frontmatter — easy
  once markdown support lands.
- **CAIPs** (ChainAgnostic/CAIPs, branch `main`): EIP-clone format with
  `caip:` frontmatter — nearly free.
- **W3C specs**: HTML/Bikeshed — a different (HTML-first) pipeline; out of
  scope here.

---

## 2. Crate research

### 2.1 Markdown parsing → IR

The parser's job is to produce a tree/events we transform into our own
`Block`/`Inline` IR — we never want its HTML output. The ecosystem has
consolidated onto two serious options:

| | **comrak 0.54** (recommended) | pulldown-cmark 0.13 |
|---|---|---|
| Model | Full arena AST — recursive `match` into our IR | Pull events — manual stack machine (~150–300 lines) |
| Maintenance | Very active; used by crates.io, docs.rs, GitLab | Very active; used by mdBook, Zola, rustdoc, mdbook-epub |
| GFM | All 5 GFM extensions + footnotes, alerts, math, description lists | Via option flags; comparable coverage |
| Frontmatter | First-class `NodeValue::FrontMatter(String)` node | `ENABLE_YAML_STYLE_METADATA_BLOCKS` event (raw text) |
| Header anchors | **Auto-generated, GitHub-compatible IDs** | Only explicit `{#id}` attributes |
| Code fences | `NodeCodeBlock { info, literal }` | `CodeBlockKind::Fenced(info)` |
| Raw HTML | Typed `HtmlBlock` / `HtmlInline` nodes | `Event::Html` / `Event::InlineHtml` |
| MSRV / weight | 1.85 / heavier (arena, RefCell) | 1.71 / very light |

**Recommendation: comrak.** Tree→tree transformation is materially simpler
than reconstructing structure from a flat event stream (tables, footnote
definitions, and nested lists — all heavily used by EIPs — are the painful
cases). Its GitHub-compatible auto header IDs matter directly: EIP/ERC
cross-references link to GitHub-style `#section-anchors`, and matching them
makes our anchor map work unchanged. GFM-as-GitHub-renders-it is the reference
rendering for both repos. Perf difference is irrelevant at document sizes.

Rejected: `markdown` (markdown-rs — nice mdast tree but 15 months without a
release, no heading-anchor story), markdown-it-rust (dormant since 2024),
jotdown (Djot, wrong language).

**Frontmatter**: parse the raw text ourselves as ordered RFC-822 `key: value`
lines (~30 lines of code). EIP preambles are *specified* as RFC 822 and only
accidentally YAML — a real YAML engine chokes on edge cases like
`requires: 20, 155` and buys nothing. The same parser (plus
indented-continuation handling) covers BIP preambles. `gray_matter` (0.3.2,
alive) exists if we ever want a generic engine; the dedicated `matter` crate
is dead.

### 2.2 Syntax highlighting

EPUB readers don't run JavaScript (mdBook's highlight.js approach is
unavailable), so highlighting must happen at build time — pandoc's model:
`<span>`s with short CSS classes plus a stylesheet embedded in the EPUB.

| | **syntect 5.3 + two-face 0.5** (recommended) | lumis 0.12 (ex-autumnus) | tree-sitter-highlight |
|---|---|---|---|
| Grammars | Sublime `.sublime-syntax`, embedded, offline | tree-sitter, 110+ langs compiled in as C | BYO grammars + queries |
| Solidity | **Yes, via two-face** (bat's syntax set) | Yes (tree-sitter-solidity) | Via grammar crate |
| Class-based HTML | `ClassedHTMLGenerator` + CSS from theme or hand-written | `HtmlLinkedBuilder` + `CssBuilder`; multi-theme light/dark builder | `HtmlRenderer` w/ attribute callback |
| Build | Pure Rust possible (`fancy-regex`); light | Needs C compiler, slow first build (feature-flag per language) | C per grammar, lots of glue |
| Maintenance | Stable, "mostly complete", PRs reviewed | Very active (released this week) | Active (core tree-sitter) |

**Recommendation: syntect + two-face, class-based output, hand-written
stylesheet.** Two reasons specific to this project:

1. **E-ink/dark-mode themes.** Built-in RGB color themes are wasted on
   grayscale e-ink and fight dark mode. Class-based spans + our own ~40-line
   CSS (font-weight/font-style/gray levels, `currentColor`-relative shades, a
   dark-scheme block) matches the inline-SVG `currentColor` technique we
   already use for diagrams.
2. **Extensibility.** A custom `.sublime-syntax` (e.g. EVM assembly, ABNF) is
   an afternoon; a tree-sitter grammar is a project.

Caveat: verify two-face's Solidity grammar under `fancy-regex` (unmarked in
its exclusion table but untested); default `onig` is fine for a native CLI.
If Sublime-grammar Solidity fidelity proves inadequate on real ERC code,
**lumis** is the switch — the EPUB-side markup shape (spans + stylesheet)
stays identical. Rejected: inkjet (archived; author explicitly refuses
blockchain languages), syntastica (no Solidity, git-cloning build scripts),
synoptic (no HTML output, hand-written rules).

### 2.3 Mermaid rendering

EPUB = no JS, so mermaid must become static SVG/PNG at build time. Three
findings shape the plan:

1. **A serious pure-Rust renderer now exists: `merman`** (0.8.0-alpha.3,
   2026-07-09; stable 0.7.0). Headless reimplementation of Mermaid.js
   v11.16 — no browser, no Node. 23+ diagram families, parity-tested against
   3,500+ upstream SVG baselines, ported dagre/fCoSE layout engines, and —
   crucially — an **"export-safe" SVG mode that avoids `foreignObject`**,
   purpose-built for non-browser consumers. Alpha versioning; pin the version.
2. **The `foreignObject` landmine**: since mermaid v9.2, default flowchart
   output embeds labels as `<foreignObject><div>…</div></foreignObject>`.
   epubcheck flags it and Kindle/resvg-class renderers silently drop the
   labels. Any backend that runs real mermaid.js (Kroki, mermaid.ink, mmdc)
   must have `htmlLabels:false` forced — portably done by prepending
   `%%{init: {"flowchart": {"htmlLabels": false}, "htmlLabels": false}}%%`
   to the diagram source. Post-render, scan output for `<foreignObject` and
   reject if present.
3. **Everyone else needs a browser.** mermaid.js's render path hard-requires a
   DOM (`getBBox()` etc.), so JS-engine embedding (QuickJS/Boa/deno_core) is a
   dead end; mdbook-mermaid is client-side JS (useless for EPUB); pandoc's
   mermaid-filter and mdbook-mermaid-ssr shell out to Node/Chromium.

**Recommended fallback chain** (never fail the build over a diagram):

1. `merman` in-process, export-safe SVG, fed through the existing
   inline-SVG/`SvgMode` machinery (`resvg` is already a dep if rasterizing is
   ever needed).
2. Network fallback: **Kroki** — `POST https://kroki.io/mermaid/svg` with
   plain-text body (no encoding dance), htmlLabels-init prepended;
   self-hostable Docker image; mermaid.ink as a second remote.
3. Optional opt-in: shell out to `mmdc` if on PATH (`--mermaid-backend=cli`).
4. Terminal fallback: render the mermaid source as a normal code block with a
   build warning — exactly what GitHub showed readers before mermaid support.

Given only 3 ERC files use mermaid today, even shipping with just (1) + (4)
is defensible.

### 2.4 Math rendering

**Corpus census** (grep over the cached repo tarballs): 20 files use LaTeX
math (11 EIPs including eip-2982/-7378/-7999, 9 ERCs including erc-5115).
The constructs are uniformly AMS-lite: `\frac`, `\sum` with limits,
sub/superscripts, `\bar`, `\text`/`\textit`, `\left(\right)`, `\lfloor`,
`\max`/`\min`, `\times`, Greek — plus exactly **one** environment in the whole
corpus (`\begin{cases}` in erc-7832). No TikZ, no arrays, no custom macros.

**Crate**: **math-core 0.7** (tmke8/math-core) — the actively maintained
successor of the dead `latex2mathml` (releases monthly through July 2026).
Pure Rust, one function call per formula, no fonts, no JS engine. Emits
**MathML Core** (the subset browsers/readers actually implement) and covers
the full corpus profile above. Rejected: `pulldown-latex` (stale ~20 months),
`latex2mathml` (dead 2020; math-core *is* its maintained fork), `katex`
JS-bindings crate (drags in a JS engine and needs KaTeX CSS + 19 web fonts —
unusable in EPUB), ReX (unpublished, "not intended to be used"), Typst/mitex
(entire compiler for a few formulas). If real LaTeX→SVG typesetting is ever
needed, RaTeX (pure-Rust KaTeX engine, active) is the only credible option.

**Reader support for MathML** (2026): Apple Books, Kobo (kepub renderer
only), KOReader (crengine grew native MathML), and Calibre all render MathML
Core. **Kindle is the weak spot**: KDP's Enhanced Typesetting officially
supports MathML but behavior on sideloaded/Send-to-Kindle EPUBs is
inconsistent; community experience says SVG images are the reliable path
there. Pandoc's EPUB default is likewise MathML, with SVG (`--gladtex`) as
the Kindle-safe escape hatch.

**Plan**: emit `<math>` (MathML Core) with the raw LaTeX preserved in
`alttext`, via comrak's math extension (`$…$` / `$$…$$` → typed math nodes —
its GitHub-compatible delimiter rules also dodge the dollar-literal false
positive in erc-5115, where `$10 million … $` would naively parse as math).
On math-core conversion failure, fall back to rendering the source as inline
code — never fail the build. Simple `mfrac`/`msub` constructs are Enhanced
Typesetting's best case, so accept imperfect Kindle rendering initially; if
it proves bad in practice, a later `--math svg` mode can reuse the existing
inline-SVG machinery (RaTeX or hand layout). EPUB manifest note: content
documents containing MathML should declare the `mathml` property — check
what `epub-builder` allows (same class of limitation as the `svg` property).

### 2.5 MediaWiki (for BIPs) — see §3.2

- `parse-wiki-text-2` 0.2.0 (maintained fork of the dead `parse_wiki_text`,
  updated 2024): wikitext AST, would still need handling for the HTML-ish
  constructs BIPs embed (`<pre>`, `<source>`, `<ref>`, `<img>`).
- Shelling out to pandoc (`-f mediawiki`) handles everything but adds an
  external-binary dependency.
- The BIP corpus uses a *small, closed* construct set (no templates, no math —
  verified §1.2), so a hand-rolled subset converter is genuinely viable.

---

## 3. Design

### 3.1 Generalizing the IR

The `Block`/`Inline` layer is already format-neutral; the RFC-specific parts
are `Document`'s header fields. Proposed changes to `model.rs`:

**Document metadata** — replace RFC-specific fields with a general identity +
per-collection extras:

```rust
pub struct Document {
    /// e.g. DocId { collection: Rfc, number: 9110 } → "RFC 9110"
    pub id: Option<DocId>,
    pub title: String,
    pub short_title: Option<String>,
    pub authors: Vec<Author>,
    pub date: Option<String>,
    /// Generalizes RFC `category`: "Internet Standard", "Standards Track: Core",
    /// "Consensus (soft fork)" …
    pub status: Option<String>,
    /// Generalizes obsoletes/updates/requires into labeled relations:
    /// ("Obsoletes", [..]), ("Requires", [..]), ("Replaces", [..]).
    pub relations: Vec<(String, Vec<DocId>)>,
    pub abstract_: Vec<Block>,   // EIP `description:` maps here
    pub keywords: Vec<String>,
    pub sections: Vec<Section>,
    pub source: SourceKind,      // grows Markdown and Mediawiki variants
}

pub struct DocId { pub collection: Collection, pub number: u32 }
pub enum Collection { Rfc, Eip, Erc, Bip /*, Bolt, Caip …*/ }
```

`Author` grows an optional `link` (EIP authors carry GitHub handles/emails).
Extra preamble fields that don't fit (discussions-to, license, created …) can
go in a `Vec<(String, String)>` rendered as a metadata table on the title
page.

**Block/Inline additions** for markdown content:

```rust
enum Block {
    // … existing …
    /// An image with alt text and optional caption. `resource` names an
    /// asset embedded in the EPUB (fetched at build time).
    Figure { resource: String, alt: String, caption: Option<Vec<Inline>> },
    /// A rendered diagram (SVG), flowing through the existing SvgMode
    /// machinery like Artwork does today.
    Diagram { svg: String, source: String },
    /// Pre-highlighted code: spans of (style-class, text) per line, produced
    /// by the highlighter at parse/render time. Plain Code remains the
    /// unhighlighted path.
    HighlightedCode { language: String, html: String },
    /// Display math ($$…$$): MathML Core markup, LaTeX source kept for
    /// alttext / fallback.
    Math { mathml: String, source: String },
    ThematicBreak,
}

enum Inline {
    // … existing …
    Strikethrough(Vec<Inline>),
    Image { resource: String, alt: String },
    FootnoteRef(String),   // + footnote definitions collected per-section
    Math { mathml: String, source: String },   // inline $…$
}
```

(Exact shape of `HighlightedCode` — pre-rendered XHTML vs. structured spans —
can be decided at implementation time; pre-rendered XHTML string is simplest
and mirrors how `Diagram`/svg flows.)

Footnotes: comrak gives definitions as AST nodes; render them as an
end-of-section list with backlinks, GitHub-style.

Raw inline HTML in EIPs (~40 files): map the small observed subset
(`<sup>`, `<sub>`, `<br>`, `<details>` → Aside, raw `<table>` → Table via a
mini HTML pass) and degrade the rest to text; don't try to be a browser.

### 3.2 Scope decision: BIP MediaWiki

Options, in increasing effort:

- **A. Markdown-only BIPs** (14 of 210, but including recent/important ones
  like BIP-3 and the CTV/TXHASH-era ones). Honest but disappointing —
  Taproot (341), SegWit (141), BIP-32/39 are all mediawiki.
- **B. Hand-rolled MediaWiki-subset parser** targeting exactly the verified
  construct list (§1.2): headings, bold/italic, links, lists, `{|` tables,
  `<pre>`, `<source>`, `<code>`/`<tt>`, `<ref>` footnotes, images. It becomes
  a third parser emitting the same IR, symmetric with `parse/text.rs` (which
  already proves the "parse a quirky legacy format into the IR" pattern
  works). Estimated effort: comparable to the existing text parser.
- **C. `parse-wiki-text-2`** for the wikitext skeleton + custom handling of
  embedded HTML constructs. Less code than B but adds a dependency of
  middling maintenance, and the HTML-construct handling is still on us.
- **D. pandoc at runtime** — lowest effort, best coverage, but an external
  binary dependency clashes with the project's self-contained character.

**Recommendation: A first (markdown BIPs ship with the EIP work basically for
free), then B as its own milestone.** B over C is a judgment call; the
verified construct census makes B's scope concrete and bounded.

### 3.3 Fetch layer

Generalize `fetch.rs` from RFC-only to per-collection sources sharing the
HTTP/caching plumbing:

- CLI grows collection-qualified ids: `rfc2epub eip-1559`, `bip-341`,
  `rfc-9110` (bare number stays RFC for compat).
- EIP/ERC resolution: `EIPS/eip-N.md` → on `status: Moved` follow to
  `ERCS/erc-N.md`; inside ERC content rewrite `eip-N` → `erc-N` paths.
- BIP resolution: zero-pad to 4, probe `.md` then `.mediawiki`.
- **Asset fetching**: markdown parse yields referenced image paths; fetcher
  downloads them relative to the raw URL base (with the ERC rewrite), caches
  alongside the document, and the EPUB builder embeds them via the existing
  `add_resource` path. SVG assets embed as-is; GIFs (3 exist) get frame-1
  PNG conversion or pass-through.
- Cross-references to *other* documents (`./eip-2718.md`) render as external
  links to `https://eips.ethereum.org/EIPS/eip-2718` (an EPUB contains one
  document); same-document `#anchors` use the existing anchor map, which
  works because comrak's auto IDs match GitHub's.

### 3.4 Proposed dependency additions

| Crate | Version | Purpose | Notes |
|---|---|---|---|
| comrak | 0.54 | Markdown → AST | MSRV 1.85 = ours |
| syntect | 5.3 | Highlighting engine | default `onig`; consider `fancy-regex` later |
| two-face | 0.5 | Extra syntaxes (Solidity) + curated set | ~0.6 MiB |
| merman | pin 0.7/0.8-alpha | Mermaid → export-safe SVG | behind a feature flag if binary size matters |
| math-core | 0.7 | LaTeX → MathML Core | pure Rust, no fonts/JS |
| parse-wiki-text-2 | 0.2 | *(only if §3.2 option C)* | |

No new C dependencies; everything works offline after fetch (mermaid network
fallback is optional/graceful).

---

## 4. Risks & open questions

1. **merman is alpha.** Parity gaps exist (dense architecture diagrams).
   Mitigation: pin version, code-block fallback, only 3 ERC files affected
   today. Watch for 0.8 stable.
2. **Solidity via Sublime grammar** may mis-highlight modern syntax; test on
   erc-20/erc-721/erc-4626 code early; lumis is the prepared exit.
3. **Untagged fences dominate** (1914 in EIPs, 3839 in ERCs). They render as
   plain `Code` — fine. Optional later: sniff obvious JSON/Solidity.
4. **MathML on Kindle** is the one shaky reader (§2.4): Enhanced Typesetting
   claims support but sideloaded behavior is inconsistent. Corpus math is
   simple (ET's best case); if real-device testing disappoints, add a
   `--math svg` mode reusing the inline-SVG machinery.
5. **`epub-builder` limitations** already noted for SVG (`svg` manifest
   property); highlighted-code CSS and image embedding don't add new
   conformance issues.
6. **Preamble drift**: eipw lints EIP preambles, but historical files have
   strays (`updated:`); parser must be lenient (unknown keys → metadata
   table, never a hard error).

## 5. Suggested phasing

1. **IR generalization** (`DocId`/`Collection`, `status`, `relations`, new
   Block/Inline variants) + rename-safe refactor of existing parsers/renderer.
   No behavior change for RFCs.
2. **Markdown parser** (comrak → IR) + RFC-822 preamble parser + EIP/ERC
   fetcher with Moved-resolution and asset download. Ships: EIPs, ERCs, and
   the 14 markdown BIPs (frontmatter dialect differs; content pipeline
   identical).
3. **Syntax highlighting** (syntect/two-face, class CSS in `render/css.rs`)
   and **math** (comrak math extension + math-core → MathML, code fallback) —
   both are localized render-side features.
4. **Mermaid** via merman + fallback chain.
5. **BIP MediaWiki parser** (hand-rolled subset per §1.2 census) — unlocks
   the remaining 93% of BIPs.
6. Later: BOLTs/CAIPs (near-free), GIF handling polish, `--math svg` if
   Kindle testing warrants it.
