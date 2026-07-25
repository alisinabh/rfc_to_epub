//! Hand-rolled parser for the **MediaWiki** subset used by Bitcoin BIPs into the
//! shared [`Document`] IR.
//!
//! 196 of 210 BIPs are `.mediawiki`, not Markdown — so a real (if small) third
//! parser is needed to reach Taproot (341), SegWit (141), BIP-32/39, and the
//! rest. It is symmetric with [`text`](super::text): "parse a quirky legacy
//! format into the IR." The target is the *closed, verified* construct set BIPs
//! actually use (no templates, no `<math>`):
//!
//! | Construct | Maps to |
//! |---|---|
//! | first `<pre>…</pre>` (RFC-822 preamble) | [`preamble`](super::preamble) |
//! | other `<pre>…</pre>` | [`Block::Code`] |
//! | `{\| … \|}` wiki tables | [`Block::Table`] |
//! | `<source lang="…">` / `<syntaxhighlight>` | highlighted code |
//! | `<code>` / `<tt>` | [`Inline::Code`] |
//! | `<ref>` footnotes | [`Inline::FootnoteRef`] + a trailing Footnotes section |
//! | `[[File:…]]`, raw `<img src="…">` | [`Block::Figure`] |
//! | `== Heading ==`, `'''bold'''`, `''italic''`, `[url label]`, `*`/`#` lists | sections / emphasis / links / lists |
//!
//! The preamble → metadata mapping is shared with the Markdown parser via
//! [`super::preamble::apply_preamble`], and cross-document wiki links
//! (`[[bip-0032.mediawiki|BIP-32]]`) reuse the Markdown parser's
//! [`rewrite_doc_link`](super::markdown::rewrite_doc_link).

use std::collections::HashMap;
use std::sync::OnceLock;

use comrak::Anchorizer;
use regex::Regex;

use super::markdown::rewrite_doc_link;
use crate::error::{Error, Result};
use crate::model::{
    Block, Collection, DefEntry, Document, Inline, List, Section, SourceKind, Table,
};

/// Parse a MediaWiki `body`. `collection` is the caller's hint (always
/// [`Collection::Bip`] in practice); `number` is a fallback id.
pub fn parse(body: &str, collection: Option<Collection>, number: Option<u32>) -> Result<Document> {
    // Drop HTML comments up front; they can appear anywhere and never carry
    // content we render.
    let body = strip_comments(body);

    // The BIP preamble sits inside the first `<pre>…</pre>` as RFC-822 headers.
    let (preamble, body) = extract_preamble(&body);
    let preamble = preamble.unwrap_or_default();

    let mut doc = Document {
        source: SourceKind::Mediawiki,
        ..Default::default()
    };
    super::preamble::apply_preamble(&mut doc, &preamble, collection, number);

    let mut parser = MwParser::default();
    let lines: Vec<&str> = body.lines().collect();
    let items = parser.parse_body(&lines);

    let mut sections = build_sections(items, &doc.title);

    // Footnotes (`<ref>` bodies) become a trailing section, anchored `fn-{name}`
    // to match the renderer's footnote scheme — identical to the Markdown path.
    if !parser.footnotes.is_empty() {
        let entries: Vec<DefEntry> = parser
            .footnotes
            .iter()
            .enumerate()
            .map(|(i, (name, description))| DefEntry {
                anchor: Some(format!("fn-{name}")),
                term: vec![Inline::text(format!("{}.", i + 1))],
                description: description.clone(),
            })
            .collect();
        sections.push(Section {
            number: None,
            title: "Footnotes".into(),
            id: "footnotes".into(),
            blocks: vec![Block::DefinitionList(entries)],
            subsections: Vec::new(),
        });
    }

    if doc.title.is_empty() {
        doc.title = sections
            .first()
            .map(|s| s.title.clone())
            .or_else(|| doc.id.map(|d| d.label()))
            .unwrap_or_else(|| "Untitled".into());
    }

    if sections.is_empty() {
        return Err(Error::Parse("MediaWiki document had no content".into()));
    }
    doc.sections = sections;
    Ok(doc)
}

/// A top-level document item: a heading (section boundary) or a content block.
enum Item {
    Heading { level: u8, title: String },
    Block(Block),
}

/// Stateful parser: the only state is the accumulating footnote table, threaded
/// through inline parsing so a `<ref>` anywhere registers a footnote.
#[derive(Default)]
struct MwParser {
    /// `(anchor-name, definition blocks)` in first-appearance order; the index
    /// into this vec + 1 is the footnote's display number.
    footnotes: Vec<(String, Vec<Block>)>,
    /// Named-ref reuse: `<ref name="x"/>` points back at an earlier definition.
    ref_index: HashMap<String, usize>,
    auto_ref: usize,
}

