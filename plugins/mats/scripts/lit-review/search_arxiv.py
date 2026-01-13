# /// script
# requires-python = ">=3.11"
# dependencies = ["arxiv>=2.1.0"]
# ///
"""
Search arXiv API for academic papers.

Usage:
    uv run search_arxiv.py --queries queries.json --output results.json [--limit 100]
"""

import argparse
import json
import sys
import time
from pathlib import Path

import arxiv

DEFAULT_LIMIT_PER_QUERY = 100


def search_query(query: str, limit: int = DEFAULT_LIMIT_PER_QUERY) -> list[dict]:
    """Search arXiv for a single query."""
    client = arxiv.Client(
        page_size=100,
        delay_seconds=3.0,  # Respectful rate limiting
        num_retries=5,
    )

    search = arxiv.Search(
        query=query,
        max_results=limit,
        sort_by=arxiv.SortCriterion.Relevance,
    )

    results = []
    try:
        for paper in client.results(search):
            results.append(
                {
                    "source": "arxiv",
                    "search_query": query,
                    "arxiv_id": paper.entry_id,
                    "title": paper.title,
                    "abstract": paper.summary,
                    "authors": [a.name for a in paper.authors],
                    "year": paper.published.year if paper.published else None,
                    "published": (
                        paper.published.isoformat() if paper.published else None
                    ),
                    "updated": paper.updated.isoformat() if paper.updated else None,
                    "pdf_url": paper.pdf_url,
                    "doi": paper.doi,
                    "categories": paper.categories,
                    "primary_category": paper.primary_category,
                }
            )
    except Exception as e:
        print(f"  Error searching arXiv: {e}", file=sys.stderr)

    return results


def search_all_queries(
    queries: list[str], limit_per_query: int = DEFAULT_LIMIT_PER_QUERY
) -> list[dict]:
    """Search all queries and combine results."""
    all_results = []

    for i, query in enumerate(queries):
        print(f"Searching ({i+1}/{len(queries)}): {query}", file=sys.stderr)
        results = search_query(query, limit_per_query)
        print(f"  Found {len(results)} results", file=sys.stderr)
        all_results.extend(results)
        # Additional delay between queries
        if i < len(queries) - 1:
            time.sleep(1)

    return all_results


def main():
    parser = argparse.ArgumentParser(description="Search arXiv for papers")
    parser.add_argument(
        "--queries",
        type=Path,
        required=True,
        help="JSON file containing list of search queries",
    )
    parser.add_argument(
        "--output", type=Path, required=True, help="Output JSON file for results"
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=DEFAULT_LIMIT_PER_QUERY,
        help="Max results per query",
    )
    args = parser.parse_args()

    # Load queries
    with open(args.queries) as f:
        queries = json.load(f)

    if not isinstance(queries, list):
        print("Error: queries file must contain a JSON array of strings", file=sys.stderr)
        sys.exit(1)

    # Run search
    results = search_all_queries(queries, args.limit)

    # Save results
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with open(args.output, "w") as f:
        json.dump(results, f, indent=2)

    print(f"Saved {len(results)} results to {args.output}", file=sys.stderr)


if __name__ == "__main__":
    main()
