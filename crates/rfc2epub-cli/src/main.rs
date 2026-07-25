//! `rfc2epub` — command-line front-end for the `rfc2epub` library.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use rfc2epub::fetch::DocSpec;
use rfc2epub::model::{SourceKind, SvgMode};
use rfc2epub::{fetch::SourcePref, Options};

/// Convert IETF RFCs and Markdown spec collections into clean, reflowable EPUBs.
#[derive(Parser, Debug)]
#[command(name = "rfc2epub", version, about, long_about = None)]
struct Cli {
    /// Documents to convert: a bare RFC number (`9110`) or a collection-qualified
    /// id (`eip-1559`, `erc-20`, `bip-341`, `rfc-8446`, `caip-2`).
    #[arg(value_name = "DOC", required_unless_present = "input")]
    docs: Vec<String>,

    /// Convert a local source file instead of fetching (XML, text, or Markdown).
    #[arg(long, value_name = "FILE", conflicts_with = "docs")]
    input: Option<PathBuf>,

    /// Write output to this exact file (only valid for a single document).
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Directory to write generated EPUBs into.
    #[arg(short = 'd', long, value_name = "DIR", default_value = ".")]
    out_dir: PathBuf,

    /// Which source format to prefer.
    #[arg(short, long, value_enum, default_value_t = Format::Auto)]
    format: Format,

    /// How diagrams (ASCII art / code) adapt to the reader's theme.
    #[arg(long, value_enum, default_value_t = SvgModeArg::Inline)]
    svg_mode: SvgModeArg,

    /// Do not reproduce the original document's page breaks (kept by default;
    /// only plain-text sources carry pagination).
    #[arg(long)]
    no_page_breaks: bool,

    /// Do not read or write the download cache.
    #[arg(long)]
    no_cache: bool,

    /// Render mermaid diagrams the in-process engine can't handle via the Kroki
    /// service (sends the diagram source to https://kroki.io).
    #[arg(long)]
    online_mermaid: bool,

    /// Suppress progress output.
    #[arg(short, long)]
    quiet: bool,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Format {
    /// Prefer XML v3, fall back to text.
    Auto,
    Xml,
    Text,
}

impl From<Format> for SourcePref {
    fn from(f: Format) -> Self {
        match f {
            Format::Auto => SourcePref::Auto,
            Format::Xml => SourcePref::Xml,
            Format::Text => SourcePref::Text,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum SvgModeArg {
    /// Inline SVG that follows the reader's light/dark theme (looks best, incl. Kindle).
    Inline,
    /// Referenced light "card" images: self-contained and epubcheck-clean, stays light in dark mode.
    Card,
}

impl From<SvgModeArg> for SvgMode {
    fn from(m: SvgModeArg) -> Self {
        match m {
            SvgModeArg::Card => SvgMode::Card,
            SvgModeArg::Inline => SvgMode::Inline,
        }
    }
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("{} {e:#}", "error:".red().bold());
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    if cli.output.is_some() && cli.docs.len() > 1 {
        bail!("--output can only be used with a single document; use --out-dir for batches");
    }

    let opts = Options {
        source: cli.format.into(),
        cache_dir: if cli.no_cache {
            None
        } else {
            rfc2epub::fetch::default_cache_dir()
        },
        svg_mode: cli.svg_mode.into(),
        page_breaks: !cli.no_page_breaks,
        mermaid_online: cli.online_mermaid,
    };

    if let Some(input) = &cli.input {
        return convert_local(input, cli.output.as_deref(), &cli.out_dir, &opts, cli.quiet);
    }

    // Parse ids up front so a typo fails before any network work.
    let specs: Vec<(String, DocSpec)> = cli
        .docs
        .iter()
        .map(|raw| {
            DocSpec::parse(raw)
                .map(|s| (raw.clone(), s))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "unrecognized document id '{raw}' (try 9110, eip-1559, bip-341)"
                    )
                })
        })
        .collect::<Result<_>>()?;

