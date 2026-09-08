# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Generate catalog.md from paper summaries.

Usage:
    uv run generate_catalog.py --summaries summaries/ --papers deduplicated.json --output catalog.md
"""

import argparse
import json
import re
import sys
from pathlib import Path

from paper_ids import get_paper_id, legacy_forum_stem


def load_summaries(summaries_dir: Path) -> dict[str, dict]:
    """Parse each summary's `# title`, `Score: N` line and `## Summary` section (the
    template in agents/summarizer.md), keyed by file stem."""
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
        }

    return summaries


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


if __name__ == "__main__":
    main()