impl MwParser {
    fn parse_body(&mut self, lines: &[&str]) -> Vec<Item> {
        let mut items = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let lead = lines[i].trim();
            if lead.is_empty() {
                i += 1;
                continue;
            }
            // Magic words and lone reference markers: drop.
            if is_magic_word(lead) || lead.starts_with("<references") {
                i += 1;
                continue;
            }
            // Horizontal rule.
            if is_hr(lead) {
                items.push(Item::Block(Block::ThematicBreak));
                i += 1;
                continue;
            }
            // Heading.
            if let Some((level, title)) = parse_heading(lead) {
                items.push(Item::Heading {
                    level,
                    title: strip_markup(&title),
                });
                i += 1;
                continue;
            }
            // `<pre>` code (the preamble pre was already removed).
            if opens_tag(lead, "pre") {
                let (inner, _attrs, next) = consume_tag_block(lines, i, "pre");
                items.push(Item::Block(Block::Code {
                    text: inner,
                    language: None,
                }));
                i = next;
                continue;
            }
            // `<source>` / `<syntaxhighlight>` highlighted code.
            if let Some(tag) = ["source", "syntaxhighlight"]
                .into_iter()
                .find(|t| opens_tag(lead, t))
            {
                let (inner, attrs, next) = consume_tag_block(lines, i, tag);
                items.push(Item::Block(highlighted_code(&inner, &attrs)));
                i = next;
                continue;
            }
            // Wiki table.
            if lead.starts_with("{|") {
                let (block, next) = self.consume_table(lines, i);
                items.push(Item::Block(block));
                i = next;
                continue;
            }
            // A line that is *only* an image → a figure.
            if let Some(fig) = figure_line(lead) {
                items.push(Item::Block(fig));
                i += 1;
                continue;
            }
            // List (bullets, numbers, or definitions).
            if starts_list(lead) {
                let (block, next) = self.consume_list(lines, i);
                items.push(Item::Block(block));
                i = next;
                continue;
            }
            // Otherwise: a paragraph, accumulated until a blank line or the next
            // block construct.
            let (block, next) = self.consume_paragraph(lines, i);
            items.push(Item::Block(block));
            i = next;
        }
        items
    }

    fn consume_paragraph(&mut self, lines: &[&str], start: usize) -> (Block, usize) {
        let mut buf: Vec<&str> = Vec::new();
        let mut i = start;
        while i < lines.len() {
            let lead = lines[i].trim();
            if lead.is_empty() {
                break;
            }
            if i > start && is_block_start(lead) {
                break;
            }
            buf.push(lead);
            i += 1;
        }
        let joined = buf.join(" ");
        (Block::Paragraph(self.parse_inlines(&joined)), i)
    }

    fn consume_list(&mut self, lines: &[&str], start: usize) -> (Block, usize) {
        let mut raw: Vec<(String, String)> = Vec::new();
        let mut i = start;
        while i < lines.len() {
            let lead = lines[i].trim();
            let marker_len = lead
                .chars()
                .take_while(|c| matches!(c, '*' | '#' | ':' | ';'))
                .count();
            if marker_len == 0 {
                break;
            }
            let marker: String = lead.chars().take(marker_len).collect();
            let content = lead[marker_len..].trim().to_string();
            raw.push((marker, content));
            i += 1;
        }
        let all_def = raw
            .iter()
            .all(|(m, _)| m.chars().all(|c| c == ';' || c == ':'));
        let block = if all_def {
            self.build_def_list(&raw)
        } else {
            self.build_list_level(&raw, 0)
        };
        (block, i)
    }

    /// Build one nesting level of a `*`/`#` list. Items whose marker is exactly
    /// `depth + 1` long belong to this level; longer-markered items that follow
    /// are the preceding item's nested sublist.
    fn build_list_level(&mut self, items: &[(String, String)], depth: usize) -> Block {
        let ordered = items
            .first()
            .and_then(|(m, _)| m.chars().nth(depth))
            .map(|c| c == '#')
            .unwrap_or(false);
        let mut list = List {
            ordered,
            items: Vec::new(),
        };
        let mut idx = 0;
        while idx < items.len() {
            let (_, content) = &items[idx];
            let mut blocks = vec![Block::Paragraph(self.parse_inlines(content))];
            // Gather deeper-markered children immediately following this item.
            let mut j = idx + 1;
            while j < items.len() && items[j].0.chars().count() > depth + 1 {
                j += 1;
            }
            if j > idx + 1 {
                blocks.push(self.build_list_level(&items[idx + 1..j], depth + 1));
            }
            list.items.push(blocks);
            idx = j;
        }
        Block::List(list)
    }

    fn build_def_list(&mut self, raw: &[(String, String)]) -> Block {
        let mut entries = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let (marker, content) = &raw[i];
            if marker.starts_with(';') {
                // MediaWiki allows the inline form `; term : definition` on one
                // line as well as the multi-line `; term` / `: definition` form.
                // Lift verbatim inline spans (<tt>/<code>/<ref>…) into
                // placeholders first, so a `:` inside one of them is never taken
                // for the term/definition separator.
                let (ph, tokens) = self.extract_tokens(content);
                let (term_ph, inline_desc) = match split_top_level_colon(&ph) {
                    Some((t, d)) => (t.to_string(), Some(d.to_string())),
                    None => (ph, None),
                };
                let term = markup(&term_ph, &tokens);
                let mut description = Vec::new();
                if let Some(d) = inline_desc {
                    if !d.trim().is_empty() {
                        description.push(Block::Paragraph(markup(&d, &tokens)));
                    }
                }
                let mut j = i + 1;
                while j < raw.len() && raw[j].0.starts_with(':') {
                    description.push(Block::Paragraph(self.parse_inlines(&raw[j].1)));
                    j += 1;
                }
                entries.push(DefEntry {
                    anchor: None,
                    term,
                    description,
                });
                i = j;
            } else {
                // A stray `:` line with no `;` term: a lone description.
                entries.push(DefEntry {
                    anchor: None,
                    term: Vec::new(),
                    description: vec![Block::Paragraph(self.parse_inlines(content))],
                });
                i += 1;
            }
        }
        Block::DefinitionList(entries)
    }

    fn consume_table(&mut self, lines: &[&str], start: usize) -> (Block, usize) {
        let mut rows: Vec<Vec<(bool, Vec<Inline>)>> = Vec::new();
        let mut current: Vec<(bool, Vec<Inline>)> = Vec::new();
        let mut i = start + 1; // skip the `{|` (its attributes are ignored)
        while i < lines.len() {
            let lead = lines[i].trim();
            if lead.starts_with("|}") {
                i += 1;
                break;
            }
            if lead.starts_with("|+") {
                i += 1; // caption — ignored
                continue;
            }
            if lead.starts_with("|-") {
                if !current.is_empty() {
                    rows.push(std::mem::take(&mut current));
                }
                i += 1;
                continue;
            }
            if let Some(rest) = lead.strip_prefix('!') {
                for cell in split_header_cells(rest) {
                    let inl = self.parse_inlines(&strip_cell_attrs(cell));
                    current.push((true, inl));
                }
                i += 1;
                continue;
            }
            if let Some(rest) = lead.strip_prefix('|') {
                for cell in rest.split("||") {
                    let inl = self.parse_inlines(&strip_cell_attrs(cell));
                    current.push((false, inl));
                }
                i += 1;
                continue;
            }
            // A continuation line extends the last cell's content.
            if let Some(last) = current.last_mut() {
                last.1.push(Inline::text(" "));
                let more = self.parse_inlines(lead);
                last.1.extend(more);
            }
            i += 1;
        }
        if !current.is_empty() {
            rows.push(current);
        }

        let mut table = Table::default();
        for row in rows {
            let is_header = !row.is_empty() && row.iter().all(|(h, _)| *h);
            let cells: Vec<Vec<Inline>> = row.into_iter().map(|(_, c)| c).collect();
            if is_header && table.head.is_empty() && table.rows.is_empty() {
                table.head = cells;
            } else {
                table.rows.push(cells);
            }
        }
        (Block::Table(table), i)
    }

    // ---- Inline parsing ---------------------------------------------------

    /// Parse an inline run. `<ref>`/`<code>`/`<nowiki>` are lifted out first into
    /// opaque placeholders ([`extract_tokens`](Self::extract_tokens)) so that the
    /// markup pass ([`markup`]) can pair `'''bold'''`/`''italic''` *across* a
    /// code span or footnote without a stray `''` ever swallowing a `<ref>`.
    fn parse_inlines(&mut self, s: &str) -> Vec<Inline> {
        let (text, tokens) = self.extract_tokens(s);
        markup(&text, &tokens)
    }

    /// Replace each `<ref…>…</ref>` / `<code>…</code>` / `<nowiki>…</nowiki>`
    /// with a private-use placeholder (`\u{E000}<index>\u{E000}`), returning the
    /// placeholdered text and the resolved [`Inline`]s those placeholders stand
    /// for (a footnote reference, inline code, or literal text).
    fn extract_tokens(&mut self, s: &str) -> (String, Vec<Inline>) {
        let res = inline_res();
        let mut out = String::new();
        let mut tokens: Vec<Inline> = Vec::new();
        let mut rest = s;
        while !rest.is_empty() {
            let mut best = usize::MAX;
            let mut kind = Kind::None;
            macro_rules! consider {
                ($re:expr, $k:expr) => {
                    if let Some(m) = $re.find(rest) {
                        if m.start() < best {
                            best = m.start();
                            kind = $k;
                        }
                    }
                };
            }
            consider!(res.ref_re, Kind::Ref);
            consider!(res.code, Kind::Code);
            consider!(res.nowiki, Kind::Nowiki);

            if kind == Kind::None {
                out.push_str(rest);
                break;
            }
            out.push_str(&rest[..best]);

            // Pull the token's raw pieces (owned) so the capture borrow ends
            // before any `&mut self` work below.
            let (raw, end): (Raw, usize) = match kind {
                Kind::Ref => {
                    let caps = res.ref_re.captures(rest).unwrap();
                    let end = caps.get(0).unwrap().end();
                    let name = caps
                        .get(1)
                        .and_then(|a| res.ref_name.captures(a.as_str()))
                        .map(|c| slug_ref(&c[1]));
                    let content = caps.get(2).map(|c| c.as_str().to_string());
                    (Raw::Ref { name, content }, end)
                }
                Kind::Code => {
                    let caps = res.code.captures(rest).unwrap();
                    (
                        Raw::Code(decode_entities(&caps[1])),
                        caps.get(0).unwrap().end(),
                    )
                }
                Kind::Nowiki => {
                    let caps = res.nowiki.captures(rest).unwrap();
                    (
                        Raw::Nowiki(decode_entities(&caps[1])),
                        caps.get(0).unwrap().end(),
                    )
                }
                _ => unreachable!(),
            };

            let inline = match raw {
                Raw::Ref { name, content } => {
                    let content = content.map(|c| vec![Block::Paragraph(self.parse_inlines(&c))]);
                    let (key, number) = self.register_ref(name, content);
                    Inline::FootnoteRef { name: key, number }
                }
                Raw::Code(c) => Inline::Code(c),
                Raw::Nowiki(c) => Inline::Text(c),
            };
            let idx = tokens.len();
            tokens.push(inline);
            out.push('\u{E000}');
            out.push_str(&idx.to_string());
            out.push('\u{E000}');

            rest = &rest[end..];
        }
        (out, tokens)
    }

    /// Register a footnote and return its `(anchor-name, display number)`. A
    /// named ref reuses an earlier definition; an anonymous one gets a fresh id.
    fn register_ref(
        &mut self,
        name: Option<String>,
        content: Option<Vec<Block>>,
    ) -> (String, usize) {
        if let Some(n) = &name {
            if let Some(&idx) = self.ref_index.get(n) {
                // Reuse: fill in the body if this occurrence is the one that
                // carries it (define-after-reuse).
                if let Some(c) = content {
                    if self.footnotes[idx].1.is_empty() {
                        self.footnotes[idx].1 = c;
                    }
                }
                return (n.clone(), idx + 1);
            }
        }
        let key = name.unwrap_or_else(|| {
            self.auto_ref += 1;
            format!("auto{}", self.auto_ref)
        });
        let idx = self.footnotes.len();
        self.ref_index.insert(key.clone(), idx);
        self.footnotes
            .push((key.clone(), content.unwrap_or_default()));
        (key, idx + 1)
    }
}

