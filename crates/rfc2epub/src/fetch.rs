//! Fetching document sources, with on-disk caching and per-collection routing.
//!
//! Every collection shares the same HTTP/caching plumbing ([`http_get`],
//! [`http_get_bytes`], [`cached`]) and differs only in *where* the source lives
//! and *which* format(s) to probe:
//!
//! * **RFC** — the RFC Editor: canonical xml2rfc **v3** when it exists (only
//!   newer RFCs have it), else the published plain text.
//! * **EIP** — `ethereum/EIPs` `EIPS/eip-N.md`; a `status: Moved` tombstone
//!   redirects to `ethereum/ERCs` `ERCS/erc-N.md`.
//! * **ERC** — `ethereum/ERCs` `ERCS/erc-N.md` directly.
//! * **BIP** — `bitcoin/bips`, probing `bip-NNNN.md` then `bip-NNNN.mediawiki`.
//! * **CAIP** — `ChainAgnostic/CAIPs` `CAIPs/caip-N.md`.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::{Collection, SourceKind};

const RFC_BASE: &str = "https://www.rfc-editor.org/rfc";
const EIP_BASE: &str = "https://raw.githubusercontent.com/ethereum/EIPs/master/EIPS/";
const ERC_BASE: &str = "https://raw.githubusercontent.com/ethereum/ERCs/master/ERCS/";
const BIP_BASE: &str = "https://raw.githubusercontent.com/bitcoin/bips/master/";
const CAIP_BASE: &str = "https://raw.githubusercontent.com/ChainAgnostic/CAIPs/main/CAIPs/";
const BOLT_BASE: &str = "https://raw.githubusercontent.com/lightning/bolts/master/";

/// Generous cap; the largest sources are a few MB.
const BODY_LIMIT: u64 = 64 * 1024 * 1024;

/// Which source format the caller wants (RFC only; other collections have a
/// single canonical format).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourcePref {
    /// Try XML v3, fall back to text. The default.
    #[default]
    Auto,
    Xml,
    Text,
}

/// A document to fetch: a collection and a number (`eip-1559`, `rfc-9110`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocSpec {
    pub collection: Collection,
    pub number: u32,
}

impl DocSpec {
    pub fn new(collection: Collection, number: u32) -> Self {
        Self { collection, number }
    }

    /// Parse a CLI id: `"eip-1559"`, `"erc-20"`, `"bip-341"`, `"rfc-9110"`,
    /// `"caip-2"`, or a bare number (`"9110"` → RFC, for backwards
    /// compatibility). Collection and number may be separated by `-` or a space.
    pub fn parse(s: &str) -> Option<DocSpec> {
        let s = s.trim();
        if let Ok(n) = s.parse::<u32>() {
            return Some(DocSpec::new(Collection::Rfc, n));
        }
        let (tok, rest) = s.split_once(['-', ' '])?;
        let collection = Collection::from_token(tok)?;
        let number: u32 = rest.trim().parse().ok()?;
        Some(DocSpec::new(collection, number))
    }

    /// The display label, e.g. `"EIP-1559"`.
    pub fn label(self) -> String {
        self.collection.label(self.number)
    }
}

/// A fetched source together with how to parse it and where its relative assets
/// live.
pub struct Fetched {
    pub body: String,
    pub kind: SourceKind,
    /// The resolved collection (an EIP fetch may resolve to an ERC).
    pub collection: Collection,
    pub number: u32,
    /// Raw-URL base against which relative image/asset paths resolve. `None` for
    /// sources without assets (RFCs).
    pub asset_base: Option<String>,
}

/// Fetch the best available source for `spec`.
pub fn fetch(spec: DocSpec, pref: SourcePref, cache_dir: Option<&Path>) -> Result<Fetched> {
    match spec.collection {
        Collection::Rfc => fetch_rfc(spec.number, pref, cache_dir),
        Collection::Eip => fetch_eip(spec.number, cache_dir),
        Collection::Erc => fetch_erc(spec.number, cache_dir),
        Collection::Bip => fetch_bip(spec.number, cache_dir),
        Collection::Caip => fetch_caip(spec.number, cache_dir),
        Collection::Bolt => fetch_bolt(spec.number, cache_dir),
    }
}