    std::fs::create_dir_all(&cli.out_dir)
        .with_context(|| format!("creating output dir {}", cli.out_dir.display()))?;

    let mut failures = 0;
    for (raw, spec) in &specs {
        let label = spec.label();
        let out = cli
            .output
            .clone()
            .unwrap_or_else(|| cli.out_dir.join(default_filename(spec)));
        let spinner = spinner(cli.quiet, &format!("{label}: fetching & converting"));
        match rfc2epub::convert(*spec, &out, &opts) {
            Ok(()) => finish_ok(&spinner, cli.quiet, &format!("{label} → {}", out.display())),
            Err(e) => {
                finish_err(&spinner, cli.quiet, &format!("{raw}: {e}"));
                failures += 1;
            }
        }
    }

    if failures > 0 {
        bail!("{failures} of {} document(s) failed", specs.len());
    }
    Ok(())
}

/// Default output filename, e.g. `rfc9110.epub`, `eip-1559.epub`, `bip-341.epub`.
fn default_filename(spec: &DocSpec) -> String {
    use rfc2epub::model::Collection;
    match spec.collection {
        Collection::Rfc => format!("rfc{}.epub", spec.number),
        other => format!("{}-{}.epub", other.token(), spec.number),
    }
}

fn convert_local(
    input: &std::path::Path,
    output: Option<&std::path::Path>,
    out_dir: &std::path::Path,
    opts: &Options,
    quiet: bool,
) -> Result<()> {
    let body =
        std::fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?;
    let kind = sniff_kind(input, &body);

    let spinner = spinner(quiet, &format!("Converting {}", input.display()));
    let mut doc = rfc2epub::parse_source(&body, kind, None)
        .with_context(|| format!("parsing {}", input.display()))?;
    // Render any mermaid diagrams in place (in-process; `--online-mermaid` adds
    // the opt-in Kroki fallback).
    rfc2epub::diagram::resolve(&mut doc, opts.mermaid_online);
    let bytes = rfc2epub::render::to_epub(&doc, opts.svg_mode, opts.page_breaks)
        .context("rendering EPUB")?;

    let out = output.map(PathBuf::from).unwrap_or_else(|| {
        let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("rfc");
        out_dir.join(format!("{stem}.epub"))
    });
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&out, bytes).with_context(|| format!("writing {}", out.display()))?;
    finish_ok(
        &spinner,
        quiet,
        &format!("{} → {}", input.display(), out.display()),
    );
    Ok(())
}

/// Guess whether a local file is XML, Markdown, MediaWiki, or plain text.
fn sniff_kind(path: &std::path::Path, body: &str) -> SourceKind {
    match path.extension().and_then(|e| e.to_str()) {
        Some("xml") => return SourceKind::Xml,
        Some("md") | Some("markdown") => return SourceKind::Markdown,
        Some("mediawiki") | Some("wiki") => return SourceKind::Mediawiki,
        _ => {}
    }
    // Char-boundary-safe prefix: raw byte slicing panics if a multi-byte
    // character straddles the cut (non-ASCII near the top of a file).
    let head = rfc2epub::fetch::char_boundary_prefix(body, 2048);
    if head.contains("<rfc") {
        SourceKind::Xml
    } else if head.trim_start().starts_with("---") {
        // A `---` frontmatter block signals a Markdown spec file.
        SourceKind::Markdown
    } else {
        SourceKind::Text
    }
}

fn spinner(quiet: bool, msg: &str) -> Option<ProgressBar> {
    if quiet {
        return None;
    }
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    Some(pb)
}

fn finish_ok(pb: &Option<ProgressBar>, quiet: bool, msg: &str) {
    if let Some(pb) = pb {
        pb.finish_and_clear();
    }
    if !quiet {
        println!("{} {msg}", "✓".green().bold());
    }
}

fn finish_err(pb: &Option<ProgressBar>, quiet: bool, msg: &str) {
    if let Some(pb) = pb {
        pb.finish_and_clear();
    }
    let _ = quiet;
    eprintln!("{} {msg}", "✗".red().bold());
}
