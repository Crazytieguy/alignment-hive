# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Generate catalog.md from paper summaries, plus configs.json next to it: each
paper's experimental configuration in nine fixed factors.

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

# The factors a paper's configuration is recorded in, in this order. Tools read
# configs.json by these keys, so renaming one breaks them.
FACTORS = ("base_model", "trainer", "objective", "reward", "data", "eval",
           "decoding", "variation", "compute")
# How a factor the paper does not state gets written: the template's `not
# reported` and `n/a`, plus the wordings a small model drifts to. Dropped from
# configs.json, which lists those factors under `not_reported` instead.
NOT_REPORTED = {"", "not reported", "not stated", "not specified", "n/a",
                "none reported", "unknown", "-", "—"}


def section(content: str, heading: str) -> str | None:
    """The body of the `## <heading>` section, or None if there isn't one."""
    match = re.search(rf"##\s+{heading}\s*\n+(.+?)(?=\n##|\Z)", content, re.DOTALL)
    return match.group(1).strip() if match else None


def parse_configuration(content: str) -> dict[str, str]:
    """The `## Configuration` list as {factor: value} in FACTORS order, known keys
    only, with not-reported values dropped. Missing section → {}. Accepts the
    colon inside or outside the bold, or no bold at all."""
    out = {}
    # The lookahead keeps an empty bullet from taking the next bullet as its value.
    pattern = r"^[-*]\s+(\*\*)?([a-z_]+)(\*\*)?\s*:(?!\s*\n[-*]\s)\s*(.*)$"
    body = section(content, "Configuration") or ""
    for m in re.finditer(pattern, body, re.MULTILINE | re.IGNORECASE):
        opened, key, closed, raw = m.groups()
        value = " ".join(raw.split())
        if opened and not closed:  # `**key:** value` or `**key: value**`
            value = value.removeprefix("**").removesuffix("**").strip()
        key = key.lower()
        if key in FACTORS and value.strip("[]. ").lower() not in NOT_REPORTED:
            out[key] = value[:200]
    return {k: out[k] for k in FACTORS if k in out}


def load_summaries(summaries_dir: Path) -> dict[str, dict]:
    """Parse each summary's `# title`, `## Summary` section, `Score: N` in its
    `## Relevance to Proposal` section and `## Configuration` list (the template
    in agents/summarizer.md), keyed by file stem, highest relevance first and
    unscored last."""
    summaries = {}

    for md_file in summaries_dir.glob("*.md"):
        content = md_file.read_text(encoding="utf-8")
        title = re.search(r"^#\s+(.+)$", content, re.MULTILINE)
        # Its own section first: findings and eval lines can quote other scores.
        score = (re.search(r"Score:\s*(\d+)", section(content, r"Relevance[^\n]*") or "")
                 or re.search(r"Score:\s*(\d+)", content))
        short = section(content, "Summary")
        summaries[md_file.stem] = {
            "title": title.group(1).strip() if title else None,
            "relevance_score": int(score.group(1)) if score else None,
            "short_summary": short[:300] if short else None,
            "configuration": parse_configuration(content),
        }

    return dict(sorted(
        summaries.items(),
        key=lambda x: (x[1]["relevance_score"] is not None, x[1]["relevance_score"] or 0),
        reverse=True,
    ))


def write_configs(summaries: dict[str, dict], papers: dict[str, dict],
                  output_path: Path) -> int:
    """configs.json: one record per paper that reported at least one factor, in
    catalog order. Returns the number of records written."""
    records = []
    for paper_id, summary in summaries.items():
        factors = summary["configuration"]
        if not factors:
            continue
        paper = papers.get(paper_id, {})
        records.append({
            "paper_id": paper_id,
            "title": summary["title"] or paper_id,
            "url": paper.get("url") or paper.get("pdf_url"),
            "relevance_score": summary["relevance_score"],
            "factors": factors,
            "not_reported": [k for k in FACTORS if k not in factors],
            "summary": f"summaries/{paper_id}.md",
        })
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
    """Generate catalog.md with all papers, in the order load_summaries gives."""
    lines = [
        "# Literature Review Catalog",
        "",
        f"Total papers: {len(summaries)}",
        "",
        "---",
        "",
    ]

    for i, (paper_id, summary) in enumerate(summaries.items(), 1):
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
        config = summary["configuration"]
        if config:
            missing = [k for k in FACTORS if k not in config]
            lines.append("**Configuration:** " + "; ".join(f"{k}={v}" for k, v in config.items())
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
