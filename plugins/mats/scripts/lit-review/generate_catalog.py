# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Generate catalog.md from paper summaries, plus configs.json: each paper's
experimental configuration in the MATS dashboard's nine comparability factors.

Usage:
    uv run generate_catalog.py --summaries summaries/ --papers deduplicated.json --output catalog.md
    (configs.json is written next to the catalog; --configs overrides the path)
"""

import argparse
import json
import re
import sys
from pathlib import Path

from paper_ids import get_paper_id, legacy_forum_stem

# The comparability factors, in this order. A COPY of `auto_alignment.FACTORS`
# in MATSResearch/mats_dashboards, which owns the vocabulary: the dashboard
# files each paper's configuration as a typed item and computes, per paper,
# which factors a project's own design differs on. A rename is a commit in
# both places.
FACTORS = ("base_model", "trainer", "objective", "reward", "data", "eval",
           "decoding", "variation", "compute")
# What the summarizer writes when a paper does not state a factor. Dropped
# from configs.json so a blank stays a blank on the dashboard side.
NOT_REPORTED = {"", "not reported", "n/a", "none reported", "unknown", "-", "—"}


def parse_configuration(content: str) -> dict[str, str]:
    """The `## Configuration` list as {factor: value}, known keys only, with
    not-reported values dropped. Missing section → {}."""
    section = re.search(r"##\s+Configuration\s*\n(.+?)(?=\n##|\Z)", content, re.DOTALL)
    if not section:
        return {}
    out = {}
    for m in re.finditer(r"^-\s+\*\*([a-z_]+)\*\*\s*:\s*(.*)$", section.group(1), re.MULTILINE):
        key, value = m.group(1).strip(), " ".join(m.group(2).split())
        if key in FACTORS and value.strip("[] ").lower() not in NOT_REPORTED:
            out[key] = value[:200]
    return out


def load_summaries(summaries_dir: Path) -> dict[str, dict]:
    """Parse each summary's `# title`, `Score: N` line, `## Summary` section and
    `## Configuration` list (the template in agents/summarizer.md), keyed by
    file stem."""
    summaries = {}

    for md_file in summaries_dir.glob("*.md"):
        content = md_file.read_text(encoding="utf-8")
        title = re.search(r"^#\s+(.+)$", content, re.MULTILINE)
        score = re.search(r"Score:\s*(\d+)", content)
        short = re.search(r"##\s+Summary\s*\n+(.+?)(?=\n##|\Z)", content, re.DOTALL)
        summaries[md_file.stem] = {
            "title": title.group(1).strip() if title else None,
            "relevance_score": int(score.group(1)) if score else None,
            "short_summary": short.group(1).strip()[:300] if short else None,
            "configuration": parse_configuration(content),
        }

    return summaries


def write_configs(summaries: dict[str, dict], papers: dict[str, dict],
                  output_path: Path) -> int:
    """configs.json: one record per paper that reported at least one factor —
    {paper_id, title, url, factors: {...}, not_reported: [...]} — sorted by
    relevance like the catalog. Returns the number of records written."""
    records = []
    for paper_id, summary in summaries.items():
        factors = summary.get("configuration") or {}
        if not factors:
            continue
        paper = papers.get(paper_id, {})
        records.append({
            "paper_id": paper_id,
            "title": summary.get("title") or paper_id,
            "url": paper.get("url") or paper.get("pdf_url"),
            "relevance_score": summary.get("relevance_score"),
            "factors": {k: factors[k] for k in FACTORS if k in factors},
            "not_reported": [k for k in FACTORS if k not in factors],
            "summary": f"summaries/{paper_id}.md",
        })
    records.sort(key=lambda r: (r["relevance_score"] is not None,
                                r["relevance_score"] or 0), reverse=True)
    output_path.write_text(
        json.dumps({"factors": list(FACTORS), "papers": records}, indent=2,
                   ensure_ascii=False) + "\n", encoding="utf-8")
    return len(records)


def load_papers(papers_file: Path) -> dict[str, dict]:
    """Paper metadata keyed by the same file stem the pipeline used (forum posts
    also under the stem older pipelines gave them)."""
    with open(papers_file) as f:
        papers = json.load(f)
    lookup = {get_paper_id(p): p for p in papers}
    for p in papers:
        if p.get("post_id"):
            lookup.setdefault(legacy_forum_stem(p), p)
    return lookup


def format_authors(paper: dict) -> str:
    """Authors may be strings (arXiv) or {"name": ...} objects (Semantic Scholar);
    forum posts carry a single `author`."""
    authors = paper.get("authors") or ([paper["author"]] if paper.get("author") else [])
    names = [a.get("name", "") if isinstance(a, dict) else str(a) for a in authors]
    names = [n for n in names if n]
    return ", ".join(names[:3]) + (" et al." if len(names) > 3 else "")


def generate_catalog(
    summaries: dict[str, dict], papers: dict[str, dict], output_path: Path
) -> None:
    """Generate catalog.md with all papers sorted by relevance."""
    # Sort by relevance score (descending), None values last
    sorted_items = sorted(
        summaries.items(),
        key=lambda x: (x[1]["relevance_score"] is not None, x[1]["relevance_score"] or 0),
        reverse=True,
    )

    lines = [
        "# Literature Review Catalog",
        "",
        f"Total papers: {len(summaries)}",
        "",
        "---",
        "",
    ]

    for i, (paper_id, summary) in enumerate(sorted_items, 1):
        title = summary.get("title") or paper_id
        score = summary.get("relevance_score")
        short_summary = summary.get("short_summary") or "No summary available."

        paper = papers.get(paper_id, {})
        source = paper.get("source", "unknown")
        authors_str = format_authors(paper)
        year = paper.get("year")
        url = paper.get("url") or paper.get("pdf_url")

        lines.append(f"## {i}. {title}")
        lines.append("")
        if score is not None:
            lines.append(f"**Relevance Score:** {score}/10")
        lines.append(f"**Source:** {source}")
        if year:
            lines.append(f"**Year:** {year}")
        if authors_str:
            lines.append(f"**Authors:** {authors_str}")
        if url:
            lines.append(f"**URL:** {url}")
        lines.append("")
        lines.append(short_summary)
        lines.append("")
        config = summary.get("configuration") or {}
        if config:
            missing = [k for k in FACTORS if k not in config]
            lines.append("**Configuration:** "
                         + "; ".join(f"{k}={config[k]}" for k in FACTORS if k in config)
                         + (f"  \n*not reported:* {', '.join(missing)}" if missing else ""))
            lines.append("")
        lines.append(f"*Full summary: [{paper_id}.md](summaries/{paper_id}.md)*")
        lines.append("")
        lines.append("---")
        lines.append("")

    output_path.write_text("\n".join(lines), encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(
        description="Generate catalog from paper summaries"
    )
    parser.add_argument(
        "--summaries",
        type=Path,
        required=True,
        help="Directory containing summary markdown files",
    )
    parser.add_argument(
        "--papers",
        type=Path,
        required=True,
        help="JSON file with paper metadata (deduplicated.json)",
    )
    parser.add_argument(
        "--output", type=Path, required=True, help="Output catalog markdown file"
    )
    parser.add_argument(
        "--configs", type=Path, default=None,
        help="Output configs.json (default: next to the catalog)",
    )
    args = parser.parse_args()

    if not args.summaries.exists():
        sys.exit(f"Error: Summaries directory does not exist: {args.summaries}")
    if not args.papers.exists():
        sys.exit(f"Error: Papers file does not exist: {args.papers}")

    print("Loading summaries...", file=sys.stderr)
    summaries = load_summaries(args.summaries)
    print(f"  Loaded {len(summaries)} summaries", file=sys.stderr)

    print("Loading paper metadata...", file=sys.stderr)
    papers = load_papers(args.papers)
    print(f"  Loaded {len(papers)} paper records", file=sys.stderr)

    print("Generating catalog...", file=sys.stderr)
    generate_catalog(summaries, papers, args.output)
    print(f"Saved catalog to {args.output}", file=sys.stderr)

    configs_path = args.configs or args.output.with_name("configs.json")
    n = write_configs(summaries, papers, configs_path)
    print(f"Saved {n} paper configuration(s) to {configs_path}", file=sys.stderr)


if __name__ == "__main__":
    main()
