# rfc2epub

Convert IETF RFCs into clean, **reflowable EPUB** files that read well on Kindle
and other e-readers.

RFCs are published as fixed 72-column text with page breaks — painful on a 6"
screen. `rfc2epub` reflows the prose to your device while keeping packet
diagrams, ABNF, and code **verbatim and monospaced** so they never get mangled.

```console
$ rfc2epub 9110
✓ RFC 9110 → ./rfc9110.epub

$ rfc2epub 8446 791 2119 --out-dir ~/rfcs
✓ RFC 8446 → ~/rfcs/rfc8446.epub
✓ RFC 791 → ~/rfcs/rfc791.epub
✓ RFC 2119 → ~/rfcs/rfc2119.epub
```

Then use **Send to Kindle** (Amazon accepts EPUB directly) or copy the file to
any reader.

## Why EPUB?

EPUB is an open W3C standard — mechanically just a ZIP of XHTML + CSS — and it
reflows. That combination is exactly what a small e-reader screen needs, and
Kindle has accepted EPUB since 2022.

## How it works

```
fetch (rfc-editor.org, cached) → parse → IR → render XHTML → assemble EPUB
```

The key idea is a single intermediate representation (IR) produced by two
parsers and consumed by one renderer:

| Input | When used | Fidelity |
|-------|-----------|----------|
| **xml2rfc v3** | Modern RFCs (~2020+) that publish canonical XML | High — real section/prose/artwork/code structure |
| **Plain text** | Everything older (e.g. RFC 791) | Heuristic reconstruction; diagrams kept verbatim |

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

## Usage

```
rfc2epub [OPTIONS] <RFC>...

Arguments:
  <RFC>...  RFC numbers to convert, e.g. 9110 8446 791

Options:
  --input <FILE>     Convert a local RFC source file instead of fetching
  -o, --output <FILE>  Write to this exact file (single RFC only)
  -d, --out-dir <DIR>  Output directory [default: .]
  -f, --format <auto|xml|text>  Source format preference [default: auto]
      --svg-mode <inline|card>  Diagram theme handling [default: inline]
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

## Project layout

```
crates/
  rfc2epub/       library core (fetch, parse, render)
  rfc2epub-cli/   the `rfc2epub` command-line tool
```

The library is usable on its own:

```rust
let opts = rfc2epub::Options::default();
rfc2epub::convert_rfc(9110, std::path::Path::new("rfc9110.epub"), &opts)?;
```

## Status & limitations

Working today: XML path (full structure, metadata, tables, lists, code) and a
text fallback that recovers sections, nesting, prose, and diagrams.

Known rough edges in the **text** path (older RFCs), inherent to reconstructing
structure from plain text:

- Front matter (status-of-memo boilerplate) is grouped loosely.
- Very unusual heading styles may be missed.
- Cross-references are rendered as plain text, not in-book links (both paths).

Contributions and bug reports on specific RFCs are welcome.

## License

MIT OR Apache-2.0
