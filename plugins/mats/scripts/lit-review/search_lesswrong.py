# /// script
# requires-python = ">=3.11"
# dependencies = ["httpx", "beautifulsoup4"]
# ///
"""
Search LessWrong and Alignment Forum using Google site-search.
Then fetches full post content and comments via GraphQL API.

Usage:
    uv run search_lesswrong.py --queries queries.json --output results.json [--limit 50]
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

# GraphQL endpoints for fetching full content
LESSWRONG_GRAPHQL = "https://www.lesswrong.com/graphql"
EA_FORUM_GRAPHQL = "https://forum-bots.effectivealtruism.org/graphql"

DUCKDUCKGO_HTML_URL = "https://html.duckduckgo.com/html/"

USER_AGENTS = [
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:121.0) Gecko/20100101 Firefox/121.0",
]

POST_BY_SLUG_QUERY = """
query GetPostBySlug($slug: String!) {
  post(input: {selector: {slug: $slug}}) {
    result {
      _id
      title
      slug
      pageUrl
      postedAt
      baseScore
      voteCount
      commentsCount
      contents {
        html
      }
      user {
        username
        displayName
      }
      tags {
        name
      }
    }
  }
}
"""

COMMENTS_QUERY = """
query GetComments($postId: String!, $limit: Int!, $offset: Int!) {
  comments(input: {terms: {postId: $postId, limit: $limit, offset: $offset, view: "postCommentsOld"}}) {
    results {
      _id
      postId
      parentCommentId
      contents {
        html
      }
      baseScore
      postedAt
      user {
        username
        displayName
      }
    }
  }
}
"""


def extract_post_info_from_url(url: str) -> tuple[str | None, str | None, str]:
    """Extract post ID and slug from LW/AF URL.

    Returns: (post_id, slug, source)
    - LessWrong: supports slug selector
    - EA Forum: requires _id selector (no slug support)
    """
    # Determine source
    if "lesswrong.com" in url:
        source = "lesswrong"
    elif "effectivealtruism.org" in url or "alignmentforum.org" in url:
        source = "ea_forum"
    else:
        return None, None, ""

    # URL format: /posts/{post_id}/{slug}
    match = re.search(r'/posts/([^/]+)/([^/?#]+)', url)
    if match:
        post_id = match.group(1)
        slug = match.group(2)
        return post_id, slug, source

    # Simpler format: /posts/{post_id}
    match = re.search(r'/posts/([^/?#]+)', url)
    if match:
        return match.group(1), None, source

    return None, None, source


async def duckduckgo_search_site(
    client: httpx.AsyncClient,
    query: str,
    site: str,
    limit: int = 20,
) -> list[dict]:
    """Search DuckDuckGo for a specific site."""
    import urllib.parse

    results = []
    search_query = f"site:{site} {query}"

    await asyncio.sleep(random.uniform(1, 3))

    headers = {
        "User-Agent": random.choice(USER_AGENTS),
        "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        "Accept-Language": "en-US,en;q=0.9",
    }

    try:
        # DuckDuckGo HTML version - more scraper-friendly
        resp = await client.post(
            DUCKDUCKGO_HTML_URL,
            data={"q": search_query, "b": ""},
            headers=headers,
            timeout=30.0,
            follow_redirects=True,
        )

        if resp.status_code == 429:
            print(f"    Rate limited, waiting...", file=sys.stderr)
            await asyncio.sleep(30)
            return results

        resp.raise_for_status()

        soup = BeautifulSoup(resp.text, "html.parser")

        # DuckDuckGo HTML results are in .result class
        for result in soup.select(".result"):
            link = result.select_one(".result__a")
            if not link:
                continue

            href = link.get("href", "")

            # DuckDuckGo uses uddg parameter for actual URL
            if "uddg=" in href:
                actual_url = urllib.parse.unquote(href.split("uddg=")[1].split("&")[0])
            else:
                actual_url = href

            # Only include URLs with /posts/ (actual posts, not wiki pages)
            if site in actual_url and "/posts/" in actual_url:
                title = link.get_text(strip=True)
                results.append({
                    "url": actual_url,
                    "title": title,
                })

            if len(results) >= limit:
                break

    except Exception as e:
        print(f"    DuckDuckGo search error: {e}", file=sys.stderr)

    # Dedupe by URL
    seen = set()
    deduped = []
    for r in results:
        if r["url"] not in seen:
            seen.add(r["url"])
            deduped.append(r)

    return deduped[:limit]


async def fetch_comments(
    client: httpx.AsyncClient, graphql_url: str, post_id: str, max_comments: int = 500
) -> list[dict]:
    """Fetch all comments for a post with pagination."""
    comments = []
    offset = 0
    batch_size = 100

    while len(comments) < max_comments:
        for attempt in range(3):
            try:
                resp = await client.post(
                    graphql_url,
                    json={
                        "query": COMMENTS_QUERY,
                        "variables": {
                            "postId": post_id,
                            "limit": batch_size,
                            "offset": offset,
                        },
                    },
                    timeout=30.0,
                )
                resp.raise_for_status()
                data = resp.json()

                if "errors" in data:
                    return comments

                batch = data.get("data", {}).get("comments", {}).get("results", [])
                comments.extend(batch)

                if len(batch) < batch_size:
                    return comments
                offset += batch_size
                break
            except Exception as e:
                if attempt == 2:
                    return comments
                await asyncio.sleep(2**attempt)

    return comments


POST_BY_ID_QUERY = """
query GetPostById($id: String!) {
  post(input: {selector: {_id: $id}}) {
    result {
      _id
      title
      slug
      pageUrl
      postedAt
      baseScore
      voteCount
      commentsCount
      contents {
        html
      }
      user {
        username
        displayName
      }
      tags {
        name
      }
    }
  }
}
"""


async def fetch_post(
    client: httpx.AsyncClient, source: str, post_id: str | None, slug: str | None
) -> dict | None:
    """Fetch full post content by ID or slug depending on source."""
    graphql_url = LESSWRONG_GRAPHQL if source == "lesswrong" else EA_FORUM_GRAPHQL

    try:
        # LessWrong supports slug, EA Forum requires _id
        if source == "lesswrong" and slug:
            query = POST_BY_SLUG_QUERY
            variables = {"slug": slug}
        elif post_id:
            query = POST_BY_ID_QUERY
            variables = {"id": post_id}
        else:
            return None

        resp = await client.post(
            graphql_url,
            json={"query": query, "variables": variables},
            timeout=30.0,
        )
        resp.raise_for_status()
        data = resp.json()

        if "errors" in data:
            return None

        return data.get("data", {}).get("post", {}).get("result")
    except Exception as e:
        identifier = slug or post_id
        print(f"    Error fetching post '{identifier}': {e}", file=sys.stderr)
        return None


async def search_all_queries(
    queries: list[str], limit_per_query: int = 20
) -> list[dict]:
    """Search LessWrong and EA Forum using Google site-search."""
    all_results = []
    seen_urls = set()

    async with httpx.AsyncClient(follow_redirects=True) as client:
        for i, query in enumerate(queries):
            print(f"Searching ({i+1}/{len(queries)}): {query}", file=sys.stderr)

            # Search LessWrong via DuckDuckGo
            print("  Searching LessWrong...", file=sys.stderr)
            lw_urls = await duckduckgo_search_site(
                client, query, "lesswrong.com", limit_per_query
            )
            print(f"    Found {len(lw_urls)} URLs", file=sys.stderr)

            # Search EA Forum via DuckDuckGo
            print("  Searching EA Forum...", file=sys.stderr)
            ea_urls = await duckduckgo_search_site(
                client, query, "forum.effectivealtruism.org", limit_per_query
            )
            print(f"    Found {len(ea_urls)} URLs", file=sys.stderr)

            # Also try Alignment Forum
            af_urls = await duckduckgo_search_site(
                client, query, "alignmentforum.org", limit_per_query // 2
            )
            print(f"    Found {len(af_urls)} AF URLs", file=sys.stderr)

            # Process all found URLs
            for url_info in lw_urls + ea_urls + af_urls:
                url = url_info["url"]
                if url in seen_urls:
                    continue
                seen_urls.add(url)

                post_id, slug, source = extract_post_info_from_url(url)
                if not post_id and not slug:
                    continue

                identifier = slug or post_id
                print(f"    Fetching: {identifier[:40]}...", file=sys.stderr)
                post = await fetch_post(client, source, post_id, slug)

                if not post:
                    continue

                # Fetch comments
                comments = []
                if post.get("commentsCount", 0) > 0:
                    comments = await fetch_comments(
                        client, graphql_url, post["_id"]
                    )

                all_results.append({
                    "source": source,
                    "search_query": query,
                    "post_id": post.get("_id"),
                    "title": post.get("title"),
                    "slug": post.get("slug"),
                    "url": post.get("pageUrl") or url,
                    "posted_at": post.get("postedAt"),
                    "score": post.get("baseScore"),
                    "vote_count": post.get("voteCount"),
                    "comments_count": post.get("commentsCount"),
                    "html_content": post.get("contents", {}).get("html") if post.get("contents") else None,
                    "author": (post.get("user", {}) or {}).get("displayName")
                        or (post.get("user", {}) or {}).get("username"),
                    "tags": [t.get("name") for t in post.get("tags", []) if t],
                    "comments": [
                        {
                            "comment_id": c.get("_id"),
                            "parent_comment_id": c.get("parentCommentId"),
                            "html_content": c.get("contents", {}).get("html") if c.get("contents") else None,
                            "score": c.get("baseScore"),
                            "posted_at": c.get("postedAt"),
                            "author": (c.get("user", {}) or {}).get("displayName")
                                or (c.get("user", {}) or {}).get("username"),
                        }
                        for c in comments
                    ],
                })

                await asyncio.sleep(0.5)

            # Delay between queries
            if i < len(queries) - 1:
                await asyncio.sleep(random.uniform(3, 6))

    return all_results


def main():
    parser = argparse.ArgumentParser(
        description="Search LessWrong and EA Forum for posts using Google site-search"
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
        "--limit", type=int, default=20, help="Max results per query"
    )
    args = parser.parse_args()

    # Load queries
    with open(args.queries) as f:
        queries = json.load(f)

    if not isinstance(queries, list):
        print("Error: queries file must contain a JSON array of strings", file=sys.stderr)
        sys.exit(1)

    print(f"Searching with {len(queries)} queries...", file=sys.stderr)
    print("Using DuckDuckGo site-search for LW/AF content.", file=sys.stderr)
    print("", file=sys.stderr)

    # Run search
    results = asyncio.run(search_all_queries(queries, args.limit))

    # Save results
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with open(args.output, "w") as f:
        json.dump(results, f, indent=2)

    print(f"\nSaved {len(results)} results to {args.output}", file=sys.stderr)


if __name__ == "__main__":
    main()
