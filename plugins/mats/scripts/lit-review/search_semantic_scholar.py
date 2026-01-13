# /// script
# requires-python = ">=3.11"
# dependencies = ["httpx"]
# ///
"""
Search Semantic Scholar API for academic papers.

Usage:
    uv run search_semantic_scholar.py --queries queries.json --output results.json [--limit 100]
"""

import argparse
import asyncio
import json
import sys
from pathlib import Path

import httpx

SEMANTIC_SCHOLAR_API = "https://api.semanticscholar.org/graph/v1/paper/search"
FIELDS = "paperId,externalIds,title,abstract,authors,year,citationCount,openAccessPdf,url"
DEFAULT_LIMIT_PER_QUERY = 100


async def search_query(
    client: httpx.AsyncClient, query: str, limit: int = DEFAULT_LIMIT_PER_QUERY
) -> list[dict]:
    """Search Semantic Scholar for a single query with retry logic."""
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
                    return results
                offset = data["next"]
                break
            except httpx.HTTPStatusError as e:
                if attempt == 4:
                    print(
                        f"  Failed after 5 attempts: {e}",
                        file=sys.stderr,
                    )
                    return results
                await asyncio.sleep(2**attempt)
            except Exception as e:
                if attempt == 4:
                    print(f"  Error: {e}", file=sys.stderr)
                    return results
                await asyncio.sleep(2**attempt)

    return results


async def search_all_queries(
    queries: list[str], limit_per_query: int = DEFAULT_LIMIT_PER_QUERY
) -> list[dict]:
    """Search all queries and combine results."""
    all_results = []

    async with httpx.AsyncClient() as client:
        for i, query in enumerate(queries):
            print(f"Searching ({i+1}/{len(queries)}): {query}", file=sys.stderr)
            results = await search_query(client, query, limit_per_query)
            print(f"  Found {len(results)} results", file=sys.stderr)
            all_results.extend(results)
            # Small delay between queries to be respectful
            if i < len(queries) - 1:
                await asyncio.sleep(1)

    return all_results


def main():
    parser = argparse.ArgumentParser(description="Search Semantic Scholar for papers")
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
    results = asyncio.run(search_all_queries(queries, args.limit))

    # Save results
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with open(args.output, "w") as f:
        json.dump(results, f, indent=2)

    print(f"Saved {len(results)} results to {args.output}", file=sys.stderr)


if __name__ == "__main__":
    main()
