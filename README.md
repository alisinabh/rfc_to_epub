# rfc2epub

Convert IETF RFCs — and Markdown-based spec collections like Ethereum
**EIPs/ERCs**, Bitcoin **BIPs**, and **CAIPs** — into clean, **reflowable EPUB**
files that read well on Kindle and other e-readers.

RFCs are published as fixed 72-column text with page breaks — painful on a 6"
screen. `rfc2epub` reflows the prose to your device while keeping packet
diagrams, ABNF, and code **verbatim and monospaced** so they never get mangled.
Markdown specs bring their own needs — real **images**, **syntax-highlighted**
code, and **math** — all handled at build time so the EPUB stays self-contained
and JavaScript-free.

```console
$ rfc2epub 9110
✓ RFC 9110 → ./rfc9110.epub

$ rfc2epub eip-1559 erc-721 bip-3 --out-dir ~/specs
✓ EIP-1559 → ~/specs/eip-1559.epub
✓ ERC-721 → ~/specs/erc-721.epub
✓ BIP 3 → ~/specs/bip-3.epub
```

Then use **Send to Kindle** (Amazon accepts EPUB directly) or copy the file to
any reader.

Documents are named by a bare RFC number (`9110`) or a collection-qualified id
(`eip-1559`, `erc-20`, `bip-341`, `rfc-8446`, `caip-2`). ERC-20 style ids that
now live in the ERCs repo are followed automatically (the EIPs repo keeps a
`status: Moved` tombstone that redirects the fetch).

## Pre-built RFCs

Don't want to install anything? Every IETF RFC is already converted and waiting
at **<https://alisinabh.github.io/rfc_to_epub/>** — search by number or title,
download one, or grab the lot as a single ZIP. A weekly job reconverts whatever
is new, so RFCs published this week appear at the top of the page on their own.

## Why EPUB?

EPUB is an open W3C standard — mechanically just a ZIP of XHTML + CSS — and it
reflows. That combination is exactly what a small e-reader screen needs, and
Kindle has accepted EPUB since 2022.

## How it works

```
fetch (rfc-editor.org, cached) → parse → IR → render XHTML → assemble EPUB
```

The key idea is a single intermediate representation (IR) produced by several
parsers and consumed by one renderer:

| Input              | When used                                                            | Fidelity                                                                                          |
| ------------------ | -------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| **xml2rfc v3**     | Modern RFCs (~2020+) that publish canonical XML                      | High — real section/prose/artwork/code structure                                                  |
| **Plain text**     | Everything older (e.g. RFC 791)                                      | Heuristic reconstruction; diagrams kept verbatim                                                  |
| **Markdown (GFM)** | EIPs, ERCs, CAIPs, BOLTs, and Markdown BIPs                          | High — full AST, GitHub-compatible anchors, images/highlighting/math                              |
| **MediaWiki**      | The ~93% of BIPs still in `.mediawiki` (Taproot, SegWit, BIP-32/39…) | Good — hand-rolled subset: headings, tables, `<ref>` footnotes, `<source>` code, images, emphasis |

The IR is format-neutral: only [`Collection`](crates/rfc2epub/src/model.rs)
knows how an id is spelled and where it lives on the web. RFC output is byte-for-
byte the same as before.

By default (`--format auto`) it fetches the XML, verifies it is really v3, and
falls back to the published text otherwise. The IR's central distinction is
reflowable **prose** vs. verbatim **artwork/code**, which is what protects ASCII
diagrams on narrow screens.

### Diagrams that fit any screen

RFC artwork is up to 72 monospace columns wide — more than fits a phone-sized
e-reader at its default font, and reading systems (notably Kindle) ignore
`overflow-x`, so a plain `<pre>` gets soft-wrapped into nonsense.