/// Fetch an RFC (XML v3 preferred, plain-text fallback), honouring `pref`.
pub fn fetch_rfc(number: u32, pref: SourcePref, cache_dir: Option<&Path>) -> Result<Fetched> {
    let not_found = || Error::NotFound(format!("RFC {number}"));
    let base = |body, kind| Fetched {
        body,
        kind,
        collection: Collection::Rfc,
        number,
        asset_base: None,
    };
    match pref {
        SourcePref::Xml => {
            let body = fetch_rfc_kind(number, SourceKind::Xml, cache_dir)?.ok_or_else(not_found)?;
            Ok(base(body, SourceKind::Xml))
        }
        SourcePref::Text => {
            let body =
                fetch_rfc_kind(number, SourceKind::Text, cache_dir)?.ok_or_else(not_found)?;
            Ok(base(body, SourceKind::Text))
        }
        SourcePref::Auto => {
            if let Some(xml) = fetch_rfc_kind(number, SourceKind::Xml, cache_dir)? {
                if is_xml_v3(&xml) {
                    return Ok(base(xml, SourceKind::Xml));
                }
            }
            let body =
                fetch_rfc_kind(number, SourceKind::Text, cache_dir)?.ok_or_else(not_found)?;
            Ok(base(body, SourceKind::Text))
        }
    }
}

/// Fetch one RFC format, returning `Ok(None)` on a clean 404.
fn fetch_rfc_kind(
    number: u32,
    kind: SourceKind,
    cache_dir: Option<&Path>,
) -> Result<Option<String>> {
    let ext = match kind {
        SourceKind::Xml => "xml",
        _ => "txt",
    };
    let filename = format!("rfc{number}.{ext}");
    let url = format!("{RFC_BASE}/{filename}");
    cached(&filename, &url, cache_dir)
}

fn fetch_eip(number: u32, cache_dir: Option<&Path>) -> Result<Fetched> {
    let url = format!("{EIP_BASE}eip-{number}.md");
    let body = cached(&format!("eip-{number}.md"), &url, cache_dir)?
        .ok_or_else(|| Error::NotFound(format!("EIP-{number}")))?;
    // A `status: Moved` tombstone redirects to the ERCs repo.
    if frontmatter_status(&body).is_some_and(|s| s.eq_ignore_ascii_case("Moved")) {
        return fetch_erc(number, cache_dir);
    }
    Ok(Fetched {
        body,
        kind: SourceKind::Markdown,
        collection: Collection::Eip,
        number,
        asset_base: Some(EIP_BASE.to_string()),
    })
}

fn fetch_erc(number: u32, cache_dir: Option<&Path>) -> Result<Fetched> {
    let url = format!("{ERC_BASE}erc-{number}.md");
    let body = cached(&format!("erc-{number}.md"), &url, cache_dir)?
        .ok_or_else(|| Error::NotFound(format!("ERC-{number}")))?;
    Ok(Fetched {
        body,
        kind: SourceKind::Markdown,
        collection: Collection::Erc,
        number,
        asset_base: Some(ERC_BASE.to_string()),
    })
}

fn fetch_bip(number: u32, cache_dir: Option<&Path>) -> Result<Fetched> {
    let padded = format!("{number:04}");
    // Probe Markdown first, then MediaWiki (196 of 210 BIPs are mediawiki).
    let md_url = format!("{BIP_BASE}bip-{padded}.md");
    if let Some(body) = cached(&format!("bip-{padded}.md"), &md_url, cache_dir)? {
        return Ok(Fetched {
            body,
            kind: SourceKind::Markdown,
            collection: Collection::Bip,
            number,
            asset_base: Some(BIP_BASE.to_string()),
        });
    }
    let wiki_url = format!("{BIP_BASE}bip-{padded}.mediawiki");
    let body = cached(&format!("bip-{padded}.mediawiki"), &wiki_url, cache_dir)?
        .ok_or_else(|| Error::NotFound(format!("BIP {number}")))?;
    Ok(Fetched {
        body,
        kind: SourceKind::Mediawiki,
        collection: Collection::Bip,
        number,
        asset_base: Some(BIP_BASE.to_string()),
    })
}

/// Fetch a Lightning BOLT. BOLT filenames embed a title (`11-payment-encoding.md`)
/// and `raw.githubusercontent.com` can't be listed, so the number→filename map
/// ([`crate::model::bolt_filename`]) resolves the path. Content is plain GFM with
/// no frontmatter, so it flows through the Markdown parser like any other spec.
fn fetch_bolt(number: u32, cache_dir: Option<&Path>) -> Result<Fetched> {
    let filename = crate::model::bolt_filename(number)
        .ok_or_else(|| Error::NotFound(format!("BOLT {number}")))?;
    let url = format!("{BOLT_BASE}{filename}");
    let body = cached(filename, &url, cache_dir)?
        .ok_or_else(|| Error::NotFound(format!("BOLT {number}")))?;
    Ok(Fetched {
        body,
        kind: SourceKind::Markdown,
        collection: Collection::Bolt,
        number,
        asset_base: Some(BOLT_BASE.to_string()),
    })
}

