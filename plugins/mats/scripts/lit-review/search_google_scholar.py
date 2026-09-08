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

import asyncio
import random
import re
import sys

import httpx
from bs4 import BeautifulSoup

from search_common import run_cli

GOOGLE_SCHOLAR_URL = "https://scholar.google.com/scholar"
DEFAULT_LIMIT_PER_QUERY = 50

USER_AGENTS = [
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:121.0) Gecko/20100101 Firefox/121.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Safari/605.1.15",
]
HEADERS = {
    "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    "Accept-Language": "en-US,en;q=0.9",
    "DNT": "1",
    "Connection": "keep-alive",
    "Upgrade-Insecure-Requests": "1",
}


class Blocked(Exception):
    """Google Scholar answered with a CAPTCHA or 503; later queries would be blocked too.
    Carries the results already collected for the current query."""

    def __init__(self, message: str, results: list[dict]):
        super().__init__(message)
        self.results = results


def parse_citation_count(citation_text: str) -> int | None:
    """Extract citation count from 'Cited by N' text."""
    match = re.search(r"Cited by (\d+)", citation_text)
    if match:
        return int(match.group(1))
    return None


async def search_query(
    client: httpx.AsyncClient, query: str, limit: int
) -> tuple[list[dict], bool]:
    """Search Google Scholar for a single query. Returns (results, ok); raises Blocked."""
    results = []
    start = 0

    while len(results) < limit:
        # Random delay to avoid detection
        await asyncio.sleep(random.uniform(3, 7))

        for attempt in range(3):
            try:
                resp = await client.get(
                    GOOGLE_SCHOLAR_URL,
                    params={"q": query, "start": start, "hl": "en"},
                    headers={"User-Agent": random.choice(USER_AGENTS), **HEADERS},
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
                    raise Blocked("Google Scholar returned 503 (possibly CAPTCHA)", results)

                resp.raise_for_status()

                soup = BeautifulSoup(resp.text, "html.parser")

                if soup.find("form", {"id": "gs_captcha_f"}):
                    raise Blocked("CAPTCHA detected", results)

                articles = soup.select(".gs_ri")

                if not articles:
                    # No more results
                    return results, True

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

            except Blocked:
                raise
            except Exception as e:
                if attempt == 2:
                    print(f"  Error after 3 attempts: {e}", file=sys.stderr)
                    return results, False
                await asyncio.sleep(10 * (attempt + 1))
        else:
            # Three 429s in a row: give up on this query instead of retrying the same page forever.
            print("  Giving up after 3 rate-limited attempts", file=sys.stderr)
            return results, False

    return results, True


async def search_all_queries_async(queries: list[str], limit_per_query: int) -> tuple[list[dict], int]:
    all_results = []
    failed = 0

    async with httpx.AsyncClient() as client:
        for i, query in enumerate(queries):
            print(f"Searching ({i+1}/{len(queries)}): {query}", file=sys.stderr)
            try:
                results, ok = await search_query(client, query, limit_per_query)
            except Blocked as e:
                print(f"  {e}. Stopping; {len(queries) - i} queries not searched.", file=sys.stderr)
                all_results.extend(e.results)
                failed += len(queries) - i
                break
            failed += not ok
            print(f"  Found {len(results)} results", file=sys.stderr)
            all_results.extend(results)

            # Longer delay between queries
            if i < len(queries) - 1:
                delay = random.uniform(10, 20)
                print(f"  Waiting {delay:.1f}s before next query...", file=sys.stderr)
                await asyncio.sleep(delay)

    return all_results, failed


def search_all_queries(queries: list[str], limit_per_query: int) -> tuple[list[dict], int]:
    print(
        "WARNING: Google Scholar scraping is fragile and may be blocked.",
        file=sys.stderr,
    )
    print("This source is 'best effort' - results may be incomplete.", file=sys.stderr)
    print("", file=sys.stderr)
    return asyncio.run(search_all_queries_async(queries, limit_per_query))


if __name__ == "__main__":
    run_cli("Search Google Scholar for papers (web scraping)", DEFAULT_LIMIT_PER_QUERY, search_all_queries)
