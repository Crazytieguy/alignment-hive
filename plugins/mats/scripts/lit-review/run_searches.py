# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Run all search scripts in parallel and collect results.

This script orchestrates the parallel execution of search scripts and
aggregates their exit codes and outputs.
"""

import argparse
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path


def run_search(script_path: Path, queries_file: Path, output_file: Path, limit: int) -> tuple[str, int, str]:
    """Run a single search script and return (name, exit_code, stderr)."""
    name = script_path.stem
    cmd = [
        "uv", "run", str(script_path),
        "--queries", str(queries_file),
        "--output", str(output_file),
        "--limit", str(limit),
    ]
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
        return name, result.returncode, result.stderr
    except subprocess.TimeoutExpired:
        return name, -1, "Timeout after 300 seconds"
    except Exception as e:
        return name, -1, str(e)


def main():
    parser = argparse.ArgumentParser(description="Run all searches in parallel")
    parser.add_argument("--queries", required=True, help="Path to search_terms.json")
    parser.add_argument("--output-dir", required=True, help="Directory for raw results")
    parser.add_argument("--scripts-dir", required=True, help="Directory containing search scripts")
    parser.add_argument("--arxiv-limit", type=int, default=100)
    parser.add_argument("--semantic-scholar-limit", type=int, default=100)
    parser.add_argument("--google-scholar-limit", type=int, default=50)
    args = parser.parse_args()

    queries_file = Path(args.queries)
    output_dir = Path(args.output_dir)
    scripts_dir = Path(args.scripts_dir)

    output_dir.mkdir(parents=True, exist_ok=True)

    # Note: LessWrong/AF is handled separately via WebSearch + fetch_lesswrong.py
    searches = [
        (scripts_dir / "search_semantic_scholar.py", output_dir / "semantic_scholar.json", args.semantic_scholar_limit),
        (scripts_dir / "search_arxiv.py", output_dir / "arxiv.json", args.arxiv_limit),
        (scripts_dir / "search_google_scholar.py", output_dir / "google_scholar.json", args.google_scholar_limit),
    ]

    results = []
    with ThreadPoolExecutor(max_workers=4) as executor:
        futures = {
            executor.submit(run_search, script, queries_file, output, limit): script.stem
            for script, output, limit in searches
        }
        for future in as_completed(futures):
            name, exit_code, stderr = future.result()
            results.append((name, exit_code, stderr))
            if exit_code == 0:
                print(f"✓ {name} completed successfully")
            else:
                print(f"✗ {name} failed (exit code {exit_code})")
                if stderr:
                    print(f"  Error: {stderr[:200]}")

    success_count = sum(1 for _, code, _ in results if code == 0)
    print(f"\nCompleted: {success_count}/{len(searches)} searches succeeded")

    # Exit with error only if ALL searches failed
    if success_count == 0:
        sys.exit(1)
    sys.exit(0)


if __name__ == "__main__":
    main()
