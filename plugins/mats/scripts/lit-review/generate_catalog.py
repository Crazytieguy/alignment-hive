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


def extract_relevance_score(summary_content: str) -> int | None:
    """Extract relevance score from summary content."""
    # Look for "Score: N" or "Score: N/10" pattern
    match = re.search(r"Score:\s*(\d+)(?:/10)?", summary_content)
    if match:
        return int(match.group(1))
    return None


def extract_title_from_summary(summary_content: str) -> str | None:
    """Extract title from summary content (first # heading)."""
    match = re.search(r"^#\s+(.+)$", summary_content, re.MULTILINE)
    if match:
        return match.group(1).strip()
    return None


def extract_short_summary(summary_content: str) -> str | None:
    """Extract the summary section from the content."""
    # Look for ## Summary section
    match = re.search(
        r"##\s+Summary\s*\n+(.+?)(?=\n##|\Z)", summary_content, re.DOTALL
    )
    if match:
        return match.group(1).strip()[:300]  # Limit length
    return None


def load_summaries(summaries_dir: Path) -> dict[str, dict]:
    """Load all summary files and extract metadata."""
    summaries = {}

    for md_file in summaries_dir.glob("*.md"):
        try:
            content = md_file.read_text(encoding="utf-8")
            paper_id = md_file.stem

            summaries[paper_id] = {
                "file": str(md_file),
                "title": extract_title_from_summary(content),
                "relevance_score": extract_relevance_score(content),
                "short_summary": extract_short_summary(content),
                "content": content,
            }
        except Exception as e:
            print(f"  Error loading {md_file.name}: {e}", file=sys.stderr)

    return summaries


def load_papers(papers_file: Path) -> dict[str, dict]:
    """Load paper metadata keyed by sanitized ID."""
    with open(papers_file) as f:
        papers = json.load(f)

    # Create lookup by various IDs
    lookup = {}
    for paper in papers:
        # Try to match how download_pdfs.py generates IDs
        if paper.get("doi"):
            key = paper["doi"].replace("/", "_")
            lookup[key] = paper
        if paper.get("arxiv_id"):
            arxiv_id = paper["arxiv_id"]
            if "arxiv.org" in arxiv_id:
                arxiv_id = arxiv_id.split("/")[-1]
            lookup[f"arxiv_{arxiv_id}"] = paper
        if paper.get("post_id"):
            lookup[f"lw_{paper['post_id']}"] = paper
        if paper.get("paperId"):
            lookup[f"s2_{paper['paperId']}"] = paper

    return lookup


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

        # Get additional metadata from papers if available
        paper = papers.get(paper_id, {})
        source = paper.get("source", "unknown")
        authors = paper.get("authors", [])
        year = paper.get("year")
        url = paper.get("url") or paper.get("pdf_url") or paper.get("pageUrl")

        # Format authors
        if isinstance(authors, list):
            if len(authors) > 3:
                authors_str = ", ".join(str(a) for a in authors[:3]) + " et al."
            else:
                authors_str = ", ".join(str(a) for a in authors)
        else:
            authors_str = str(authors) if authors else "Unknown"

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
        print(f"Error: Summaries directory does not exist: {args.summaries}", file=sys.stderr)
        sys.exit(1)

    if not args.papers.exists():
        print(f"Error: Papers file does not exist: {args.papers}", file=sys.stderr)
        sys.exit(1)

    # Load data
    print("Loading summaries...", file=sys.stderr)
    summaries = load_summaries(args.summaries)
    print(f"  Loaded {len(summaries)} summaries", file=sys.stderr)

    print("Loading paper metadata...", file=sys.stderr)
    papers = load_papers(args.papers)
    print(f"  Loaded {len(papers)} paper records", file=sys.stderr)

    # Generate catalog
    print("Generating catalog...", file=sys.stderr)
    generate_catalog(summaries, papers, args.output)
    print(f"Saved catalog to {args.output}", file=sys.stderr)


if __name__ == "__main__":
    main()
