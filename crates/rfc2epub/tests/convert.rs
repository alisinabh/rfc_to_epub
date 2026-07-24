//! End-to-end tests for both parsers and the EPUB renderer, using inline
//! sources (no network).

use rfc2epub::model::{Block, Collection, SourceKind, SvgMode};
use rfc2epub::{parse_source, render};

const XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<rfc version="3" number="9999" category="info" obsoletes="1234">
<front>
<title abbrev="Test">A Test Document</title>
<author fullname="A. Author"><organization>Example Org</organization></author>
<date month="06" year="2022"/>
<abstract><t>This is the abstract.</t></abstract>
</front>
<middle>
<section anchor="intro"><name>Introduction</name>
<t>Hello <xref target="sec-two" derivedContent="Section 2"/> world.</t>
<artwork>+--+
|ab|
+--+</artwork>
<sourcecode type="c">int x = 0;</sourcecode>
<section><name>Nested</name><t>Deeper.</t></section>
</section>
</middle>
</rfc>"#;

#[test]
fn xml_extracts_metadata_and_blocks() {
    let doc = parse_source(XML, SourceKind::Xml, None).unwrap();
    assert_eq!(doc.number(), Some(9999));
    assert_eq!(doc.collection(), Some(Collection::Rfc));
    assert_eq!(doc.title, "A Test Document");
    assert_eq!(doc.authors.len(), 1);
    assert_eq!(doc.authors[0].name, "A. Author");
    assert_eq!(doc.date.as_deref(), Some("June 2022")); // numeric month mapped
    let obsoletes = doc
        .relations
        .iter()
        .find(|r| r.label == "Obsoletes")
        .expect("obsoletes relation");
    assert_eq!(obsoletes.targets.len(), 1);
    assert_eq!(obsoletes.targets[0].number, 1234);
    assert_eq!(obsoletes.targets[0].collection, Collection::Rfc);
    assert_eq!(doc.status.as_deref(), Some("Informational"));
    assert_eq!(doc.abstract_.len(), 1);

    // One top-level section, numbered "1", with a nested "1.1".
    assert_eq!(doc.sections.len(), 1);
    let intro = &doc.sections[0];
    assert_eq!(intro.number.as_deref(), Some("1"));
    assert_eq!(intro.title, "Introduction");
    assert_eq!(intro.subsections.len(), 1);
    assert_eq!(intro.subsections[0].number.as_deref(), Some("1.1"));

    // Artwork and code survive as verbatim blocks.
    assert!(intro
        .blocks
        .iter()
        .any(|b| matches!(b, Block::Artwork(_))));
    assert!(intro
        .blocks
        .iter()
        .any(|b| matches!(b, Block::Code { .. })));
}

const TEXT: &str = "
                            A Text RFC

                            September 2020


                            1.  INTRODUCTION

  This is a paragraph of prose that should
  be joined into a single reflowed line.

  +--------+
  |  art   |
  +--------+

1.1.  Details

  More prose here.
";

#[test]
fn text_reconstructs_sections_and_preserves_art() {
    let doc = parse_source(TEXT, SourceKind::Text, Some(9998)).unwrap();
    assert_eq!(doc.title, "A Text RFC"); // mixed-case title preserved as-is

    // Locate the numbered top-level section (a "Front Matter" section may lead).
    let intro = doc
        .sections
        .iter()
        .find(|s| s.number.as_deref() == Some("1"))
        .expect("top-level section 1");
    assert_eq!(intro.title, "Introduction");
    assert_eq!(intro.subsections.len(), 1, "1.1 should nest under 1");

    // The prose block is reflowed to a single line; the box stays verbatim.
    let has_reflowed_prose = intro.blocks.iter().any(|b| {
        matches!(b, Block::Paragraph(inlines) if para_text(inlines).contains("single reflowed line"))
    });
    assert!(has_reflowed_prose, "prose should be reflowed");
    assert!(
        intro.blocks.iter().any(|b| matches!(b, Block::Artwork(a) if a.contains("art"))),
        "ascii box should be preserved as artwork"
    );
}

#[test]
fn renders_valid_epub_zip() {
    let doc = parse_source(XML, SourceKind::Xml, None).unwrap();
    for mode in [SvgMode::Card, SvgMode::Inline] {
        let bytes = render::to_epub(&doc, mode, true).unwrap();
        // EPUB is a ZIP; ZIP files start with "PK".
        assert_eq!(&bytes[..2], b"PK");
        // The uncompressed `mimetype` string appears near the start.
        let head = String::from_utf8_lossy(&bytes[..200]);
        assert!(head.contains("application/epub+zip"));
    }
}

const EIP_MD: &str = r#"---
eip: 9999
title: A Test EIP
description: Testing the markdown pipeline
author: Alice (@alice), Bob <bob@example.com>
discussions-to: https://example.com/t/1
status: Draft
type: Standards Track
category: Core
created: 2026-07-24
requires: 20, 721
---

## Abstract

