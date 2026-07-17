# /// script
# requires-python = ">=3.11"
# dependencies = ["rapidfuzz"]
# ///
"""
Deduplicate papers from multiple sources using DOI and fuzzy title matching.

Usage:
    uv run dedup_papers.py --input-dir raw_results/ --output deduplicated.json [--threshold 0.85]

For two-stage lit review (merge multiple directories):
    uv run dedup_papers.py --input-dir raw_results/ --input-dir raw_results_stage2/ \
        --output deduplicated_merged.json --threshold 0.85
"""

import argparse
import json
import re
import sys
from pathlib import Path

from rapidfuzz import fuzz


def normalize_title(title: str) -> str:
    """Normalize title for comparison."""
    if not title:
        return ""
    # Lowercase, remove extra whitespace, remove punctuation
    title = title.lower()
    title = re.sub(r"[^\w\s]", " ", title)
    title = " ".join(title.split())
    return title


def get_doi(paper: dict) -> str | None:
    """Extract DOI from paper metadata."""
    # Try various fields where DOI might be stored
    if paper.get("doi"):
        return paper["doi"].lower()
    if paper.get("externalIds") and paper["externalIds"].get("DOI"):
        return paper["externalIds"]["DOI"].lower()
    return None


def get_title(paper: dict) -> str:
    """Extract title from paper metadata."""
    return paper.get("title", "")


def deduplicate_papers(papers: list[dict], threshold: float = 0.85) -> list[dict]:
    """
    Deduplicate papers using DOI matching and fuzzy title matching.

    Strategy:
    1. First pass: exact DOI matching
    2. Second pass: fuzzy title matching for papers without DOI or unmatched DOIs
    """
    # Map dedup keys to their index in `deduplicated` so that when a
    # duplicate is found, we can keep whichever record is richer (more
    # fields) — e.g. a full lesswrong.json entry with html_content beats
    # the bare {url, title} stub from lesswrong_urls.json.
    seen_dois: dict[str, int] = {}
    seen_titles: list[tuple[str, int]] = []  # (normalized title, index)
    deduplicated: list[dict] = []
    duplicates_removed = 0

    def keep_richer(existing_idx: int, paper: dict) -> None:
        if len(paper) > len(deduplicated[existing_idx]):
            deduplicated[existing_idx] = paper

    for paper in papers:
        # Check DOI (exact match)
        doi = get_doi(paper)
        if doi and doi in seen_dois:
            keep_richer(seen_dois[doi], paper)
            duplicates_removed += 1
            continue

        # Check title (fuzzy match)
        title = get_title(paper)
        normalized = normalize_title(title) if title else ""
        if normalized:
            duplicate_idx = None
            for seen_title, idx in seen_titles:
                # Use token_sort_ratio for better matching of reordered words
                similarity = fuzz.token_sort_ratio(normalized, seen_title) / 100
                if similarity >= threshold:
                    duplicate_idx = idx
                    break

            if duplicate_idx is not None:
                keep_richer(duplicate_idx, paper)
                duplicates_removed += 1
                continue

        index = len(deduplicated)
        if doi:
            seen_dois[doi] = index
        if normalized:
            seen_titles.append((normalized, index))
        deduplicated.append(paper)

    print(f"  Removed {duplicates_removed} duplicates", file=sys.stderr)
    return deduplicated


def load_results_from_dir(input_dir: Path) -> list[dict]:
    """Load all JSON result files from a directory."""
    all_papers = []

    for json_file in input_dir.glob("*.json"):
        print(f"  Loading: {json_file.name}", file=sys.stderr)
        try:
            with open(json_file) as f:
                data = json.load(f)
                if isinstance(data, list):
                    all_papers.extend(data)
                    print(f"    Found {len(data)} papers", file=sys.stderr)
                else:
                    print(f"    Skipped (not a list)", file=sys.stderr)
        except Exception as e:
            print(f"    Error loading: {e}", file=sys.stderr)

    return all_papers


def main():
    parser = argparse.ArgumentParser(
        description="Deduplicate papers from multiple sources"
    )
    parser.add_argument(
        "--input-dir",
        type=Path,
        action="append",
        required=True,
        dest="input_dirs",
        help="Directory containing JSON result files (can specify multiple times)",
    )
    parser.add_argument(
        "--output", type=Path, required=True, help="Output JSON file for deduplicated results"
    )
    parser.add_argument(
        "--threshold",
        type=float,
        default=0.85,
        help="Fuzzy matching threshold (0-1, default: 0.85)",
    )
    args = parser.parse_args()

    # Verify all input directories exist
    for input_dir in args.input_dirs:
        if not input_dir.exists():
            print(f"Error: Input directory does not exist: {input_dir}", file=sys.stderr)
            sys.exit(1)

    # Load all results from all directories
    print("Loading results...", file=sys.stderr)
    all_papers = []
    for input_dir in args.input_dirs:
        print(f"From {input_dir}:", file=sys.stderr)
        papers = load_results_from_dir(input_dir)
        all_papers.extend(papers)
    print(f"Total papers loaded: {len(all_papers)}", file=sys.stderr)

    # Deduplicate
    print(f"Deduplicating with threshold {args.threshold}...", file=sys.stderr)
    deduplicated = deduplicate_papers(all_papers, args.threshold)

    # Count by source
    sources = {}
    for paper in deduplicated:
        source = paper.get("source", "unknown")
        sources[source] = sources.get(source, 0) + 1

    print("", file=sys.stderr)
    print("Results by source:", file=sys.stderr)
    for source, count in sorted(sources.items()):
        print(f"  {source}: {count}", file=sys.stderr)

    # Save output
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with open(args.output, "w") as f:
        json.dump(deduplicated, f, indent=2)

    print("", file=sys.stderr)
    print(f"Saved {len(deduplicated)} unique papers to {args.output}", file=sys.stderr)


if __name__ == "__main__":
    main()
