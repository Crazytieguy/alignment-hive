# /// script
# requires-python = ">=3.11"
# dependencies = ["arxiv>=2.1.0"]
# ///
"""
Search arXiv API for academic papers.

Usage:
    uv run search_arxiv.py --queries queries.json --output results.json [--limit 100]
"""

import sys

import arxiv

from search_common import run_cli

DEFAULT_LIMIT_PER_QUERY = 100


def search_query(client: arxiv.Client, query: str, limit: int) -> tuple[list[dict], bool]:
    """Search arXiv for a single query. Returns (results, ok)."""
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
                    "arxiv_id": paper.get_short_id(),
                    "url": paper.entry_id,
                    "title": paper.title,
                    "abstract": paper.summary,
                    "authors": [a.name for a in paper.authors],
                    "year": paper.published.year,
                    "published": paper.published.isoformat(),
                    "updated": paper.updated.isoformat(),
                    "pdf_url": paper.pdf_url,
                    "doi": paper.doi,
                    "categories": paper.categories,
                    "primary_category": paper.primary_category,
                }
            )
    except Exception as e:
        print(f"  Error searching arXiv: {e}", file=sys.stderr)
        return results, False

    return results, True


def search_all_queries(queries: list[str], limit_per_query: int) -> tuple[list[dict], int]:
    # One client so its delay_seconds pacing applies across queries too.
    client = arxiv.Client(page_size=100, delay_seconds=3.0, num_retries=5)
    all_results = []
    failed = 0

    for i, query in enumerate(queries):
        print(f"Searching ({i+1}/{len(queries)}): {query}", file=sys.stderr)
        results, ok = search_query(client, query, limit_per_query)
        failed += not ok
        print(f"  Found {len(results)} results", file=sys.stderr)
        all_results.extend(results)

    return all_results, failed


if __name__ == "__main__":
    run_cli("Search arXiv for papers", DEFAULT_LIMIT_PER_QUERY, search_all_queries)