This is the **abstract** with a [cross-ref](#specification) and an [EIP-20](./eip-20.md).

## Specification

Here is some code:

```solidity
contract Foo { uint256 public x; }
```

Inline math $E = mc^2$ and display math:

$$
\frac{a}{b}
$$

A table:

| Left | Right |
|:-----|------:|
| 1    | 2     |

A claim needing a note.[^note]

[^note]: The footnote text.

### Sub-detail

Deeper content here.
"#;

fn find_section<'a>(sections: &'a [rfc2epub::model::Section], title: &str) -> Option<&'a rfc2epub::model::Section> {
    sections.iter().find(|s| s.title == title)
}

fn any_block(doc: &rfc2epub::Document, pred: impl Fn(&Block) -> bool + Copy) -> bool {
    fn walk(sections: &[rfc2epub::model::Section], pred: &dyn Fn(&Block) -> bool) -> bool {
        sections.iter().any(|s| {
            s.blocks.iter().any(&pred) || walk(&s.subsections, pred)
        })
    }
    walk(&doc.sections, &pred)
}

#[test]
fn markdown_eip_full_pipeline() {
    use rfc2epub::model::{Collection, Inline};
    let doc = parse_source(EIP_MD, SourceKind::Markdown, None).unwrap();

    // Preamble metadata.
    assert_eq!(doc.collection(), Some(Collection::Eip));
    assert_eq!(doc.number(), Some(9999));
    assert_eq!(doc.title, "A Test EIP");
    assert_eq!(doc.status.as_deref(), Some("Draft"));
    assert_eq!(doc.date.as_deref(), Some("2026-07-24"));

    // Authors: GitHub handle → profile link; email → mailto.
    assert_eq!(doc.authors.len(), 2);
    assert_eq!(doc.authors[0].name, "Alice");
    assert_eq!(doc.authors[0].link.as_deref(), Some("https://github.com/alice"));
    assert_eq!(doc.authors[1].name, "Bob");
    assert_eq!(doc.authors[1].link.as_deref(), Some("mailto:bob@example.com"));

    // Relations.
    let req = doc.relations.iter().find(|r| r.label == "Requires").unwrap();
    assert_eq!(req.targets.iter().map(|t| t.number).collect::<Vec<_>>(), vec![20, 721]);

    // Extra metadata table carries unmapped keys.
    let extra_keys: Vec<&str> = doc.extra.iter().map(|(k, _)| k.as_str()).collect();
    assert!(extra_keys.contains(&"Discussions-To"), "extra = {extra_keys:?}");
    assert!(extra_keys.contains(&"Type"));
    assert!(extra_keys.contains(&"Category"));

    // Abstract from `description`.
    assert_eq!(doc.abstract_.len(), 1);

    // Sections: Abstract, Specification (+ Sub-detail), Footnotes.
    assert!(find_section(&doc.sections, "Abstract").is_some());
    let spec = find_section(&doc.sections, "Specification").expect("spec section");
    assert_eq!(spec.id, "specification"); // GitHub-compatible anchor
    assert!(find_section(&spec.subsections, "Sub-detail").is_some());
    assert!(find_section(&doc.sections, "Footnotes").is_some());

    // Cross-reference resolved to an XRef (anchor matches a section id).
    let abstract_sec = find_section(&doc.sections, "Abstract").unwrap();
    let has_xref = abstract_sec.blocks.iter().any(|b| matches!(b, Block::Paragraph(inls)
        if inls.iter().any(|i| matches!(i, Inline::XRef { target, .. } if target == "specification"))));
    assert!(has_xref, "cross-ref should become an XRef to 'specification'");

    // Cross-document link rewritten to an absolute web URL.
    let has_ext_link = abstract_sec.blocks.iter().any(|b| matches!(b, Block::Paragraph(inls)
        if inls.iter().any(|i| matches!(i, Inline::Link { href, .. } if href == "https://eips.ethereum.org/EIPS/eip-20"))));
    assert!(has_ext_link, "./eip-20.md should rewrite to the canonical URL");

    // Solidity code highlighted; inline + display math present; GFM table.
    assert!(any_block(&doc, |b| matches!(b, Block::HighlightedCode { language, .. } if language == "solidity")));
    assert!(any_block(&doc, |b| matches!(b, Block::Math { .. })), "display math block");
    assert!(any_block(&doc, |b| matches!(b, Block::Table(t) if t.align.len() == 2)));

    // Footnote reference and definition anchored consistently.
    assert!(any_block(&doc, |b| matches!(b, Block::DefinitionList(entries)
        if entries.iter().any(|e| e.anchor.as_deref() == Some("fn-note")))));

    // Renders to a valid EPUB.
    let bytes = render::to_epub(&doc, SvgMode::Inline, false).unwrap();
    assert_eq!(&bytes[..2], b"PK");
}

fn para_text(inlines: &[rfc2epub::model::Inline]) -> String {
    use rfc2epub::model::Inline;
    inlines
        .iter()
        .map(|i| match i {
            Inline::Text(t) => t.clone(),
            _ => String::new(),
        })
        .collect()
}