/// Raw pieces of an extracted `<ref>`/`<code>`/`<nowiki>` token, owned so the
/// regex-capture borrow can be released before footnote registration.
enum Raw {
    Ref {
        name: Option<String>,
        content: Option<String>,
    },
    Code(String),
    Nowiki(String),
}

/// Parse the markup layer of a placeholdered text run (see
/// [`MwParser::extract_tokens`]): links, `'''bold'''`/`''italic''`, `<br>`, and
/// inline-tag stripping, expanding placeholders back to their [`Inline`]s.
fn markup(s: &str, tokens: &[Inline]) -> Vec<Inline> {
    let mut out = Vec::new();
    markup_into(s, tokens, &mut out);
    out
}

fn markup_into(s: &str, tokens: &[Inline], out: &mut Vec<Inline>) {
    let res = inline_res();
    let mut rest = s;
    while !rest.is_empty() {
        let mut best = usize::MAX;
        let mut kind = Kind::None;
        macro_rules! consider {
            ($re:expr, $k:expr) => {
                if let Some(m) = $re.find(rest) {
                    if m.start() < best {
                        best = m.start();
                        kind = $k;
                    }
                }
            };
        }
        consider!(res.br, Kind::Br);
        consider!(res.wiki, Kind::Wiki);
        consider!(res.ext, Kind::Ext);
        consider!(res.bold, Kind::Bold);
        consider!(res.italic, Kind::Italic);
        consider!(res.tag, Kind::Tag);

        if kind == Kind::None {
            expand_text(rest, tokens, out);
            break;
        }
        if best > 0 {
            expand_text(&rest[..best], tokens, out);
        }
        let end = match kind {
            Kind::Br => {
                out.push(Inline::LineBreak);
                res.br.find(rest).unwrap().end()
            }
            Kind::Wiki => {
                let caps = res.wiki.captures(rest).unwrap();
                let end = caps.get(0).unwrap().end();
                wiki_link_into(&caps[1], tokens, out);
                end
            }
            Kind::Ext => {
                let caps = res.ext.captures(rest).unwrap();
                let end = caps.get(0).unwrap().end();
                let url = caps[1].to_string();
                let label = caps.get(2).map(|m| m.as_str().trim()).unwrap_or("");
                let text = if label.is_empty() {
                    vec![Inline::text(url.clone())]
                } else {
                    markup(label, tokens)
                };
                out.push(Inline::Link { text, href: url });
                end
            }
            Kind::Bold => {
                let caps = res.bold.captures(rest).unwrap();
                let end = caps.get(0).unwrap().end();
                out.push(Inline::Strong(markup(&caps[1], tokens)));
                end
            }
            Kind::Italic => {
                let caps = res.italic.captures(rest).unwrap();
                let end = caps.get(0).unwrap().end();
                out.push(Inline::Emph(markup(&caps[1], tokens)));
                end
            }
            Kind::Tag => {
                let m = res.tag.find(rest).unwrap();
                // A real HTML tag (<sub>, <span>…) is stripped, keeping its
                // content; an unknown angle-bracket token (<pubkey>, <txid>, <C>)
                // is kept *literally*, matching how MediaWiki escapes-and-shows
                // it — BIP script/witness templates rely on this.
                if !is_html_tag(m.as_str()) {
                    push_text(out, decode_entities(m.as_str()));
                }
                m.end()
            }
            _ => rest.len(),
        };
        rest = &rest[end..];
    }
}

