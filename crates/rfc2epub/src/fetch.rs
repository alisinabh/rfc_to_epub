//! Fetching RFC sources from the RFC Editor, with on-disk caching and
//! automatic best-format selection.
//!
//! Format strategy: prefer canonical xml2rfc **v3** when it exists (only newer
//! RFCs have it), and fall back to the published plain text otherwise. The
//! auto-detector fetches the XML and only accepts it when it really is v3;
//! auto-generated or absent XML falls through to text.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::SourceKind;

const BASE: &str = "https://www.rfc-editor.org/rfc";
/// Generous cap; the largest RFCs are a few MB.
const BODY_LIMIT: u64 = 64 * 1024 * 1024;

/// Which source format the caller wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourcePref {
    /// Try XML v3, fall back to text. The default.
    #[default]
    Auto,
    Xml,
    Text,
}

/// A fetched RFC source together with the format it turned out to be.
pub struct Fetched {
    pub body: String,
    pub kind: SourceKind,
}

/// Fetch the best available source for `number`, honouring `pref` and using
/// `cache_dir` (when `Some`) to avoid repeat downloads.
pub fn fetch_rfc(number: u32, pref: SourcePref, cache_dir: Option<&Path>) -> Result<Fetched> {
    match pref {
        SourcePref::Xml => {
            let body = fetch_kind(number, SourceKind::Xml, cache_dir)?
                .ok_or(Error::NotFound(number))?;
            Ok(Fetched { body, kind: SourceKind::Xml })
        }
        SourcePref::Text => {
            let body = fetch_kind(number, SourceKind::Text, cache_dir)?
                .ok_or(Error::NotFound(number))?;
            Ok(Fetched { body, kind: SourceKind::Text })
        }
        SourcePref::Auto => {
            if let Some(xml) = fetch_kind(number, SourceKind::Xml, cache_dir)? {
                if is_xml_v3(&xml) {
                    return Ok(Fetched { body: xml, kind: SourceKind::Xml });
                }
            }
            let body = fetch_kind(number, SourceKind::Text, cache_dir)?
                .ok_or(Error::NotFound(number))?;
            Ok(Fetched { body, kind: SourceKind::Text })
        }
    }
}

/// Fetch one specific format, returning `Ok(None)` on a clean 404.
fn fetch_kind(number: u32, kind: SourceKind, cache_dir: Option<&Path>) -> Result<Option<String>> {
    let ext = match kind {
        SourceKind::Xml => "xml",
        _ => "txt",
    };
    let filename = format!("rfc{number}.{ext}");

    if let Some(dir) = cache_dir {
        let path = dir.join(&filename);
        if path.exists() {
            let body = std::fs::read_to_string(&path).map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
            return Ok(Some(body));
        }
    }

    let url = format!("{BASE}/{filename}");
    let Some(body) = http_get(&url)? else {
        return Ok(None);
    };

    if let Some(dir) = cache_dir {
        std::fs::create_dir_all(dir).ok();
        let path = dir.join(&filename);
        std::fs::write(&path, &body).ok();
    }
    Ok(Some(body))
}

/// GET a URL. Returns `Ok(None)` for a 404, `Err` for other failures.
fn http_get(url: &str) -> Result<Option<String>> {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(concat!("rfc2epub/", env!("CARGO_PKG_VERSION")))
        .build();
    let agent: ureq::Agent = config.into();

    let mut resp = agent.get(url).call().map_err(|source| Error::Http {
        url: url.to_string(),
        source: Box::new(source),
    })?;

    let status = resp.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(Error::Parse(format!("unexpected HTTP {status} for {url}")));
    }

    let body = resp
        .body_mut()
        .with_config()
        .limit(BODY_LIMIT)
        .read_to_string()
        .map_err(|source| Error::Http {
            url: url.to_string(),
            source: Box::new(source),
        })?;
    Ok(Some(body))
}

/// Heuristic: does this XML declare itself as xml2rfc v3?
fn is_xml_v3(xml: &str) -> bool {
    // Look only near the opening <rfc ...> tag.
    let head = &xml[..xml.len().min(2048)];
    head.contains("version=\"3\"") || head.contains("version='3'")
}

/// Default cache directory (`~/Library/Caches/rfc2epub` on macOS, etc.).
pub fn default_cache_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("org", "rfc2epub", "rfc2epub")
        .map(|d| d.cache_dir().to_path_buf())
}
