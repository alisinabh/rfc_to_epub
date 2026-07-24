//! Fetch and embed the image assets a Markdown document references.
//!
//! Markdown parsing yields [`Block::Figure`] / [`Inline::Image`] carrying the
//! *original* (usually relative) source path, e.g. `../assets/eip-1/foo.png`.
//! This module walks the IR, resolves each path against the document's raw-URL
//! base, downloads the bytes (cached), embeds them as an [`Asset`], and rewrites
//! the resource to the in-EPUB path. Images that fail to download are rewritten
//! to an empty resource, which the renderer degrades to the alt text — a diagram
//! must never fail the build.
//!
//! Two collection quirks are handled here: absolute image URLs pass through
//! unchanged (they can't be embedded, so they're kept as-is only if already an
//! `http(s)` link — otherwise dropped), and the ERCs repo's `eip-N` asset paths
//! are rewritten to their on-disk `erc-N` form (the Jekyll build does the same).

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::fetch::http_get_bytes;
use crate::model::{Asset, Block, Collection, Document, Inline, Section};

/// Download and embed every referenced image, rewriting resource paths in place.
pub fn resolve(doc: &mut Document, base: &str, cache_dir: Option<&Path>, collection: Collection) {
    let mut resolver = Resolver {
        base: base.to_string(),
        cache_dir,
        collection,
        memo: HashMap::new(),
        assets: Vec::new(),
        counter: 0,
    };

    let mut sections = std::mem::take(&mut doc.sections);
    for section in &mut sections {
        resolver.walk_section(section);
    }
    doc.sections = sections;

    let mut abstract_ = std::mem::take(&mut doc.abstract_);
    resolver.walk_blocks(&mut abstract_);
    doc.abstract_ = abstract_;

    doc.assets = resolver.assets;
}

struct Resolver<'a> {
    base: String,
    cache_dir: Option<&'a Path>,
    collection: Collection,
    /// original path → resolved EPUB path (empty string means "failed/dropped").
    memo: HashMap<String, String>,
    assets: Vec<Asset>,
    counter: usize,
}

impl Resolver<'_> {
    fn walk_section(&mut self, section: &mut Section) {
        self.walk_blocks(&mut section.blocks);
        for sub in &mut section.subsections {
            self.walk_section(sub);
        }
    }

    fn walk_blocks(&mut self, blocks: &mut [Block]) {
        for block in blocks {
            self.walk_block(block);
        }
    }

    fn walk_block(&mut self, block: &mut Block) {
        match block {
            Block::Figure { resource, caption, .. } => {
                *resource = self.resource(resource);
                if let Some(cap) = caption {
                    self.walk_inlines(cap);
                }
            }
            Block::Paragraph(inlines) => self.walk_inlines(inlines),
            Block::List(list) => {
                for item in &mut list.items {
                    self.walk_blocks(item);
                }
            }
            Block::DefinitionList(entries) => {
                for entry in entries {
                    self.walk_inlines(&mut entry.term);
                    self.walk_blocks(&mut entry.description);
                }
            }
            Block::Table(table) => {
                for cell in &mut table.head {
                    self.walk_inlines(cell);
                }
                for row in &mut table.rows {
                    for cell in row {
                        self.walk_inlines(cell);
                    }
                }
            }
            Block::Aside(blocks) | Block::Quote(blocks) => self.walk_blocks(blocks),
            _ => {}
        }
    }

    fn walk_inlines(&mut self, inlines: &mut [Inline]) {
        for inline in inlines {
            match inline {
                Inline::Image { resource, .. } => *resource = self.resource(resource),
                Inline::Emph(inner)
                | Inline::Strong(inner)
                | Inline::Strikethrough(inner)
                | Inline::Link { text: inner, .. }
                | Inline::XRef { text: inner, .. } => self.walk_inlines(inner),
                _ => {}
            }
        }
    }

    /// Resolve one original resource path to an EPUB path (memoized). Returns an
    /// empty string when the image can't be embedded.
    fn resource(&mut self, original: &str) -> String {
        if let Some(path) = self.memo.get(original) {
            return path.clone();
        }
        let resolved = self.download(original).unwrap_or_default();
        self.memo.insert(original.to_string(), resolved.clone());
        resolved
    }

    fn download(&mut self, original: &str) -> Option<String> {
        let ext = image_ext(original)?; // only known image extensions
        let url = self.absolute_url(original)?;
        let bytes = self.fetch_bytes(&url, ext)?;
        self.counter += 1;
        let epub_path = format!("assets/img{}.{ext}", self.counter);
        self.assets.push(Asset {
            path: epub_path.clone(),
            mime: mime_for(ext).to_string(),
            bytes,
        });
        Some(epub_path)
    }

    /// Turn a possibly-relative source path into an absolute raw URL.
    fn absolute_url(&self, original: &str) -> Option<String> {
        if original.starts_with("http://") || original.starts_with("https://") {
            return Some(original.to_string());
        }
        // ERCs reference assets with the `eip-N` *directory* prefix, but on disk
        // that directory is `erc-N` (the file *names* inside keep `eip-`); the
        // Jekyll build rewrites the same way.
        let path = if self.collection == Collection::Erc {
            rewrite_erc_asset_dir(original)
        } else {
            original.to_string()
        };
        join_url(&self.base, &path)
    }

    fn fetch_bytes(&self, url: &str, ext: &str) -> Option<Vec<u8>> {
        let cache_name = format!(
            "asset-{}.{ext}",
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, url.as_bytes()).simple()
        );
        if let Some(dir) = self.cache_dir {
            let path = dir.join("assets").join(&cache_name);
            if path.exists() {
                if let Ok(bytes) = std::fs::read(&path) {
                    return Some(bytes);
                }
            }
        }
        let bytes = http_get_bytes(url).ok()??;
        if let Some(dir) = self.cache_dir {
            let adir = dir.join("assets");
            std::fs::create_dir_all(&adir).ok();
            std::fs::write(adir.join(&cache_name), &bytes).ok();
        }
        Some(bytes)
    }
}