/// Emit a text run, decoding entities and expanding `\u{E000}<index>\u{E000}`
/// placeholders back into the [`Inline`]s they stand for.
fn expand_text(run: &str, tokens: &[Inline], out: &mut Vec<Inline>) {
    let re = placeholder_re();
    let mut last = 0;
    for caps in re.captures_iter(run) {
        let m = caps.get(0).unwrap();
        if m.start() > last {
            push_text(out, decode_entities(&run[last..m.start()]));
        }
        if let Some(inl) = caps[1].parse::<usize>().ok().and_then(|i| tokens.get(i)) {
            out.push(inl.clone());
        }
        last = m.end();
    }
    if last < run.len() {
        push_text(out, decode_entities(&run[last..]));
    }
}

/// Resolve a `[[…]]` wiki link into `out`: `File:`/`Image:` → an image,
/// `#anchor` → a cross-reference, a cross-document BIP link → its canonical web
/// URL, and an unknown internal page → its label text.
fn wiki_link_into(inner: &str, tokens: &[Inline], out: &mut Vec<Inline>) {
    let (target, label) = match inner.split_once('|') {
        Some((t, l)) => (t.trim(), l.trim()),
        None => (inner.trim(), inner.trim()),
    };
    if let Some(name) = target
        .strip_prefix("File:")
        .or_else(|| target.strip_prefix("Image:"))
    {
        out.push(Inline::Image {
            resource: name.trim().to_string(),
            alt: plain_text(&markup(label, tokens)),
        });
        return;
    }
    let text = markup(label, tokens);
    if let Some(frag) = target.strip_prefix('#') {
        out.push(Inline::XRef {
            text,
            target: anchorize_once(frag),
        });
        return;
    }
    if let Some(href) = rewrite_doc_link(target) {
        out.push(Inline::Link { text, href });
        return;
    }
    out.extend(text);
}

fn placeholder_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x{E000}(\d+)\x{E000}").expect("re"))
}

/// Flatten inlines to plain text (for image alt text).
fn plain_text(inlines: &[Inline]) -> String {
    let mut s = String::new();
    for i in inlines {
        match i {
            Inline::Text(t) | Inline::Code(t) => s.push_str(t),
            Inline::Emph(x) | Inline::Strong(x) | Inline::Strikethrough(x) => {
                s.push_str(&plain_text(x))
            }
            Inline::Link { text, .. } | Inline::XRef { text, .. } => s.push_str(&plain_text(text)),
            _ => {}
        }
    }
    s
}

/// Which inline token was selected as leftmost.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    None,
    Ref,
    Code,
    Nowiki,
    Br,
    Wiki,
    Ext,
    Bold,
    Italic,
    Tag,
}

/// Lazily-compiled inline regexes.
struct InlineRes {
    ref_re: Regex,
    ref_name: Regex,
    code: Regex,
    nowiki: Regex,
    br: Regex,
    wiki: Regex,
    ext: Regex,
    bold: Regex,
    italic: Regex,
    tag: Regex,
}

fn inline_res() -> &'static InlineRes {
    static R: OnceLock<InlineRes> = OnceLock::new();
    R.get_or_init(|| InlineRes {
        // `<ref …/>` (self-closing) or `<ref …>body</ref>`.
        ref_re: Regex::new(r"(?is)<ref(\s[^>/]*)?(?:/\s*>|>(.*?)</ref\s*>)").expect("re"),
        ref_name: Regex::new(r#"(?i)name\s*=\s*"?([^"\s/>]+)"?"#).expect("re"),
        code: Regex::new(r"(?is)<(?:code|tt)\b[^>]*>(.*?)</(?:code|tt)\s*>").expect("re"),
        nowiki: Regex::new(r"(?is)<nowiki>(.*?)</nowiki>").expect("re"),
        br: Regex::new(r"(?i)<br\s*/?\s*>").expect("re"),
        wiki: Regex::new(r"(?s)\[\[([^\[\]]*)\]\]").expect("re"),
        ext: Regex::new(r"(?i)\[((?:https?|ftp|mailto|bitcoin):[^\s\]]+)([ \t][^\]]*)?\]")
            .expect("re"),
        bold: Regex::new(r"'''(.+?)'''").expect("re"),
        italic: Regex::new(r"''(.+?)''").expect("re"),
        // Only a real tag (`<name…>` or `</name>`); a literal `<` in prose
        // (`a < b`) must stay text, not be eaten as a tag.
        tag: Regex::new(r"(?s)</?[a-zA-Z][^>]*>").expect("re"),
    })
}

