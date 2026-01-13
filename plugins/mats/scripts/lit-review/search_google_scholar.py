# /// script
# requires-python = ">=3.11"
# dependencies = ["httpx", "beautifulsoup4"]
# ///
"""
Search Google Scholar via web scraping.
Note: This is fragile and may break. Google Scholar has no official API.

Usage:
    uv run search_google_scholar.py --queries queries.json --output results.json [--limit 50]
"""

import argparse
import asyncio
import json
import random
import re
import sys
from pathlib import Path

import httpx
from bs4 import BeautifulSoup

GOOGLE_SCHOLAR_URL = "https://scholar.google.com/scholar"
DEFAULT_LIMIT_PER_QUERY = 50

USER_AGENTS = [
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:121.0) Gecko/20100101 Firefox/121.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Safari/605.1.15",
]


def parse_citation_count(citation_text: str) -> int | None:
    """Extract citation count from 'Cited by N' text."""
    match = re.search(r"Cited by (\d+)", citation_text)
    if match:
        return int(match.group(1))
    return None


async def search_query(
    client: httpx.AsyncClient, query: str, limit: int = DEFAULT_LIMIT_PER_QUERY
) -> list[dict]:
    """Search Google Scholar for a single query."""
    results = []
    start = 0

    while len(results) < limit:
        # Random delay to avoid detection
        await asyncio.sleep(random.uniform(3, 7))

        for attempt in range(3):
            try:
                headers = {
                    "User-Agent": random.choice(USER_AGENTS),
                    "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                    "Accept-Language": "en-US,en;q=0.9",
                    "Accept-Encoding": "gzip, deflate",
                    "DNT": "1",
                    "Connection": "keep-alive",
                    "Upgrade-Insecure-Requests": "1",
                }

                resp = await client.get(
                    GOOGLE_SCHOLAR_URL,
                    params={"q": query, "start": start, "hl": "en"},
                    headers=headers,
                    timeout=30.0,
                    follow_redirects=True,
                )

                if resp.status_code == 429:
                    wait_time = 60 * (attempt + 1)
                    print(
                        f"  Rate limited (429), waiting {wait_time}s...",
                        file=sys.stderr,
                    )
                    await asyncio.sleep(wait_time)
                    continue

                if resp.status_code == 503:
                    print(
                        "  Google Scholar returned 503 (possibly CAPTCHA). Stopping.",
                        file=sys.stderr,
                    )
                    return results

                resp.raise_for_status()

                soup = BeautifulSoup(resp.text, "html.parser")

                # Check for CAPTCHA
                if soup.find("form", {"id": "gs_captcha_f"}):
                    print(
                        "  CAPTCHA detected. Google Scholar scraping blocked.",
                        file=sys.stderr,
                    )
                    return results

                articles = soup.select(".gs_ri")

                if not articles:
                    # No more results
                    return results

                for article in articles:
                    title_elem = article.select_one(".gs_rt a")
                    if not title_elem:
                        # Skip entries without title links (citations, etc.)
                        continue

                    # Extract metadata
                    title = title_elem.get_text(strip=True)
                    url = title_elem.get("href", "")

                    # Author/publication info
                    meta_elem = article.select_one(".gs_a")
                    meta_text = meta_elem.get_text(strip=True) if meta_elem else ""

                    # Snippet/abstract
                    snippet_elem = article.select_one(".gs_rs")
                    snippet = (
                        snippet_elem.get_text(strip=True) if snippet_elem else ""
                    )

                    # Citation info
                    footer_elem = article.select_one(".gs_fl")
                    footer_text = footer_elem.get_text() if footer_elem else ""
                    citation_count = parse_citation_count(footer_text)

                    # PDF link if available
                    pdf_elem = article.select_one(".gs_or_ggsm a")
                    pdf_url = pdf_elem.get("href") if pdf_elem else None

                    results.append(
                        {
                            "source": "google_scholar",
                            "search_query": query,
                            "title": title,
                            "url": url,
                            "meta_info": meta_text,
                            "snippet": snippet,
                            "citation_count": citation_count,
                            "pdf_url": pdf_url,
                        }
                    )

                start += 10
                break

            except httpx.HTTPStatusError as e:
                if attempt == 2:
                    print(
                        f"  HTTP error after 3 attempts: {e}",
                        file=sys.stderr,
                    )
                    return results
                await asyncio.sleep(10 * (attempt + 1))
            except Exception as e:
                if attempt == 2:
                    print(f"  Error: {e}", file=sys.stderr)
                    return results
                await asyncio.sleep(5 * (attempt + 1))

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

            # Longer delay between queries
            if i < len(queries) - 1:
                delay = random.uniform(10, 20)
                print(f"  Waiting {delay:.1f}s before next query...", file=sys.stderr)
                await asyncio.sleep(delay)

    return all_results


def main():
    parser = argparse.ArgumentParser(
        description="Search Google Scholar for papers (web scraping)"
    )
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

    print(
        "WARNING: Google Scholar scraping is fragile and may be blocked.",
        file=sys.stderr,
    )
    print("This source is 'best effort' - results may be incomplete.", file=sys.stderr)
    print("", file=sys.stderr)

    # Run search
    results = asyncio.run(search_all_queries(queries, args.limit))

    # Save results
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with open(args.output, "w") as f:
        json.dump(results, f, indent=2)

    print(f"Saved {len(results)} results to {args.output}", file=sys.stderr)


if __name__ == "__main__":
    main()
