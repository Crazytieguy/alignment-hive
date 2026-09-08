# /// script
# requires-python = ">=3.11"
# dependencies = ["httpx"]
# ///
"""
Search Semantic Scholar API for academic papers.

Usage:
    uv run search_semantic_scholar.py --queries queries.json --output results.json [--limit 100]
"""

import asyncio
import sys

import httpx

from search_common import run_cli

SEMANTIC_SCHOLAR_API = "https://api.semanticscholar.org/graph/v1/paper/search"
FIELDS = "paperId,externalIds,title,abstract,authors,year,citationCount,openAccessPdf,url"
DEFAULT_LIMIT_PER_QUERY = 100


async def search_query(
    client: httpx.AsyncClient, query: str, limit: int
) -> tuple[list[dict], bool]:
    """Search Semantic Scholar for a single query with retry logic. Returns (results, ok)."""
    results = []
    offset = 0

    while len(results) < limit:
        for attempt in range(5):
            try:
                resp = await client.get(
                    SEMANTIC_SCHOLAR_API,
                    params={
                        "query": query,
                        "fields": FIELDS,
                        "offset": offset,
                        "limit": min(100, limit - len(results)),
                    },
                    timeout=30.0,
                )
                if resp.status_code == 429:
                    wait_time = 2**attempt
                    print(f"  Rate limited, waiting {wait_time}s...", file=sys.stderr)
                    await asyncio.sleep(wait_time)
                    continue
                resp.raise_for_status()
                data = resp.json()

                batch = data.get("data", [])
                for paper in batch:
                    paper["source"] = "semantic_scholar"
                    paper["search_query"] = query
                    # Extract DOI from externalIds
                    if paper.get("externalIds"):
                        paper["doi"] = paper["externalIds"].get("DOI")
                        paper["arxiv_id"] = paper["externalIds"].get("ArXiv")
                results.extend(batch)

                if not data.get("next"):
                    return results, True
                offset = data["next"]
                break
            except Exception as e:
                if attempt == 4:
                    print(f"  Failed after 5 attempts: {e}", file=sys.stderr)
                    return results, False
                await asyncio.sleep(2**attempt)
        else:
            # Five 429s in a row: give up on this query instead of retrying the same offset forever.
            print("  Giving up after 5 rate-limited attempts", file=sys.stderr)
            return results, False

    return results, True


async def search_all_queries_async(queries: list[str], limit_per_query: int) -> tuple[list[dict], int]:
    all_results = []
    failed = 0

    async with httpx.AsyncClient() as client:
        for i, query in enumerate(queries):
            print(f"Searching ({i+1}/{len(queries)}): {query}", file=sys.stderr)
            results, ok = await search_query(client, query, limit_per_query)
            failed += not ok
            print(f"  Found {len(results)} results", file=sys.stderr)
            all_results.extend(results)
            # Small delay between queries to be respectful
            if i < len(queries) - 1:
                await asyncio.sleep(1)

    return all_results, failed


def search_all_queries(queries: list[str], limit_per_query: int) -> tuple[list[dict], int]:
    return asyncio.run(search_all_queries_async(queries, limit_per_query))


if __name__ == "__main__":
    run_cli("Search Semantic Scholar for papers", DEFAULT_LIMIT_PER_QUERY, search_all_queries)
