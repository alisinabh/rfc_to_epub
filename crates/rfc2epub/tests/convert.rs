//! End-to-end tests for both parsers and the EPUB renderer, using inline
//! sources (no network).

use rfc2epub::model::{Block, SourceKind, SvgMode};
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
    assert_eq!(doc.number, Some(9999));
    assert_eq!(doc.title, "A Test Document");
    assert_eq!(doc.authors.len(), 1);
    assert_eq!(doc.authors[0].name, "A. Author");
    assert_eq!(doc.date.as_deref(), Some("June 2022")); // numeric month mapped
    assert_eq!(doc.obsoletes, vec![1234]);
    assert_eq!(doc.category.as_deref(), Some("Informational"));
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
        let bytes = render::to_epub(&doc, mode).unwrap();
        // EPUB is a ZIP; ZIP files start with "PK".
        assert_eq!(&bytes[..2], b"PK");
        // The uncompressed `mimetype` string appears near the start.
        let head = String::from_utf8_lossy(&bytes[..200]);
        assert!(head.contains("application/epub+zip"));
    }
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