fn fetch_caip(number: u32, cache_dir: Option<&Path>) -> Result<Fetched> {
    let url = format!("{CAIP_BASE}caip-{number}.md");
    let body = cached(&format!("caip-{number}.md"), &url, cache_dir)?
        .ok_or_else(|| Error::NotFound(format!("CAIP-{number}")))?;
    Ok(Fetched {
        body,
        kind: SourceKind::Markdown,
        collection: Collection::Caip,
        number,
        asset_base: Some(CAIP_BASE.to_string()),
    })
}

/// Read `filename` from the cache if present, else GET `url` (returning
/// `Ok(None)` on a 404) and cache the result.
fn cached(filename: &str, url: &str, cache_dir: Option<&Path>) -> Result<Option<String>> {
    if let Some(dir) = cache_dir {
        let path = dir.join(filename);
        if path.exists() {
            let bytes = std::fs::read(&path).map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
            return Ok(Some(decode_body(bytes)));
        }
    }
    let Some(body) = http_get(url)? else {
        return Ok(None);
    };
    if let Some(dir) = cache_dir {
        std::fs::create_dir_all(dir).ok();
        std::fs::write(dir.join(filename), &body).ok();
    }
    Ok(Some(body))
}

/// GET a URL as text. Returns `Ok(None)` for a 404, `Err` for other failures.
fn http_get(url: &str) -> Result<Option<String>> {
    let mut resp = get(url)?;
    let status = resp.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(Error::Parse(format!("unexpected HTTP {status} for {url}")));
    }
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(BODY_LIMIT)
        .read_to_vec()
        .map_err(|source| Error::Http {
            url: url.to_string(),
            source: Box::new(source),
        })?;
    Ok(Some(decode_body(bytes)))
}

/// Decode a fetched source. RFC text predating the RFC Editor's UTF-8 policy is
/// ISO-8859-1 — RFC 64 carries a lone `0xB5` (`µ`) — and the server declares
/// `charset=utf-8` for every `.txt` regardless, so the encoding has to be
/// sniffed: valid UTF-8 wins, otherwise each byte maps 1:1 to its Latin-1
/// codepoint. That fallback is total, so decoding never fails.
fn decode_body(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
    }
}

/// GET a URL as raw bytes (for images). `Ok(None)` on 404.
pub fn http_get_bytes(url: &str) -> Result<Option<Vec<u8>>> {
    let mut resp = get(url)?;
    let status = resp.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(Error::Parse(format!("unexpected HTTP {status} for {url}")));
    }
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(BODY_LIMIT)
        .read_to_vec()
        .map_err(|source| Error::Http {
            url: url.to_string(),
            source: Box::new(source),
        })?;
    Ok(Some(bytes))
}