Instead, every diagram/code block is rendered as a **scalable SVG image** laid
out on a fixed character grid and referenced with `<img … style="max-width:100%">`.
The reader scales the whole vector down to the page width (and caps it at natural
size on wide screens), so the monospace grid is preserved exactly and can never
wrap or clip. Columns stay aligned across lines via per-line `textLength`. The
figures are written as conformant image resources — no inline SVG, no manifest
hacks — which also renders more reliably on Kindle.

Because a referenced (`<img>`) SVG is rendered in an isolated context — host CSS
and even `prefers-color-scheme` may not reach it (Apple Books, for one, renders
it in light mode regardless of the page theme) — each figure draws its own light
"card" background behind dark text. That stays legible on any page color; a
`prefers-color-scheme: dark` rule inside the SVG is kept as a bonus for readers
that do propagate the theme.

### Cover & metadata

Each book gets a generated **cover image**: the RFC number, full title (auto-wrapped
and sized), authors, category badge, and date on a clean designed background. It's
rendered from SVG to PNG (via `resvg`, with two small bundled Roboto faces) so it
shows up as the library/shelf thumbnail — Kindle included.

EPUB metadata is filled in too: `dc:title` (`RFC N: Title`), one `dc:creator` per
author, `dc:description` (the abstract), `dc:subject` keywords, and a stable
`urn:ietf:rfc:N` identifier. For the text path, authors (from the "Authors'
Addresses" section), the date, and the RFC number are recovered heuristically.

## Spec collections (EIPs, ERCs, BIPs, CAIPs)

Markdown specs are parsed with a full GitHub-flavored-Markdown AST, so structure,
tables, footnotes, task lists, and description lists all survive. Three things
are done at **build time** (EPUB readers run no JavaScript):

- **Images** referenced by the Markdown are downloaded, cached, and embedded as
  EPUB resources; `<img>` links are rewritten to the in-book copy. Downloads that
  fail degrade to the image's alt text rather than breaking the build.
- **Fenced code** is syntax-highlighted into class-based `<span>`s (via `syntect`
  - `two-face`, so Solidity, TypeScript, JSON, Python, … all work), styled by an
    embedded stylesheet that adapts to light, dark, and grayscale e-ink.
- **Math** (`$…$` / `$$…$$`) is converted to **MathML Core** with the original
  LaTeX kept as `alttext`. Apple Books, Kobo, KOReader, and Calibre render it;
  Kindle's support is inconsistent (a known reader limitation).

The **preamble** (the `---` frontmatter, or the indented code-fence header that
Markdown BIPs use) is parsed as ordered RFC-822 headers per EIP-1 / BIP-3: the
id, title, authors (with GitHub/email links), status, and `requires`/`replaces`
relations map to first-class fields; anything else becomes a metadata table on
the title page. In-document `#section` links resolve to in-book anchors (the
GitHub-compatible slugs match), and links to _other_ documents (`./eip-2718.md`)
become canonical web links, since an EPUB holds a single document.

Both **Markdown and MediaWiki BIPs** are supported: the fetcher probes
`bip-NNNN.md` then `bip-NNNN.mediawiki`, and the MediaWiki parser handles the
closed construct set BIPs actually use — `== headings ==`, `{| wiki tables |}`,
`<ref>` footnotes, `<source lang>` highlighted code, `<pre>` blocks, images, and
`'''bold'''`/`''italic''`/links — emitting the same IR as everything else.
**BOLTs** (Lightning) fetch by number via a small filename map (`bolt-11`).

**Mermaid** diagrams are rendered to SVG **in process** with
[`merman`](https://crates.io/crates/merman) (pure Rust, export-safe output with
no `<foreignObject>`, so labels survive on Kindle/`resvg`). It is the default
`mermaid` Cargo feature; build `--no-default-features` for a leaner binary that
shows a diagram's source verbatim instead. The source is normalized first (a
stray `%%` comment before the diagram-type line, which mermaid rejects, is
dropped). For the occasional diagram merman can't render but real mermaid can
(e.g. sequence-diagram actor names with spaces), `--online-mermaid` opts into a
network fallback that renders it via [Kroki](https://kroki.io) — off by default
since it sends the diagram source to a third-party service.

## Usage

```
rfc2epub [OPTIONS] <DOC>...

Arguments:
  <DOC>...  Documents to convert: a bare RFC number (9110) or a collection-qualified
            id (eip-1559, erc-20, bip-341, bolt-11, rfc-8446, caip-2)

Options:
  --input <FILE>     Convert a local source file instead of fetching (.xml/.txt/.md/.mediawiki)
  -o, --output <FILE>  Write to this exact file (single RFC only)
  -d, --out-dir <DIR>  Output directory [default: .]
  -f, --format <auto|xml|text>  Source format preference [default: auto]
      --svg-mode <inline|card>  Diagram theme handling [default: inline]
      --online-mermaid  Render mermaid merman can't handle via kroki.io (opt-in network)
      --no-page-breaks  Do not reproduce the source's original page breaks
      --no-cache     Do not read or write the download cache
  -q, --quiet        Suppress progress output
```

### Diagram theme modes (`--svg-mode`)

- **`inline`** (default): diagrams are embedded as inline SVG using
  `currentColor`, so they follow the reader's light/dark theme. This looks best
  in practice, including on Kindle. The one caveat is strictness: inline SVG is
  not strictly EPUB3-conformant without a manifest `svg` property (which the
  underlying builder can't emit), so a strict `epubcheck` will complain even
  though readers render it fine.
- **`card`**: each diagram is a referenced SVG image with its own light
  background — self-contained and fully `epubcheck`-clean, but it stays a light
  card on a dark reader page instead of following the theme. Use it if a
  particular reader mishandles inline SVG or you need conformant output.

### Page breaks (`--no-page-breaks`)

By default the original document's pagination is reproduced as EPUB page breaks,
so each RFC page starts fresh on the reader. Pass `--no-page-breaks` for a fully
continuous flow. This only affects **plain-text** sources — xml2rfc v3 has no
page concept, so XML-sourced RFCs are unpaginated either way.

## Project layout

```
crates/
  rfc2epub/       library core
    model.rs      the format-neutral IR (Document, Block, Inline, Collection)
    fetch.rs      per-collection sources + caching; assets.rs embeds images
    parse/        xml.rs, text.rs, markdown.rs (comrak), preamble.rs (RFC-822)
    highlight.rs  syntect + two-face class-based highlighting
    mathml.rs     LaTeX → MathML Core (math-core)
    render/       xhtml.rs, css.rs, svg.rs, cover.rs, epub.rs
  rfc2epub-cli/   the `rfc2epub` command-line tool
```

The library is usable on its own:

```rust
use rfc2epub::{convert, DocSpec, Options};
use rfc2epub::model::Collection;

let opts = Options::default();
convert(DocSpec::new(Collection::Eip, 1559), "eip-1559.epub".as_ref(), &opts)?;
// or the RFC convenience wrapper:
rfc2epub::convert_rfc(9110, "rfc9110.epub".as_ref(), &opts)?;
```

## Status & limitations

Working today: RFCs via the XML path (full structure, metadata, tables, lists,
code) and a text fallback; Markdown spec collections (EIPs, ERCs, CAIPs, BOLTs,
Markdown BIPs) with images, syntax highlighting, math, footnotes, and
cross-references; MediaWiki BIPs; and in-process mermaid rendering.

Known rough edges in the **text** path (older RFCs), inherent to reconstructing
structure from plain text: front matter is grouped loosely, unusual heading
styles may be missed, and cross-references stay plain text.

For **spec collections**: raw block-level HTML is handled for the common cases
(`<table>` → a real table, `<details>`/`<summary>` → an aside, `<br>`/`<sup>`)
and anything else degrades to text; MathML rendering depends on the reader
(weakest on Kindle) — an SVG-math mode is still deferred (no dependable pure-Rust
LaTeX→SVG engine yet). Requires Rust ≥ 1.95 for the default `mermaid` feature.

Contributions and bug reports on specific documents are welcome.

## License

MIT
