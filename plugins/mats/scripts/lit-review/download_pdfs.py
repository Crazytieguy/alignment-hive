# /// script
# requires-python = ">=3.11"
# dependencies = ["httpx", "aiofiles"]
# ///
"""
Download PDFs from paper metadata.

Usage:
    uv run download_pdfs.py --input deduplicated.json --output-dir papers/ [--max-concurrent 5]
"""

import argparse
import asyncio
import json
import re
import sys
from pathlib import Path

import aiofiles
import httpx

MAX_CONCURRENT_DOWNLOADS = 5
MAX_RETRIES = 5
TIMEOUT_SECONDS = 120


def sanitize_filename(name: str) -> str:
    """Create a safe filename from a string."""
    # Remove or replace problematic characters
    name = re.sub(r'[<>:"/\\|?*]', "", name)
    name = re.sub(r"\s+", "_", name)
    name = name[:100]  # Limit length
    return name


def get_paper_id(paper: dict) -> str:
    """Generate a unique ID for a paper."""
    if paper.get("doi"):
        return sanitize_filename(paper["doi"].replace("/", "_"))
    if paper.get("arxiv_id"):
        arxiv_id = paper["arxiv_id"]
        if "arxiv.org" in arxiv_id:
            arxiv_id = arxiv_id.split("/")[-1]
        return sanitize_filename(f"arxiv_{arxiv_id}")
    if paper.get("post_id"):
        return sanitize_filename(f"lw_{paper['post_id']}")
    if paper.get("paperId"):
        return sanitize_filename(f"s2_{paper['paperId']}")
    # Fallback to title hash
    title = paper.get("title", "unknown")
    return sanitize_filename(title[:50])


def get_pdf_url(paper: dict) -> str | None:
    """Extract PDF URL from paper metadata."""
    # Check various fields where PDF URL might be stored
    if paper.get("pdf_url"):
        return paper["pdf_url"]
    if paper.get("openAccessPdf"):
        oa = paper["openAccessPdf"]
        if isinstance(oa, dict):
            return oa.get("url")
        return oa
    return None


async def download_pdf(
    client: httpx.AsyncClient,
    url: str,
    output_path: Path,
    paper_id: str,
) -> bool:
    """Download a single PDF with retry logic."""
    for attempt in range(MAX_RETRIES):
        try:
            resp = await client.get(
                url,
                follow_redirects=True,
                timeout=TIMEOUT_SECONDS,
            )
            resp.raise_for_status()

            # Check if we got a PDF
            content_type = resp.headers.get("content-type", "")
            if "pdf" not in content_type.lower() and not url.endswith(".pdf"):
                # Might be HTML (paywall, etc.)
                if "html" in content_type.lower():
                    return False

            async with aiofiles.open(output_path, "wb") as f:
                await f.write(resp.content)

            return True

        except httpx.HTTPStatusError as e:
            if e.response.status_code in (403, 404, 451):
                # Permanent failures
                return False
            if attempt < MAX_RETRIES - 1:
                await asyncio.sleep(2**attempt)
        except Exception as e:
            if attempt < MAX_RETRIES - 1:
                await asyncio.sleep(2**attempt)
            else:
                print(f"    Failed to download {paper_id}: {e}", file=sys.stderr)

    return False


async def download_all(
    papers: list[dict],
    output_dir: Path,
    max_concurrent: int = MAX_CONCURRENT_DOWNLOADS,
) -> dict:
    """Download PDFs for all papers with rate limiting."""
    output_dir.mkdir(parents=True, exist_ok=True)
    semaphore = asyncio.Semaphore(max_concurrent)

    stats = {
        "total": len(papers),
        "downloaded": 0,
        "skipped_no_url": 0,
        "skipped_exists": 0,
        "failed": 0,
        "downloaded_files": [],
        "failed_papers": [],
    }

    async def download_with_semaphore(paper: dict) -> None:
        paper_id = get_paper_id(paper)
        pdf_url = get_pdf_url(paper)

        if not pdf_url:
            stats["skipped_no_url"] += 1
            return

        output_path = output_dir / f"{paper_id}.pdf"

        if output_path.exists():
            stats["skipped_exists"] += 1
            stats["downloaded_files"].append(str(output_path))
            return

        async with semaphore:
            async with httpx.AsyncClient() as client:
                success = await download_pdf(client, pdf_url, output_path, paper_id)

            if success:
                stats["downloaded"] += 1
                stats["downloaded_files"].append(str(output_path))
                print(f"  Downloaded: {paper_id}", file=sys.stderr)
            else:
                stats["failed"] += 1
                stats["failed_papers"].append(
                    {"id": paper_id, "title": paper.get("title"), "url": pdf_url}
                )

    # Create tasks for all papers
    tasks = [download_with_semaphore(paper) for paper in papers]

    print(f"Downloading PDFs for {len(papers)} papers...", file=sys.stderr)
    await asyncio.gather(*tasks)

    return stats


def main():
    parser = argparse.ArgumentParser(description="Download PDFs from paper metadata")
    parser.add_argument(
        "--input",
        type=Path,
        required=True,
        help="JSON file with paper metadata (deduplicated.json)",
    )
    parser.add_argument(
        "--output-dir", type=Path, required=True, help="Directory to save PDFs"
    )
    parser.add_argument(
        "--max-concurrent",
        type=int,
        default=MAX_CONCURRENT_DOWNLOADS,
        help="Max concurrent downloads",
    )
    args = parser.parse_args()

    # Load papers
    with open(args.input) as f:
        papers = json.load(f)

    if not isinstance(papers, list):
        print("Error: input file must contain a JSON array of papers", file=sys.stderr)
        sys.exit(1)

    # Download PDFs
    stats = asyncio.run(download_all(papers, args.output_dir, args.max_concurrent))

    # Print summary
    print("", file=sys.stderr)
    print("Download Summary:", file=sys.stderr)
    print(f"  Total papers: {stats['total']}", file=sys.stderr)
    print(f"  Downloaded: {stats['downloaded']}", file=sys.stderr)
    print(f"  Already existed: {stats['skipped_exists']}", file=sys.stderr)
    print(f"  No PDF URL: {stats['skipped_no_url']}", file=sys.stderr)
    print(f"  Failed: {stats['failed']}", file=sys.stderr)

    # Save stats
    stats_path = args.output_dir / "download_stats.json"
    with open(stats_path, "w") as f:
        json.dump(stats, f, indent=2)
    print(f"  Stats saved to: {stats_path}", file=sys.stderr)


if __name__ == "__main__":
    main()
