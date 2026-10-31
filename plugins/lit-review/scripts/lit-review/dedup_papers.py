# /// script
# requires-python = ">=3.11"
# dependencies = ["rapidfuzz"]
# ///
"""
Deduplicate papers from multiple sources using DOI and fuzzy title matching.

Usage:
    uv run dedup_papers.py --input-dir raw_results/ --output deduplicated.json [--threshold 0.85]

To merge several stages, pass --input-dir once per directory.
"""

import argparse
import json
import re
import sys
from collections import Counter
from pathlib import Path

from rapidfuzz import fuzz

ID_KEYS = ("doi", "arxiv_id", "post_id", "paperId", "title")


def normalize_title(title: str) -> str:
    """Lowercase, drop punctuation, collapse whitespace."""
    title = re.sub(r"[^\w\s]", " ", title.lower())
    return " ".join(title.split())


def deduplicate_papers(papers: list[dict], threshold: float = 0.85) -> list[dict]:
    """Deduplicate in one pass: a record is a duplicate if its DOI was seen, else if its
    normalized title fuzzy-matches a seen title at >= threshold. Duplicates are merged
    into the first record (existing non-empty values win, new keys fill gaps, id keys
    are left alone), so a paper found by both arXiv and Semantic Scholar keeps pdf_url
    and citationCount under its arXiv file stem."""
    seen_dois: dict[str, int] = {}
    seen_titles: list[tuple[str, int]] = []  # (normalized title, index)
    deduplicated: list[dict] = []
    duplicates_removed = 0

    def merge_into(existing_idx: int, paper: dict, doi: str | None, normalized: str) -> None:
        existing = deduplicated[existing_idx]
        for k, v in paper.items():
            # Never fill in the keys that paper_ids.get_paper_id reads: the file stem
            # must stay what the first record alone would have produced.
            if k not in ID_KEYS and existing.get(k) in (None, "", []):
                existing[k] = v
        # Register the duplicate's own keys too, so a third record carrying its DOI or
        # title (arXiv preprint title vs. published title) folds into the same entry.
        if doi and doi not in seen_dois:
            seen_dois[doi] = existing_idx
        if normalized:
            seen_titles.append((normalized, existing_idx))

    for paper in papers:
        doi = (paper.get("doi") or "").lower() or None
        normalized = normalize_title(paper.get("title") or "")

        if doi and doi in seen_dois:
            merge_into(seen_dois[doi], paper, doi, normalized)
            duplicates_removed += 1
            continue

        if normalized:
            duplicate_idx = None
            for seen_title, idx in seen_titles:
                # token_sort_ratio also matches reordered words
                if fuzz.token_sort_ratio(normalized, seen_title) / 100 >= threshold:
                    duplicate_idx = idx
                    break
            if duplicate_idx is not None:
                merge_into(duplicate_idx, paper, doi, normalized)
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
    """Load every result file in a directory. A malformed file is an error, not a skip."""
    all_papers = []

    for json_file in sorted(input_dir.glob("*.json")):
        if json_file.name.endswith("_urls.json"):
            continue  # URL lists are inputs to fetch_lesswrong.py, not results
        print(f"  Loading: {json_file.name}", file=sys.stderr)
        with open(json_file) as f:
            data = json.load(f)
        if not isinstance(data, list):
            sys.exit(f"Error: {json_file}: expected a JSON array of papers")
        all_papers.extend(data)
        print(f"    Found {len(data)} papers", file=sys.stderr)

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
        help="Fuzzy matching threshold (0-1, default: %(default)s)",
    )
    args = parser.parse_args()

    for input_dir in args.input_dirs:
        if not input_dir.exists():
            sys.exit(f"Error: Input directory does not exist: {input_dir}")

    print("Loading results...", file=sys.stderr)
    all_papers = []
    for input_dir in args.input_dirs:
        print(f"From {input_dir}:", file=sys.stderr)
        all_papers.extend(load_results_from_dir(input_dir))
    print(f"Total papers loaded: {len(all_papers)}", file=sys.stderr)

    print(f"Deduplicating with threshold {args.threshold}...", file=sys.stderr)
    deduplicated = deduplicate_papers(all_papers, args.threshold)

    sources = Counter(p.get("source", "unknown") for p in deduplicated)
    print("\nResults by source:", file=sys.stderr)
    for source, count in sorted(sources.items()):
        print(f"  {source}: {count}", file=sys.stderr)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with open(args.output, "w") as f:
        json.dump(deduplicated, f, indent=2)

    print(f"\nSaved {len(deduplicated)} unique papers to {args.output}", file=sys.stderr)


if __name__ == "__main__":
    main()