/// Rewrite the `eip-N` asset *directory* segment to `erc-N` (leaving file names,
/// which keep the `eip-` prefix on disk, untouched). `../assets/eip-7401/img/
/// eip-7401-foo.png` → `../assets/erc-7401/img/eip-7401-foo.png`.
fn rewrite_erc_asset_dir(path: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"/eip-(\d+)/").expect("valid regex"));
    re.replace_all(path, "/erc-$1/").into_owned()
}

/// Resolve `rel` against a raw-URL directory `base` (which ends in `/`),
/// honouring `.` and `..` segments.
fn join_url(base: &str, rel: &str) -> Option<String> {
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return Some(rel.to_string());
    }
    let scheme_end = base.find("://")? + 3;
    let host_end = base[scheme_end..].find('/').map(|i| scheme_end + i)?;
    let origin = &base[..host_end];
    let mut segs: Vec<&str> = base[host_end..].split('/').filter(|s| !s.is_empty()).collect();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            p => segs.push(p),
        }
    }
    Some(format!("{origin}/{}", segs.join("/")))
}

/// The lowercased image extension for a path, if it is a supported image type.
fn image_ext(path: &str) -> Option<&'static str> {
    let path = path.split(['#', '?']).next().unwrap_or(path);
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("png"),
        "jpg" | "jpeg" => Some("jpg"),
        "gif" => Some("gif"),
        "svg" => Some("svg"),
        "webp" => Some("webp"),
        _ => None,
    }
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_asset_paths() {
        let base = "https://raw.githubusercontent.com/ethereum/EIPs/master/EIPS/";
        assert_eq!(
            join_url(base, "../assets/eip-1/foo.png").as_deref(),
            Some("https://raw.githubusercontent.com/ethereum/EIPs/master/assets/eip-1/foo.png"),
        );
        assert_eq!(
            join_url(base, "./diagram.svg").as_deref(),
            Some("https://raw.githubusercontent.com/ethereum/EIPs/master/EIPS/diagram.svg"),
        );
    }

    #[test]
    fn erc_rewrite_renames_directory_not_filename() {
        // Only the `eip-7401` directory segment becomes `erc-7401`; the file
        // name keeps its `eip-` prefix (that is how the ERCs repo is laid out).
        assert_eq!(
            rewrite_erc_asset_dir("../assets/eip-7401/img/eip-7401-nestable.png"),
            "../assets/erc-7401/img/eip-7401-nestable.png",
        );
        assert_eq!(
            rewrite_erc_asset_dir("../assets/eip-20/foo.png"),
            "../assets/erc-20/foo.png",
        );
    }

    #[test]
    fn recognizes_image_extensions_only() {
        assert_eq!(image_ext("foo.PNG"), Some("png"));
        assert_eq!(image_ext("a/b/c.jpeg"), Some("jpg"));
        assert_eq!(image_ext("d.svg#frag"), Some("svg"));
        assert_eq!(image_ext("notes.md"), None);
        assert_eq!(image_ext("noext"), None);
    }
}
