#!/usr/bin/env python3
"""Build the rfc2epub GitHub Pages site.

The EPUBs themselves live as GitHub Release assets, bucketed by RFC number;
GitHub Pages only carries the search page and its index (~1 MB). The releases
double as the incremental state — whatever is already uploaded is what does not
need reconverting.

Subcommands, run in order by .github/workflows/pages.yml:

    buckets                          -> the release tags that should exist
    index    rfc-index.xml           -> work/meta.json   (every RFC + metadata)
    plan     meta.json + existing    -> work/todo/*.txt  (what still needs converting)
    place    out/*.epub              -> work/upload/NNNNN/ (grouped for upload)
    build    meta.json + published   -> site/           (rfcs.json, meta.json, web/*)

Standard library only, on purpose: this runs on a stock runner with no pip step.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import sys
import urllib.request
import xml.etree.ElementTree as ET
from datetime import datetime, timezone
from pathlib import Path

INDEX_URL = "https://www.rfc-editor.org/rfc-index.xml"
NS = "{https://www.rfc-editor.org/rfc-index}"

# One release per bucket of 500 RFCs. GitHub allows up to 1,000 assets per
# release, so 500 leaves real headroom. Zero-padded to five digits because RFC
# numbers passed 10000 in 2026 and these names have to keep sorting correctly.
BUCKET_SIZE = 500

EPUB_RE = re.compile(r"rfc(\d+)\.epub")


def bucket(number: int) -> str:
    return f"{number // BUCKET_SIZE * BUCKET_SIZE:05d}"


def now_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def load_entries(path: str) -> list[dict]:
    return json.loads(Path(path).read_text(encoding="utf-8"))["entries"]


def read_numbers(path: str | None) -> set[int]:
    """Read RFC numbers from a file of either bare numbers or rfcNNNN.epub names."""
    if not path or not Path(path).is_file():
        return set()
    found = set()
    for token in Path(path).read_text(encoding="utf-8").split():
        m = EPUB_RE.fullmatch(token.strip())
        if m:
            found.add(int(m.group(1)))
        elif token.strip().isdigit():
            found.add(int(token.strip()))
    return found


# -------------------------------------------------------------------------- index


def cmd_index(args: argparse.Namespace) -> int:
    root = ET.fromstring(read_index(args.index_xml))

    entries = []
    for entry in root.findall(NS + "rfc-entry"):
        m = re.fullmatch(r"RFC(\d+)", entry.findtext(NS + "doc-id") or "")
        if not m:
            continue
        formats = {f.text for f in entry.findall(f"{NS}format/{NS}file-format")}
        date = entry.find(NS + "date")
        month = date.findtext(NS + "month") if date is not None else None
        year = date.findtext(NS + "year") if date is not None else None
        entries.append(
            {
                "n": int(m.group(1)),
                "title": " ".join((entry.findtext(NS + "title") or "").split()),
                "status": entry.findtext(NS + "current-status") or "",
                "date": " ".join(x for x in (month, year) if x),
                "xml": "XML" in formats,
                "txt": "TXT" in formats,
            }
        )

    entries.sort(key=lambda e: e["n"], reverse=True)

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(
        json.dumps({"generated": now_iso(), "entries": entries}), encoding="utf-8"
    )

    convertible = sum(1 for e in entries if e["txt"])
    print(
        f"index: {len(entries)} RFCs, {convertible} convertible, "
        f"{len(entries) - convertible} without a text source, "
        f"{sum(1 for e in entries if e['xml'])} with XML, "
        f"highest RFC {entries[0]['n'] if entries else 0}"
    )
    return 0


def read_index(path: str | None) -> bytes:
    """Prefer the rsync'd copy; fall back to HTTP so the script works standalone."""
    if path and Path(path).is_file():
        return Path(path).read_bytes()
    req = urllib.request.Request(INDEX_URL, headers={"User-Agent": "rfc2epub-site/1.0"})
    with urllib.request.urlopen(req, timeout=120) as resp:  # noqa: S310 - fixed URL
        return resp.read()


# ------------------------------------------------------------------------ buckets


def cmd_buckets(args: argparse.Namespace) -> int:
    highest = max((e["n"] for e in load_entries(args.meta)), default=0)
    for start in range(0, highest + 1, BUCKET_SIZE):
        print(f"{start:05d}")
    return 0


# --------------------------------------------------------------------------- plan


def cmd_plan(args: argparse.Namespace) -> int:
    entries = load_entries(args.meta)
    have = set() if args.rebuild_all else read_numbers(args.existing)

    missing = [e for e in entries if e["txt"] and e["n"] not in have]
    # Already newest-first, so a --limit run always covers the newest RFCs.
    if args.limit > 0:
        missing = missing[: args.limit]

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    # Split by source format: RFCs the index says have no XML go straight to
    # `-f text`, skipping the 404 probe `-f auto` would spend on them. The rest
    # use `-f auto`, the only safe path because fetch.rs still has to reject
    # non-v3 XML and fall back to text.
    auto = [e["n"] for e in missing if e["xml"]]
    text = [e["n"] for e in missing if not e["xml"]]
    (out / "todo.auto.txt").write_text("".join(f"{n}\n" for n in auto), encoding="utf-8")
    (out / "todo.text.txt").write_text("".join(f"{n}\n" for n in text), encoding="utf-8")

    print(
        json.dumps(
            {
                "total": len(entries),
                "published": len(have),
                "todo": len(missing),
                "todo_auto": len(auto),
                "todo_text": len(text),
            }
        )
    )
    return 0