/// POST `body` to `url` and return the response text, or `None` on any failure
/// (network error or non-2xx status) so callers can degrade gracefully. Used by
/// the opt-in Kroki mermaid-rendering fallback.
pub fn http_post_text(url: &str, body: &str) -> Option<String> {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(concat!("rfc2epub/", env!("CARGO_PKG_VERSION")))
        .build();
    let agent: ureq::Agent = config.into();
    let mut resp = agent.post(url).send(body).ok()?;
    if !(200..300).contains(&resp.status().as_u16()) {
        return None;
    }
    resp.body_mut()
        .with_config()
        .limit(BODY_LIMIT)
        .read_to_string()
        .ok()
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>> {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(concat!("rfc2epub/", env!("CARGO_PKG_VERSION")))
        .build();
    let agent: ureq::Agent = config.into();
    agent.get(url).call().map_err(|source| Error::Http {
        url: url.to_string(),
        source: Box::new(source),
    })
}

/// Heuristic: does this XML declare itself as xml2rfc v3?
fn is_xml_v3(xml: &str) -> bool {
    let head = char_boundary_prefix(xml, 2048);
    head.contains("version=\"3\"") || head.contains("version='3'")
}

/// The longest prefix of `s` no longer than `max_bytes` that ends on a UTF-8
/// char boundary. Plain byte-slicing (`&s[..max_bytes]`) panics when the cut
/// falls inside a multi-byte character — RFC text and xml2rfc metadata can carry
/// non-ASCII within the first 2 KB, so the naive slice is a real crash.
pub fn char_boundary_prefix(s: &str, max_bytes: usize) -> &str {
    let mut end = s.len().min(max_bytes);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The `status:` value from a `---`-delimited frontmatter block, if any.
fn frontmatter_status(body: &str) -> Option<String> {
    let body = body.trim_start();
    let rest = body.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    for line in rest[..end].lines() {
        let line = line.trim();
        if let Some(v) = line
            .strip_prefix("status:")
            .or_else(|| line.strip_prefix("Status:"))
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// Default cache directory (`~/Library/Caches/rfc2epub` on macOS, etc.).
pub fn default_cache_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("org", "rfc2epub", "rfc2epub")
        .map(|d| d.cache_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_doc_specs() {
        assert_eq!(
            DocSpec::parse("9110"),
            Some(DocSpec::new(Collection::Rfc, 9110))
        );
        assert_eq!(
            DocSpec::parse("rfc-9110"),
            Some(DocSpec::new(Collection::Rfc, 9110))
        );
        assert_eq!(
            DocSpec::parse("eip-1559"),
            Some(DocSpec::new(Collection::Eip, 1559))
        );
        assert_eq!(
            DocSpec::parse("ERC-20"),
            Some(DocSpec::new(Collection::Erc, 20))
        );
        assert_eq!(
            DocSpec::parse("bip-341"),
            Some(DocSpec::new(Collection::Bip, 341))
        );
        assert_eq!(
            DocSpec::parse("caip 2"),
            Some(DocSpec::new(Collection::Caip, 2))
        );
        assert_eq!(
            DocSpec::parse("bolt-11"),
            Some(DocSpec::new(Collection::Bolt, 11))
        );
        assert_eq!(DocSpec::parse("nope-1"), None);
        assert_eq!(DocSpec::parse("eip-x"), None);
    }

    #[test]
    fn bolt_number_maps_to_titled_filename() {
        use crate::model::bolt_filename;
        assert_eq!(bolt_filename(11), Some("11-payment-encoding.md"));
        assert_eq!(bolt_filename(0), Some("00-introduction.md"));
        // BOLT 6 does not exist; an unknown number has no filename.
        assert_eq!(bolt_filename(6), None);
        assert_eq!(bolt_filename(99), None);
    }

    #[test]
    fn char_boundary_prefix_never_splits_a_codepoint() {
        // A multi-byte char straddling the cut must not panic; the prefix backs
        // up to the previous boundary.
        let s = format!("{}\u{2014}tail", "a".repeat(2047)); // em-dash at bytes 2047..2050
        let head = char_boundary_prefix(&s, 2048);
        assert_eq!(head.len(), 2047); // backed up before the em-dash
        assert!(head.is_char_boundary(head.len()));
        // Short strings pass through unchanged.
        assert_eq!(char_boundary_prefix("hi", 2048), "hi");
        // xml2rfc detection still works with non-ASCII before the marker.
        let xml = format!(
            "<?xml?><rfc version=\"3\" who=\"\u{00e9}\">{}",
            "x".repeat(4000)
        );
        assert!(is_xml_v3(&xml));
    }

    #[test]
    fn decode_body_prefers_utf8_over_latin1() {
        assert_eq!(decode_body(b"plain ascii".to_vec()), "plain ascii");
        // Valid UTF-8 decodes as UTF-8, not as its two Latin-1 bytes.
        assert_eq!(
            decode_body("12 \u{b5}sec".as_bytes().to_vec()),
            "12 \u{b5}sec"
        );
        // A bare 0xB5 is not valid UTF-8; Latin-1 maps it to the same char.
        assert_eq!(decode_body(b"12 \xb5sec".to_vec()), "12 \u{b5}sec");
        assert_eq!(decode_body(Vec::new()), "");
    }

    #[test]
    fn reads_latin1_cached_source() {
        // RFC 64 is ISO-8859-1: a lone 0xB5 (`µ`) in "12 µsec per double word".
        // Strict UTF-8 decoding rejects it and the whole conversion fails.
        let dir = std::env::temp_dir().join(format!("rfc2epub-latin1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rfc64.txt");
        std::fs::write(
            &path,
            b"This takes approximately 12 \xb5sec per double word.\n",
        )
        .unwrap();
        let body = cached("rfc64.txt", "http://example.invalid/rfc64.txt", Some(&dir))
            .expect("latin-1 cached source should decode")
            .expect("cached file should be found");
        std::fs::remove_dir_all(&dir).ok();
        assert!(body.contains("12 \u{b5}sec"), "got {body:?}");
    }

    #[test]
    fn detects_moved_frontmatter_status() {
        let moved = "---\neip: 20\nstatus: Moved\n---\nbody";
        assert_eq!(frontmatter_status(moved).as_deref(), Some("Moved"));
        let final_ = "---\neip: 1559\nstatus: Final\n---\nbody";
        assert_eq!(frontmatter_status(final_).as_deref(), Some("Final"));
        assert_eq!(frontmatter_status("no frontmatter here"), None);
    }
}
