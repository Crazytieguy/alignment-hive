# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Search for papers and posts using the Exa API (optional enhancement).

Exa provides high-quality semantic search results. Users need an API key
from https://exa.ai - free tier available.

Set EXA_API_KEY environment variable or create .env file with:
    EXA_API_KEY=your_key_here

Usage:
    uv run search_exa.py --queries queries.json --output results.json [--limit 50]
"""

import argparse
import json
import os
import sys
import urllib.request
import urllib.error
from pathlib import Path


def load_api_key() -> str | None:
    """Load Exa API key from environment or .env file."""
    key = os.environ.get("EXA_API_KEY")
    if key:
        return key

    # Try loading from .env files
    env_paths = [
        Path.cwd() / ".env",
        Path.home() / ".env",
        Path.home() / "projects" / ".env",
    ]

    for env_path in env_paths:
        if env_path.exists():
            with open(env_path) as f:
                for line in f:
                    line = line.strip()
                    if line.startswith("EXA_API_KEY="):
                        return line.split("=", 1)[1].strip().strip('"\'')

    return None


def exa_search(query: str, api_key: str, num_results: int = 10, domains: list[str] | None = None) -> dict:
    """Search using Exa API."""
    url = "https://api.exa.ai/search"
    headers = {
        "Content-Type": "application/json",
        "x-api-key": api_key,
    }

    data = {
        "query": query,
        "numResults": num_results,
        "contents": {
            "text": True,
        },
    }

    if domains:
        data["includeDomains"] = domains

    req = urllib.request.Request(
        url,
        data=json.dumps(data).encode("utf-8"),
        headers=headers,
        method="POST"
    )

    try:
        with urllib.request.urlopen(req, timeout=60) as response:
            return json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        error_body = e.read().decode("utf-8")
        print(f"Exa API Error {e.code}: {error_body}", file=sys.stderr)
        return {"results": [], "error": error_body}
    except Exception as e:
        print(f"Exa request failed: {e}", file=sys.stderr)
        return {"results": [], "error": str(e)}


def main():
    parser = argparse.ArgumentParser(description="Search using Exa API")
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
    parser.add_argument(
        "--ai-safety-domains",
        action="store_true",
        help="Restrict to AI safety domains (LessWrong, arXiv, etc.)"
    )
    args = parser.parse_args()

    # Load API key
    api_key = load_api_key()
    if not api_key:
        print("Error: EXA_API_KEY not found.", file=sys.stderr)
        print("Get a free API key at https://exa.ai and set it as:", file=sys.stderr)
        print("  export EXA_API_KEY=your_key_here", file=sys.stderr)
        print("Or create a .env file with: EXA_API_KEY=your_key_here", file=sys.stderr)
        sys.exit(1)

    # Load queries
    with open(args.queries) as f:
        queries = json.load(f)

    if not isinstance(queries, list):
        print("Error: queries file must contain a JSON array of strings", file=sys.stderr)
        sys.exit(1)

    # AI safety domains
    domains = None
    if args.ai_safety_domains:
        domains = [
            "lesswrong.com",
            "alignmentforum.org",
            "arxiv.org",
            "anthropic.com",
            "openai.com",
            "deepmind.com",
            "80000hours.org",
        ]

    print(f"Searching with {len(queries)} queries using Exa...", file=sys.stderr)

    all_results = []
    seen_urls = set()

    for i, query in enumerate(queries):
        print(f"  ({i+1}/{len(queries)}) {query[:50]}...", file=sys.stderr)

        response = exa_search(query, api_key, args.limit, domains)

        for result in response.get("results", []):
            url = result.get("url", "")
            if url in seen_urls:
                continue
            seen_urls.add(url)

            all_results.append({
                "url": url,
                "title": result.get("title", ""),
                "text": result.get("text", ""),
                "published_date": result.get("publishedDate"),
                "author": result.get("author"),
                "search_query": query,
                "source": "exa",
            })

    # Save results
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with open(args.output, "w") as f:
        json.dump(all_results, f, indent=2)

    print(f"\nFound {len(all_results)} unique results", file=sys.stderr)
    print(f"Saved to {args.output}", file=sys.stderr)


if __name__ == "__main__":
    main()