# -------------------------------------------------------------------------- place


def cmd_place(args: argparse.Namespace) -> int:
    """Group freshly converted EPUBs by release bucket and record what landed."""
    src = Path(args.out_dir)
    dest = Path(args.dest)
    moved = []
    for path in sorted(src.glob("rfc*.epub")):
        m = EPUB_RE.fullmatch(path.name)
        if not m:
            continue
        number = int(m.group(1))
        target = dest / bucket(number)
        target.mkdir(parents=True, exist_ok=True)
        shutil.move(str(path), target / path.name)
        moved.append(number)

    moved.sort(reverse=True)
    added = Path(args.added)
    added.parent.mkdir(parents=True, exist_ok=True)
    added.write_text("".join(f"{n}\n" for n in moved), encoding="utf-8")

    requested = set()
    for name in ("todo.auto.txt", "todo.text.txt"):
        todo = Path(args.todo) / name
        if todo.is_file():
            requested.update(int(x) for x in todo.read_text().split() if x.strip())
    failed = sorted(requested - set(moved))
    Path(args.failed).write_text("".join(f"{n}\n" for n in failed), encoding="utf-8")

    print(json.dumps({"placed": len(moved), "failed": len(failed)}))
    return 0


# -------------------------------------------------------------------------- build


def cmd_build(args: argparse.Namespace) -> int:
    site = Path(args.site)
    entries = load_entries(args.meta)
    # Read back from the releases after uploading, so a download link is only
    # offered for an asset that demonstrably exists.
    have = read_numbers(args.published)
    added = sorted(read_numbers(args.added), reverse=True)

    # The trailing flag distinguishes "not converted yet" from "cannot be
    # converted" (the handful of RFCs the RFC Editor only publishes as PDF), so
    # the page never reports the two as the same thing.
    rows = [
        [
            e["n"],
            e["title"],
            e["status"],
            e["date"],
            1 if e["n"] in have else 0,
            1 if e["txt"] else 0,
        ]
        for e in entries
    ]

    site.mkdir(parents=True, exist_ok=True)
    copy_web(Path(args.web), site)
    (site / ".nojekyll").write_text("", encoding="utf-8")
    (site / "rfcs.json").write_text(
        json.dumps(rows, separators=(",", ":"), ensure_ascii=False), encoding="utf-8"
    )

    meta = {
        "generated": now_iso(),
        "total": len(rows),
        "withEpub": len(have),
        "latest": max((r[0] for r in rows), default=0),
        "added": added,
        # app.js joins this with "NNNNN/rfcN.epub", so the same code path serves
        # release-hosted assets and a plain "epub/" directory on Pages.
        "epubBase": args.epub_base,
        "zipUrl": args.zip_url or previous(site, "zipUrl"),
    }
    (site / "meta.json").write_text(json.dumps(meta), encoding="utf-8")

    print(f"build: {len(rows)} rows, {len(have)} with EPUBs, {len(added)} added")
    return 0


def copy_web(web: Path, site: Path) -> None:
    for src in sorted(web.iterdir()):
        if src.is_file():
            shutil.copy2(src, site / src.name)


def previous(site: Path, key: str) -> str:
    """Carry a value over when this run had no reason to regenerate it."""
    old = site / "meta.json"
    if old.is_file():
        try:
            return json.loads(old.read_text(encoding="utf-8")).get(key, "") or ""
        except (json.JSONDecodeError, OSError):
            pass
    return ""


# --------------------------------------------------------------------------- main


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("index", help="parse rfc-index.xml into work/meta.json")
    p.add_argument("--index-xml", help="local rfc-index.xml (falls back to HTTP)")
    p.add_argument("--out", default="work/meta.json")
    p.set_defaults(func=cmd_index)

    p = sub.add_parser("buckets", help="print the release bucket names")
    p.add_argument("--meta", default="work/meta.json")
    p.set_defaults(func=cmd_buckets)

    p = sub.add_parser("plan", help="list RFCs that still need converting")
    p.add_argument("--meta", default="work/meta.json")
    p.add_argument("--existing", help="file of published rfcNNNN.epub names")
    p.add_argument("--out", default="work/todo")
    p.add_argument("--limit", type=int, default=0, help="0 = no cap")
    p.add_argument("--rebuild-all", action="store_true")
    p.set_defaults(func=cmd_plan)

    p = sub.add_parser("place", help="group converted EPUBs by release bucket")
    p.add_argument("--out-dir", default="out")
    p.add_argument("--dest", default="work/upload")
    p.add_argument("--todo", default="work/todo")
    p.add_argument("--added", default="work/added.txt")
    p.add_argument("--failed", default="work/failed.txt")
    p.set_defaults(func=cmd_place)

    p = sub.add_parser("build", help="assemble the publishable site tree")
    p.add_argument("--meta", default="work/meta.json")
    p.add_argument("--published", help="file of published rfcNNNN.epub names")
    p.add_argument("--site", default="site")
    p.add_argument("--web", default="web")
    p.add_argument("--added", default="work/added.txt")
    p.add_argument("--zip-url", default="")
    p.add_argument("--epub-base", default="epub/")
    p.set_defaults(func=cmd_build)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