// ---------------------------------------------------------------------------
// Block-level helpers
// ---------------------------------------------------------------------------

/// Strip `<!-- … -->` HTML comments.
fn strip_comments(body: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?s)<!--.*?-->").expect("re"));
    re.replace_all(body, "").into_owned()
}

/// Extract the first `<pre>…</pre>` if it parses as a spec preamble, returning
/// the parsed preamble and the body with that block removed. If the first
/// `<pre>` is not a preamble (unusual), it is left in place as code.
fn extract_preamble(body: &str) -> (Option<super::preamble::Preamble>, String) {
    let lower = body.to_ascii_lowercase();
    let Some(open) = lower.find("<pre") else {
        return (None, body.to_string());
    };
    let Some(gt) = body[open..].find('>').map(|i| i + open) else {
        return (None, body.to_string());
    };
    let Some(close) = lower[gt..].find("</pre>").map(|i| i + gt) else {
        return (None, body.to_string());
    };
    let inner = &body[gt + 1..close];
    if !looks_like_preamble(inner) {
        return (None, body.to_string());
    }
    let pre = super::preamble::Preamble::parse(inner);
    let mut without = String::with_capacity(body.len());
    without.push_str(&body[..open]);
    without.push_str(&body[close + "</pre>".len()..]);
    (Some(pre), without)
}

/// Whether a `<pre>` block's inner text reads as a BIP/spec preamble.
fn looks_like_preamble(inner: &str) -> bool {
    super::preamble::Preamble::parse(inner).has_any(&["bip", "eip", "caip", "title", "layer"])
}

/// Whether an angle-bracket token is a real (MediaWiki-recognized) HTML tag
/// whose wrapper should be stripped, versus a bare `<token>` that BIP prose uses
/// as a placeholder (`<pubkey>`, `<txid>`, `<C>`) and MediaWiki shows literally.
/// The tag name is the leading run of ASCII alphanumerics after `<`/`</`.
fn is_html_tag(tag: &str) -> bool {
    let name = tag
        .trim_start_matches('<')
        .trim_start_matches('/')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    const KNOWN: &[&str] = &[
        "b",
        "i",
        "u",
        "s",
        "em",
        "strong",
        "small",
        "big",
        "strike",
        "sub",
        "sup",
        "span",
        "div",
        "p",
        "abbr",
        "cite",
        "dfn",
        "kbd",
        "var",
        "samp",
        "mark",
        "ins",
        "del",
        "q",
        "center",
        "font",
        "bdi",
        "bdo",
        "time",
        "data",
        "ruby",
        "rt",
        "rp",
        "blockquote",
        "hr",
        "wbr",
        "br",
        "code",
        "tt",
        "nowiki",
        "ref",
        "references",
        "pre",
        "source",
        "syntaxhighlight",
    ];
    KNOWN.contains(&name.as_str())
}

/// Split a (placeholdered) single-line definition item `term : definition` on
/// the first top-level `:` — one not inside a `[…]` link and not part of a `://`
/// URL scheme. Verbatim inline spans have already been replaced by placeholders
/// (which carry no `:`), so only link brackets and bare tags need guarding.
/// Returns `None` when there is no such separator (the multi-line form).
fn split_top_level_colon(content: &str) -> Option<(&str, &str)> {
    let mut depth: i32 = 0;
    for (idx, ch) in content.char_indices() {
        match ch {
            '<' | '[' => depth += 1,
            '>' | ']' => depth = (depth - 1).max(0),
            ':' if depth == 0 => {
                if content[idx..].starts_with("://") {
                    continue;
                }
                return Some((content[..idx].trim(), content[idx + 1..].trim()));
            }
            _ => {}
        }
    }
    None
}

/// A `== Heading ==` line → `(level, title)`. Mismatched fence lengths take the
/// smaller as the level (`== X ===` is level 2).
fn parse_heading(line: &str) -> Option<(u8, String)> {
    let t = line.trim();
    if !t.starts_with('=') || !t.ends_with('=') {
        return None;
    }
    let open = t.chars().take_while(|&c| c == '=').count();
    let close = t.chars().rev().take_while(|&c| c == '=').count();
    let level = open.min(close);
    if level == 0 || level > 6 {
        return None;
    }
    let title = t.trim_matches('=').trim();
    if title.is_empty() {
        return None;
    }
    Some((level as u8, title.to_string()))
}

/// A horizontal rule: four or more dashes on their own line.
fn is_hr(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 4 && t.chars().all(|c| c == '-')
}

/// MediaWiki magic words we simply drop.
fn is_magic_word(line: &str) -> bool {
    matches!(
        line.trim(),
        "__TOC__" | "__NOTOC__" | "__FORCETOC__" | "__NOEDITSECTION__"
    )
}

/// Whether a trimmed line starts a bullet/numbered/definition list.
fn starts_list(lead: &str) -> bool {
    lead.starts_with(['*', '#', ';', ':'])
}

/// Whether a trimmed line begins any block-level construct (used to end a
/// paragraph that runs straight into a list/table/heading without a blank line).
fn is_block_start(lead: &str) -> bool {
    is_hr(lead)
        || parse_heading(lead).is_some()
        || opens_tag(lead, "pre")
        || opens_tag(lead, "source")
        || opens_tag(lead, "syntaxhighlight")
        || lead.starts_with("{|")
        || lead.starts_with("<references")
        || starts_list(lead)
        || figure_line(lead).is_some()
}

