//! `rfc2epub` — command-line front-end for the `rfc2epub` library.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use rfc2epub::model::{SourceKind, SvgMode};
use rfc2epub::{fetch::SourcePref, Options};

/// Convert IETF RFCs into clean, reflowable EPUB files for e-readers.
#[derive(Parser, Debug)]
#[command(name = "rfc2epub", version, about, long_about = None)]
struct Cli {
    /// RFC numbers to convert, e.g. `9110 8446 791`.
    #[arg(value_name = "RFC", required_unless_present = "input")]
    rfcs: Vec<u32>,

    /// Convert a local RFC source file instead of fetching (XML or text).
    #[arg(long, value_name = "FILE", conflicts_with = "rfcs")]
    input: Option<PathBuf>,

    /// Write output to this exact file (only valid for a single RFC).
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

    /// Do not read or write the download cache.
    #[arg(long)]
    no_cache: bool,

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
    if cli.output.is_some() && cli.rfcs.len() > 1 {
        bail!("--output can only be used with a single RFC; use --out-dir for batches");
    }

    let opts = Options {
        source: cli.format.into(),
        cache_dir: if cli.no_cache {
            None
        } else {
            rfc2epub::fetch::default_cache_dir()
        },
        svg_mode: cli.svg_mode.into(),
    };

    if let Some(input) = &cli.input {
        return convert_local(input, cli.output.as_deref(), &cli.out_dir, opts.svg_mode, cli.quiet);
    }

    std::fs::create_dir_all(&cli.out_dir)
        .with_context(|| format!("creating output dir {}", cli.out_dir.display()))?;

    let mut failures = 0;
    for &n in &cli.rfcs {
        let out = cli
            .output
            .clone()
            .unwrap_or_else(|| cli.out_dir.join(format!("rfc{n}.epub")));
        let spinner = spinner(cli.quiet, &format!("RFC {n}: fetching & converting"));
        match rfc2epub::convert_rfc(n, &out, &opts) {
            Ok(()) => finish_ok(&spinner, cli.quiet, &format!("RFC {n} → {}", out.display())),
            Err(e) => {
                finish_err(&spinner, cli.quiet, &format!("RFC {n}: {e}"));
                failures += 1;
            }
        }
    }

    if failures > 0 {
        bail!("{failures} of {} RFC(s) failed", cli.rfcs.len());
    }
    Ok(())
}

fn convert_local(
    input: &std::path::Path,
    output: Option<&std::path::Path>,
    out_dir: &std::path::Path,
    svg_mode: SvgMode,
    quiet: bool,
) -> Result<()> {
    let body = std::fs::read_to_string(input)
        .with_context(|| format!("reading {}", input.display()))?;
    let kind = sniff_kind(input, &body);

    let spinner = spinner(quiet, &format!("Converting {}", input.display()));
    let doc = rfc2epub::parse_source(&body, kind, None)
        .with_context(|| format!("parsing {}", input.display()))?;
    let bytes = rfc2epub::render::to_epub(&doc, svg_mode).context("rendering EPUB")?;

    let out = output.map(PathBuf::from).unwrap_or_else(|| {
        let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("rfc");
        out_dir.join(format!("{stem}.epub"))
    });
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&out, bytes).with_context(|| format!("writing {}", out.display()))?;
    finish_ok(&spinner, quiet, &format!("{} → {}", input.display(), out.display()));
    Ok(())
}

/// Guess whether a local file is XML or text.
fn sniff_kind(path: &std::path::Path, body: &str) -> SourceKind {
    if path.extension().and_then(|e| e.to_str()) == Some("xml") {
        return SourceKind::Xml;
    }
    let head = &body[..body.len().min(2048)];
    if head.contains("<rfc") {
        SourceKind::Xml
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
