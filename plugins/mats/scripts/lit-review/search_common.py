"""CLI shared by the search scripts: --queries/--output/--limit, JSON in and out.

`search_all(queries, limit)` returns `(results, failed)` where `failed` is the
number of queries that produced nothing because the source failed. The script
exits 1 only when every query failed, so run_searches.py reports the source as
down instead of "completed successfully" with an empty file.
"""

import argparse
import json
import sys
from pathlib import Path


def run_cli(description: str, default_limit: int, search_all) -> None:
    parser = argparse.ArgumentParser(description=description)
    parser.add_argument("--queries", type=Path, required=True, help="JSON array of search queries")
    parser.add_argument("--output", type=Path, required=True, help="Output JSON file for results")
    parser.add_argument("--limit", type=int, default=default_limit, help="Max results per query")
    args = parser.parse_args()

    queries = json.loads(args.queries.read_text())
    if not isinstance(queries, list):
        sys.exit("Error: queries file must contain a JSON array of strings")

    results, failed = search_all(queries, args.limit)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(results, indent=2))
    print(f"Saved {len(results)} results to {args.output}", file=sys.stderr)
    if failed:
        print(f"{failed}/{len(queries)} queries failed", file=sys.stderr)
    if queries and failed == len(queries):
        sys.exit(1)