/// Whether a line opens `<tag …>` (case-insensitive).
fn opens_tag(lead: &str, tag: &str) -> bool {
    let l = lead.to_ascii_lowercase();
    l.starts_with(&format!("<{tag}>"))
        || l.starts_with(&format!("<{tag} "))
        || l.starts_with(&format!("<{tag}\t"))
}

/// Consume a `<tag …>…</tag>` block spanning `lines[start..]`, returning the
/// inner text, the opening tag's attribute string, and the index past `</tag>`.
fn consume_tag_block(lines: &[&str], start: usize, tag: &str) -> (String, String, usize) {
    let close = format!("</{tag}>");
    let mut j = start;
    let mut buf = String::new();
    loop {
        buf.push_str(lines[j]);
        buf.push('\n');
        if lines[j].to_ascii_lowercase().contains(&close) {
            j += 1;
            break;
        }
        j += 1;
        if j >= lines.len() {
            break;
        }
    }
    let lower = buf.to_ascii_lowercase();
    let open = lower.find(&format!("<{tag}")).unwrap_or(0);
    let gt = buf[open..].find('>').map(|i| i + open).unwrap_or(buf.len());
    let attrs = buf[(open + tag.len() + 1).min(gt)..gt].trim().to_string();
    let close_at = lower.rfind(&close).unwrap_or(buf.len());
    let inner = buf.get(gt + 1..close_at).unwrap_or("");
    let inner = inner.strip_prefix('\n').unwrap_or(inner).trim_end();
    (inner.to_string(), attrs, j)
}

/// Build a highlighted (or plain) code block from a `<source>`/`<syntaxhighlight>`
/// body and its attributes (`lang="…"`).
fn highlighted_code(code: &str, attrs: &str) -> Block {
    let lang = lang_attr(attrs);
    if let Some(l) = &lang {
        if let Some(html) = crate::highlight::highlight(code, l) {
            return Block::HighlightedCode {
                language: l.clone(),
                html,
            };
        }
    }
    Block::Code {
        text: code.to_string(),
        language: lang,
    }
}

