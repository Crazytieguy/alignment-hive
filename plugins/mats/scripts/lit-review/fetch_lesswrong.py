# /// script
# requires-python = ">=3.11"
# dependencies = ["httpx"]
# ///
"""
Fetch full content from LessWrong/Alignment Forum URLs via GraphQL API.

This script takes a JSON file of URLs (output from search phase) and fetches
the full post content and comments for each.

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

LESSWRONG_GRAPHQL = "https://www.lesswrong.com/graphql"
EA_FORUM_GRAPHQL = "https://forum-bots.effectivealtruism.org/graphql"

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
      commentCount
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
      commentCount
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
    """
    if "lesswrong.com" in url:
        source = "lesswrong"
    elif "effectivealtruism.org" in url or "alignmentforum.org" in url:
        source = "ea_forum"
    else:
        return None, None, ""

    # URL format: /posts/{post_id}/{slug}
    match = re.search(r'/posts/([^/]+)/([^/?#]+)', url)
    if match:
        return match.group(1), match.group(2), source

    # Simpler format: /posts/{post_id}
    match = re.search(r'/posts/([^/?#]+)', url)
    if match:
        return match.group(1), None, source

    return None, None, source


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


async def fetch_post(
    client: httpx.AsyncClient, source: str, post_id: str | None, slug: str | None
) -> dict | None:
    """Fetch full post content by ID or slug."""
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
        print(f"  Error fetching post '{identifier}': {e}", file=sys.stderr)
        return None


async def fetch_all_posts(urls: list[dict]) -> list[dict]:
    """Fetch full content for all URLs."""
    results = []

    async with httpx.AsyncClient(follow_redirects=True) as client:
        for i, url_info in enumerate(urls):
            url = url_info.get("url", url_info) if isinstance(url_info, dict) else url_info
            title = url_info.get("title", "") if isinstance(url_info, dict) else ""

            print(f"Fetching ({i+1}/{len(urls)}): {title[:50] or url[:50]}...", file=sys.stderr)

            post_id, slug, source = extract_post_info_from_url(url)
            if not post_id and not slug:
                print(f"  Skipping - not a valid LW/AF post URL", file=sys.stderr)
                continue

            post = await fetch_post(client, source, post_id, slug)
            if not post:
                print(f"  Failed to fetch post", file=sys.stderr)
                continue

            # Determine GraphQL URL for comments
            graphql_url = LESSWRONG_GRAPHQL if source == "lesswrong" else EA_FORUM_GRAPHQL

            # Fetch comments
            comments = []
            comment_count = post.get("commentCount", 0)
            if comment_count > 0:
                print(f"  Fetching {comment_count} comments...", file=sys.stderr)
                comments = await fetch_comments(client, graphql_url, post["_id"])

            results.append({
                "source": source,
                "post_id": post.get("_id"),
                "title": post.get("title"),
                "slug": post.get("slug"),
                "url": post.get("pageUrl") or url,
                "posted_at": post.get("postedAt"),
                "score": post.get("baseScore"),
                "vote_count": post.get("voteCount"),
                "comment_count": post.get("commentCount"),
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

            # Small delay between requests
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

    # Load URLs
    with open(args.urls) as f:
        urls = json.load(f)

    if not isinstance(urls, list):
        print("Error: urls file must contain a JSON array", file=sys.stderr)
        sys.exit(1)

    print(f"Fetching content for {len(urls)} URLs...", file=sys.stderr)

    # Fetch all posts
    results = asyncio.run(fetch_all_posts(urls))

    # Save results
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with open(args.output, "w") as f:
        json.dump(results, f, indent=2)

    print(f"\nSaved {len(results)} results to {args.output}", file=sys.stderr)


if __name__ == "__main__":
    main()
