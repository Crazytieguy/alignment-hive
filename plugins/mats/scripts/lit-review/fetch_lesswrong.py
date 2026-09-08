# /// script
# requires-python = ">=3.11"
# dependencies = ["httpx"]
# ///
"""
Fetch full content from LessWrong/Alignment Forum URLs via GraphQL API.

All requests go through lesswrong.com - AF posts are a subset of LW's database,
so the LW endpoint serves both. Posts cross-posted to both platforms share the
same post ID; comments from either platform are returned together.

Usage:
    uv run fetch_lesswrong.py --urls urls.json --output results.json
"""

import argparse
import asyncio
import json
import re
import sys
from pathlib import Path

import httpx

GRAPHQL_URL = "https://www.lesswrong.com/graphql"

USER_AGENT = (
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
)

POST_QUERY = """
query GetPost($id: String!) {
  post(input: {selector: {_id: $id}}) {
    result {
      _id
      title
      pageUrl
      postedAt
      baseScore
      commentCount
      contents {
        html
      }
      user {
        username
        displayName
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


def extract_post_id_from_url(url: str) -> str | None:
    """Extract post ID from a LessWrong or Alignment Forum URL.

    URL formats:
        /posts/{post_id}/{slug}
        /posts/{post_id}
        /s/{sequence_id}/p/{post_id}
    """
    match = re.search(r"/(?:posts|p)/([A-Za-z0-9]+)", url)
    return match.group(1) if match else None


def detect_source(url: str) -> str:
    """Detect whether a URL is from LessWrong or Alignment Forum."""
    if "alignmentforum.org" in url:
        return "alignment_forum"
    return "lesswrong"


async def graphql(client: httpx.AsyncClient, query: str, variables: dict) -> dict | None:
    """One GraphQL call with three attempts; returns the `data` object or None."""
    for attempt in range(3):
        try:
            resp = await client.post(
                GRAPHQL_URL, json={"query": query, "variables": variables}, timeout=30.0
            )
            resp.raise_for_status()
            data = resp.json()
            if "errors" in data:
                msg = data["errors"][0].get("message", "unknown error")
                print(f"  GraphQL error: {msg}", file=sys.stderr)
                return None
            return data.get("data") or {}
        except Exception as e:
            fatal = isinstance(e, httpx.HTTPStatusError) and e.response.status_code == 400
            if fatal or attempt == 2:
                print(f"  GraphQL request failed: {e}", file=sys.stderr)
                return None
            await asyncio.sleep(2**attempt)
    return None


async def fetch_post_graphql(client: httpx.AsyncClient, post_id: str) -> dict | None:
    data = await graphql(client, POST_QUERY, {"id": post_id})
    return ((data or {}).get("post") or {}).get("result")


async def fetch_comments(
    client: httpx.AsyncClient, post_id: str, max_comments: int = 500
) -> list[dict]:
    """Fetch comments for a post in pages of 100; keeps what it has on failure."""
    comments: list[dict] = []
    while len(comments) < max_comments:
        data = await graphql(
            client, COMMENTS_QUERY, {"postId": post_id, "limit": 100, "offset": len(comments)}
        )
        if data is None:
            break
        batch = (data.get("comments") or {}).get("results", [])
        comments.extend(batch)
        if len(batch) < 100:
            break
    return comments


def format_post_result(post: dict, source: str, url: str, comments: list[dict]) -> dict:
    """Format a post result into the standard output format, with comments nested
    under their parents' `replies` so the converter can render threads."""
    by_id = {
        c["_id"]: {
            "comment_id": c["_id"],
            "html_content": (c.get("contents") or {}).get("html"),
            "score": c.get("baseScore"),
            "posted_at": c.get("postedAt"),
            "author": (c.get("user") or {}).get("displayName")
                or (c.get("user") or {}).get("username"),
            "replies": [],
        }
        for c in comments
    }
    roots = []
    for c in comments:
        parent = by_id.get(c.get("parentCommentId"))
        (parent["replies"] if parent else roots).append(by_id[c["_id"]])

    posted_at = post.get("postedAt")
    author = (post.get("user") or {}).get("displayName") or (post.get("user") or {}).get("username")
    return {
        "source": source,
        "post_id": post.get("_id"),
        "title": post.get("title"),
        "url": post.get("pageUrl") or url,
        "posted_at": posted_at,
        "year": int(posted_at[:4]) if posted_at else None,
        "score": post.get("baseScore"),
        "comment_count": post.get("commentCount"),
        "html_content": (post.get("contents") or {}).get("html"),
        "author": author,
        "authors": [author] if author else [],
        "comments": roots,
    }


async def fetch_all_posts(urls: list[dict]) -> list[dict]:
    """Fetch full content for all URLs; each post id is fetched once."""
    results = []
    seen: set[str] = set()

    headers = {"User-Agent": USER_AGENT}
    async with httpx.AsyncClient(headers=headers, follow_redirects=True) as client:
        for i, url_info in enumerate(urls):
            if isinstance(url_info, str):
                url_info = {"url": url_info}
            url = url_info["url"]
            title = url_info.get("title", "")

            print(f"Fetching ({i+1}/{len(urls)}): {title[:60] or url[:60]}...", file=sys.stderr)

            post_id = extract_post_id_from_url(url)
            source = detect_source(url)

            if not post_id:
                print(f"  Skipping - can't extract post ID from URL: {url}", file=sys.stderr)
                continue
            if post_id in seen:
                print(f"  Skipping - already fetched post {post_id}", file=sys.stderr)
                continue
            seen.add(post_id)

            post = await fetch_post_graphql(client, post_id)

            if not post:
                print(f"  Post not found: {url}", file=sys.stderr)
                continue

            comments = []
            comment_count = post.get("commentCount") or 0
            if comment_count > 0:
                print(f"  Fetching {comment_count} comments...", file=sys.stderr)
                comments = await fetch_comments(client, post["_id"])
                print(f"  Got {len(comments)} comments", file=sys.stderr)

            results.append(format_post_result(post, source, url, comments))

            await asyncio.sleep(0.3)

    return results


def main():
    parser = argparse.ArgumentParser(
        description="Fetch full content from LessWrong/AF URLs"
    )
    parser.add_argument(
        "--urls",
        type=Path,
        required=True,
        help="JSON file containing list of URLs or {url, title} objects",
    )
    parser.add_argument(
        "--output", type=Path, required=True, help="Output JSON file for results"
    )
    args = parser.parse_args()

    with open(args.urls) as f:
        urls = json.load(f)

    if not isinstance(urls, list):
        print("Error: urls file must contain a JSON array", file=sys.stderr)
        sys.exit(1)

    print(f"Fetching content for {len(urls)} URLs...", file=sys.stderr)

    results = asyncio.run(fetch_all_posts(urls))
    if urls and not results:
        sys.exit("Error: no posts could be fetched")

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with open(args.output, "w") as f:
        json.dump(results, f, indent=2)

    total_comments = sum(len(r.get("comments", [])) for r in results)
    print(f"\nSaved {len(results)} posts to {args.output}", file=sys.stderr)
    print(f"  Total comments collected: {total_comments}", file=sys.stderr)


if __name__ == "__main__":
    main()