/// The `lang="…"` attribute value, if present.
fn lang_attr(attrs: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"(?i)\blang\s*=\s*"?([\w+.#-]+)"?"#).expect("re"));
    re.captures(attrs).map(|c| c[1].to_string())
}

/// If a whole line is a single image construct (`[[File:…]]` or `<img …>`),
/// return it as a figure.
fn figure_line(lead: &str) -> Option<Block> {
    let t = lead.trim();
    if (t.starts_with("[[File:") || t.starts_with("[[Image:")) && t.ends_with("]]") {
        let inner = &t[2..t.len() - 2];
        let mut parts = inner.splitn(2, ':');
        let _ns = parts.next();
        let after = parts.next().unwrap_or("");
        let fields: Vec<&str> = after.split('|').collect();
        let resource = fields.first().copied().unwrap_or("").trim().to_string();
        // The caption is the last field that isn't a formatting option.
        let caption = fields
            .iter()
            .skip(1)
            .map(|f| f.trim())
            .rfind(|f| !f.is_empty() && !is_image_option(f))
            .map(|c| vec![Inline::text(decode_entities(c))]);
        return Some(Block::Figure {
            resource,
            alt: String::new(),
            caption,
        });
    }
    if t.starts_with("<img") && t.ends_with('>') {
        let src = attr_value(t, "src")?;
        let alt = attr_value(t, "alt").unwrap_or_default();
        return Some(Block::Figure {
            resource: src,
            alt,
            caption: None,
        });
    }
    None
}

/// A `[[File:…]]` display option (not a caption): `thumb`, `300px`, `right`, …
fn is_image_option(field: &str) -> bool {
    let f = field.to_ascii_lowercase();
    matches!(
        f.as_str(),
        "thumb"
            | "thumbnail"
            | "frame"
            | "frameless"
            | "border"
            | "right"
            | "left"
            | "center"
            | "centre"
            | "none"
            | "baseline"
            | "middle"
            | "top"
            | "bottom"
    ) || f.ends_with("px")
        || f.starts_with("alt=")
        || f.starts_with("link=")
        || f.starts_with("upright")
}

/// Read an HTML attribute value from a single tag string.
fn attr_value(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let key = format!("{name}=");
    let at = lower.find(&key)? + key.len();
    let rest = &tag[at..];
    let rest = rest.trim_start();
    let bytes = rest.as_bytes();
    if bytes.first() == Some(&b'"') || bytes.first() == Some(&b'\'') {
        let q = rest.chars().next().unwrap();
        let inner = &rest[1..];
        let end = inner.find(q)?;
        Some(inner[..end].to_string())
    } else {
        Some(
            rest.split([' ', '\t', '>', '/'])
                .next()
                .unwrap_or("")
                .to_string(),
        )
    }
}

/// Split a header row's cells on `!!` (and `||`, which wiki also allows).
fn split_header_cells(s: &str) -> Vec<&str> {
    s.split("!!").flat_map(|p| p.split("||")).collect()
}

/// Drop a leading `attr=… |` prefix from a table cell, keeping the content.
fn strip_cell_attrs(cell: &str) -> String {
    let c = cell.trim();
    if let Some((left, right)) = c.split_once('|') {
        if left.contains('=') && !left.contains("://") && !left.contains("[[") {
            return right.trim().to_string();
        }
    }
    c.to_string()
}

// ---------------------------------------------------------------------------
// Section building
// ---------------------------------------------------------------------------

/// Fold heading-delimited items into a nested section tree, keyed off the `=`
/// heading depth. Content before the first heading becomes an "Overview".
fn build_sections(items: Vec<Item>, _title: &str) -> Vec<Section> {
    let levels: Vec<u8> = items
        .iter()
        .filter_map(|i| match i {
            Item::Heading { level, .. } => Some(*level),
            _ => None,
        })
        .collect();
    let Some(&top) = levels.iter().min() else {
        // No headings: one section holding everything (if anything).
        let blocks: Vec<Block> = items
            .into_iter()
            .filter_map(|i| match i {
                Item::Block(b) => Some(b),
                _ => None,
            })
            .collect();
        if blocks.is_empty() {
            return Vec::new();
        }
        return vec![Section {
            number: None,
            title: "Document".into(),
            id: "body".into(),
            blocks,
            subsections: Vec::new(),
        }];
    };

    let mut anchorizer = Anchorizer::new();
    let mut leading: Vec<Block> = Vec::new();
    let mut flat: Vec<(usize, Section)> = Vec::new();
    for item in items {
        match item {
            Item::Heading { level, title } => {
                let id = anchorizer.anchorize(&title);
                let depth = (level as usize).saturating_sub(top as usize) + 1;
                flat.push((
                    depth,
                    Section {
                        number: None,
                        title,
                        id,
                        blocks: Vec::new(),
                        subsections: Vec::new(),
                    },
                ));
            }
            Item::Block(b) => match flat.last_mut() {
                Some((_, sec)) => sec.blocks.push(b),
                None => leading.push(b),
            },
        }
    }

    let mut roots = nest_sections(flat);
    if !leading.is_empty() {
        roots.insert(
            0,
            Section {
                number: None,
                title: "Overview".into(),
                id: "overview".into(),
                blocks: leading,
                subsections: Vec::new(),
            },
        );
    }
    roots
}

fn nest_sections(flat: Vec<(usize, Section)>) -> Vec<Section> {
    let mut roots: Vec<Section> = Vec::new();
    for (depth, section) in flat {
        insert_at_depth(&mut roots, section, depth);
    }
    roots
}

fn insert_at_depth(siblings: &mut Vec<Section>, section: Section, depth: usize) {
    if depth <= 1 {
        siblings.push(section);
        return;
    }
    if let Some(parent) = siblings.last_mut() {
        insert_at_depth(&mut parent.subsections, section, depth - 1);
    } else {
        siblings.push(section);
    }
}

// ---------------------------------------------------------------------------
// Small text helpers
// ---------------------------------------------------------------------------

fn push_text(out: &mut Vec<Inline>, s: String) {
    if !s.is_empty() {
        out.push(Inline::Text(s));
    }
}

/// A one-shot GitHub-compatible slug (matches the ids assigned to sections), for
/// resolving `[[#anchor]]` wiki links.
fn anchorize_once(s: &str) -> String {
    Anchorizer::new().anchorize(s)
}

/// A stable, id-safe slug for a footnote's `name` (or anchor derived from it).
fn slug_ref(name: &str) -> String {
    let mut out = String::new();
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let s = out.trim_matches('-').to_string();
    if s.is_empty() {
        "ref".to_string()
    } else {
        s
    }
}

/// Strip wiki markup to plain text (for heading titles / slugs).
fn strip_markup(s: &str) -> String {
    let res = inline_res();
    let mut t = res
        .wiki
        .replace_all(s, |c: &regex::Captures| {
            let inner = &c[1];
            let label = inner.rsplit('|').next().unwrap_or(inner);
            label.trim().to_string()
        })
        .into_owned();
    t = res
        .ext
        .replace_all(&t, |c: &regex::Captures| {
            c.get(2)
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_else(|| c[1].to_string())
        })
        .into_owned();
    t = res
        .tag
        .replace_all(&t, |c: &regex::Captures| {
            if is_html_tag(&c[0]) {
                String::new()
            } else {
                c[0].to_string()
            }
        })
        .into_owned();
    t = t.replace("'''", "").replace("''", "");
    decode_entities(t.trim())
}

/// Decode the handful of HTML entities BIPs use in text.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&nbsp;", " ")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
        .replace("&rarr;", "→")
        .replace("&larr;", "←")
        .replace("&times;", "×")
        .replace("&le;", "≤")
        .replace("&ge;", "≥")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_bip(body: &str) -> Document {
        parse(body, Some(Collection::Bip), Some(341)).unwrap()
    }

    #[test]
    fn extracts_preamble_and_builds_sections() {
        let body = "\
<pre>
  BIP: 341
  Title: Taproot
  Author: Pieter Wuille <pw@x>
          Jonas Nick <jn@x>
  Status: Final
  Requires: 340, 342
</pre>

==Abstract==

This BIP describes '''Taproot''', a new output type.

==Motivation==

Some prose here.

===Details===

More detail.
";
        let doc = parse_bip(body);
        assert_eq!(doc.title, "Taproot");
        assert_eq!(doc.status.as_deref(), Some("Final"));
        assert_eq!(doc.authors.len(), 2);
        let req = doc
            .relations
            .iter()
            .find(|r| r.label == "Requires")
            .unwrap();
        assert_eq!(
            req.targets.iter().map(|t| t.number).collect::<Vec<_>>(),
            vec![340, 342]
        );
        // The preamble `<pre>` is not emitted as a code block.
        assert!(!doc.sections.iter().any(|s| s.title == "Document"));
        let titles: Vec<&str> = doc.sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["Abstract", "Motivation"]);
        // Nested `===Details===` is a child of Motivation.
        let motivation = &doc.sections[1];
        assert_eq!(motivation.subsections.len(), 1);
        assert_eq!(motivation.subsections[0].title, "Details");
    }

    #[test]
    fn inline_bold_italic_code_and_links() {
        let mut p = MwParser::default();
        let inls = p.parse_inlines("A '''bold''' and ''italic'' and <code>x = 1</code> and [https://example.com label] end.");
        let dbg = format!("{inls:?}");
        assert!(dbg.contains("Strong"));
        assert!(dbg.contains("Emph"));
        assert!(
            matches!(inls.iter().find(|i| matches!(i, Inline::Code(_))), Some(Inline::Code(c)) if c == "x = 1")
        );
        assert!(inls
            .iter()
            .any(|i| matches!(i, Inline::Link { href, .. } if href == "https://example.com")));
    }

    #[test]
    fn cross_document_wiki_link_rewrites_to_canonical_url() {
        let mut p = MwParser::default();
        let inls = p.parse_inlines("see [[bip-0032.mediawiki|BIP-32]] for details");
        assert!(inls.iter().any(|i| matches!(i, Inline::Link { href, .. }
            if href == "https://github.com/bitcoin/bips/blob/master/bip-0032.mediawiki")));
    }

    #[test]
    fn ref_footnotes_collected_and_numbered() {
        let body = "\
<pre>
  BIP: 341
  Title: T
</pre>

==Body==

First claim.<ref>First note.</ref> Second.<ref name=\"a\">Second note.</ref>
Reuse.<ref name=\"a\"/>
";
        let doc = parse_bip(body);
        // Two distinct footnotes; the reuse points at the second, not a third.
        let footnotes = doc
            .sections
            .iter()
            .find(|s| s.title == "Footnotes")
            .expect("footnotes section");
        let Block::DefinitionList(entries) = &footnotes.blocks[0] else {
            panic!("expected a definition list");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].anchor.as_deref(), Some("fn-auto1"));
        assert_eq!(entries[1].anchor.as_deref(), Some("fn-a"));
        // The body carries three refs, the third reusing number 2.
        let body_sec = doc.sections.iter().find(|s| s.title == "Body").unwrap();
        let refs: Vec<usize> = collect_footnote_numbers(&body_sec.blocks);
        assert_eq!(refs, vec![1, 2, 2]);
    }

    fn collect_footnote_numbers(blocks: &[Block]) -> Vec<usize> {
        let mut out = Vec::new();
        for b in blocks {
            if let Block::Paragraph(inls) = b {
                for i in inls {
                    if let Inline::FootnoteRef { number, .. } = i {
                        out.push(*number);
                    }
                }
            }
        }
        out
    }

    #[test]
    fn wiki_table_becomes_a_table() {
        let body = "\
<pre>
  BIP: 1
  Title: T
</pre>

==Table==

{| class=\"wikitable\"
! Field !! Size
|-
| version || 4
|-
| style=\"text-align:right\" | flags || 1
|}
";
        let doc = parse_bip(body);
        let sec = doc.sections.iter().find(|s| s.title == "Table").unwrap();
        let Block::Table(t) = sec
            .blocks
            .iter()
            .find(|b| matches!(b, Block::Table(_)))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(t.head.len(), 2);
        assert_eq!(t.rows.len(), 2);
        // The cell attribute prefix was stripped, leaving just "flags".
        assert!(matches!(t.rows[1][0].as_slice(), [Inline::Text(s)] if s == "flags"));
    }

    #[test]
    fn source_block_is_highlighted() {
        let body = "\
<pre>
  BIP: 1
  Title: T
</pre>

==Code==

<source lang=\"python\">
def f():
    return 1
</source>
";
        let doc = parse_bip(body);
        let sec = doc.sections.iter().find(|s| s.title == "Code").unwrap();
        assert!(sec
            .blocks
            .iter()
            .any(|b| matches!(b, Block::HighlightedCode { language, .. } if language == "python")));
    }

    #[test]
    fn pre_and_lists_and_hr() {
        let body = "\
<pre>
  BIP: 1
  Title: T
</pre>

==S==

<pre>
+---+
|art|
+---+
</pre>

* one
* two
*# nested-num

----
";
        let doc = parse_bip(body);
        let sec = doc.sections.iter().find(|s| s.title == "S").unwrap();
        assert!(sec
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Code { text, .. } if text.contains("art"))));
        let list = sec
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::List(l) => Some(l),
                _ => None,
            })
            .expect("a list");
        assert_eq!(list.items.len(), 2);
        // The second item carries a nested ordered sublist.
        assert!(list.items[1]
            .iter()
            .any(|b| matches!(b, Block::List(inner) if inner.ordered)));
        assert!(sec.blocks.iter().any(|b| matches!(b, Block::ThematicBreak)));
    }

    #[test]
    fn file_and_img_become_figures() {
        assert!(matches!(
            figure_line("[[File:bip-0341/diagram.png|thumb|A diagram]]"),
            Some(Block::Figure { resource, caption: Some(_), .. }) if resource == "bip-0341/diagram.png"
        ));
        assert!(matches!(
            figure_line("<img src=\"bip-0174/flow.png\" alt=\"flow\">"),
            Some(Block::Figure { resource, alt, .. }) if resource == "bip-0174/flow.png" && alt == "flow"
        ));
        assert!(figure_line("not an image").is_none());
    }

    #[test]
    fn single_line_definition_list_splits_term_and_definition() {
        let mut p = MwParser::default();
        // The inline `; term : definition` form (used pervasively by BIP-174),
        // including a `:` *inside* a <tt> span (must not be the separator).
        let lines = ["; Term one : Definition one", "; <tt>a:b</tt> : Second def"];
        let (block, _) = p.consume_list(&lines, 0);
        let Block::DefinitionList(entries) = block else {
            panic!("expected a definition list, got {block:?}");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(plain_text(&entries[0].term), "Term one");
        let Block::Paragraph(desc0) = &entries[0].description[0] else {
            panic!("expected a paragraph definition");
        };
        assert_eq!(plain_text(desc0), "Definition one");
        // The colon inside <tt>a:b</tt> is not the separator; the term is "a:b".
        assert_eq!(plain_text(&entries[1].term), "a:b");
        let Block::Paragraph(desc1) = &entries[1].description[0] else {
            panic!("expected a paragraph definition");
        };
        assert_eq!(plain_text(desc1), "Second def");
        // A `://` URL colon is not the separator either.
        assert_eq!(split_top_level_colon("See https://x/y here"), None);
        assert_eq!(split_top_level_colon("a : b"), Some(("a", "b")));
    }

    #[test]
    fn unknown_angle_bracket_tokens_are_kept_literal() {
        // BIP script/witness templates: `<pubkey>` etc. must survive; a real
        // formatting tag like `<sub>` is stripped but its content kept.
        let mut p = MwParser::default();
        let inls = p.parse_inlines("witness: <signature> <pubkey>, and x<sub>i</sub> end");
        let text = plain_text(&inls);
        assert!(text.contains("<signature>"), "placeholder kept: {text}");
        assert!(text.contains("<pubkey>"), "placeholder kept: {text}");
        assert!(!text.contains("<sub>"), "<sub> wrapper stripped: {text}");
        assert!(text.contains("xi"), "<sub> content kept: {text}");
        assert!(is_html_tag("<sub>") && is_html_tag("</SPAN>"));
        assert!(!is_html_tag("<pubkey>") && !is_html_tag("<C>") && !is_html_tag("<R-HASH>"));
    }
}
