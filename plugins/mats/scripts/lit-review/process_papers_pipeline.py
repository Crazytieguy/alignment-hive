# /// script
# requires-python = ">=3.11"
# dependencies = ["httpx", "aiofiles", "markdownify>=0.13.1", "pymupdf4llm", "pymupdf"]
# ///
"""
Pipeline: download PDFs, convert to markdown, and process LW/AF posts incrementally.

As each PDF downloads, it's immediately queued for markdown conversion.
LW/AF posts with HTML content are converted in parallel.
Completed markdown file paths are printed to stdout as they finish.
"""

import argparse
import asyncio
import json
import os
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import aiofiles
import httpx
import pymupdf4llm
from markdownify import markdownify as md

from paper_ids import get_paper_id, legacy_forum_stem

MAX_CONCURRENT_DOWNLOADS = 5
MAX_CONVERTER_WORKERS = 4
MAX_RETRIES = 5
TIMEOUT_SECONDS = 120
# fetch_lesswrong.py's detect_source values; these records are converted from html_content
FORUM_SOURCES = ("lesswrong", "alignment_forum")


def get_pdf_url(paper: dict) -> str | None:
    if paper.get("pdf_url"):
        return paper["pdf_url"]
    return (paper.get("openAccessPdf") or {}).get("url")


# --- Atomic writes ---


def write_atomic_text(path: Path, text: str) -> None:
    """Write text via a temp file + atomic rename so partial files never land."""
    tmp = path.with_name(f"{path.name}.{os.getpid()}.part")
    tmp.write_text(text, encoding="utf-8")
    tmp.replace(path)


# --- PDF conversion ---


def convert_pdf_to_markdown(pdf_path: Path, md_output_path: Path) -> bool:
    """Convert a PDF to markdown using pymupdf4llm."""
    try:
        md_text = pymupdf4llm.to_markdown(
            str(pdf_path), write_images=False, embed_images=False
        )
        write_atomic_text(md_output_path, md_text)
        return True
    except Exception as e:
        print(f"  Error converting {pdf_path.name}: {e}", file=sys.stderr)
        return False


# --- LW/AF HTML conversion ---


def format_comment(comment: dict, indent_level: int = 0) -> str:
    prefix = "#" * (3 + indent_level)
    author = comment.get("author", "Anonymous")
    score = comment.get("score", "?")
    content = comment.get("html_content") or ""
    if content.startswith("<"):
        content = md(content, strip=["script", "style"])
    lines = [f"{prefix} {author} (score: {score})", "", content, ""]
    for reply in comment.get("replies", []):
        lines.append(format_comment(reply, indent_level + 1))
    return "\n".join(lines)


def convert_lw_post(post: dict) -> str:
    title = post.get("title", "Untitled")
    author = post.get("author") or "Unknown"
    date = post.get("posted_at") or "Unknown"
    score = post.get("score", "?")
    url = post.get("url", "")
    html_content = post.get("html_content", "")
    comments = post.get("comments", [])
    main_content = md(html_content, strip=["script", "style"]) if html_content else ""
    lines = [
        f"# {title}",
        "",
        f"**Author:** {author}",
        f"**Posted:** {date}",
        f"**Score:** {score}",
        f"**URL:** {url}",
        "",
        "---",
        "",
        main_content,
        "",
    ]
    if comments:
        lines.extend(["---", "", f"## Comments ({len(comments)} comments)", ""])
        for comment in comments:
            lines.append(format_comment(comment))
    return "\n".join(lines)


# --- Pipeline ---


async def download_pdf(
    client: httpx.AsyncClient, url: str, output_path: Path, paper_id: str
) -> str | None:
    """Download to output_path; returns None on success, else a one-line reason."""
    for attempt in range(MAX_RETRIES):
        try:
            resp = await client.get(url, follow_redirects=True, timeout=TIMEOUT_SECONDS)
            resp.raise_for_status()
            # Check the bytes, not the headers: publisher landing pages are served for
            # .pdf URLs, and a saved HTML file would only fail later in pymupdf.
            if not resp.content.startswith(b"%PDF-"):
                return f"not a PDF ({resp.headers.get('content-type', 'unknown type')})"
            tmp_path = output_path.with_name(f"{output_path.name}.{os.getpid()}.part")
            async with aiofiles.open(tmp_path, "wb") as f:
                await f.write(resp.content)
            tmp_path.replace(output_path)
            return None
        except Exception as e:
            permanent = isinstance(e, httpx.HTTPStatusError) and e.response.status_code in (403, 404, 451)
            if permanent or attempt == MAX_RETRIES - 1:
                return str(e).splitlines()[0]
            await asyncio.sleep(2**attempt)
    return "unreachable"


async def run_pipeline(
    papers: list[dict],
    output_dir: Path,
    max_downloads: int,
    max_converters: int,
) -> dict:
    output_dir.mkdir(parents=True, exist_ok=True)

    stats = {
        "total": len(papers),
        "downloaded": 0,
        "download_skipped_no_url": 0,
        "download_skipped_exists": 0,
        "download_failed": 0,
        "converted_pdf": 0,
        "converted_lw": 0,
        "convert_failed": 0,
        "convert_skipped": 0,
        "ready_files": [],
        "failed": [],
    }

    def record_failure(paper: dict, paper_id: str, stage: str, reason: str) -> None:
        print(f"  {stage} failed for {paper_id}: {reason}", file=sys.stderr)
        stats["failed"].append(
            {"id": paper_id, "title": paper.get("title"), "stage": stage, "reason": reason}
        )

    # Separate LW/AF posts (HTML conversion) from PDF papers
    lw_posts = []
    pdf_papers = []
    for paper in papers:
        if paper.get("source", "").lower() in FORUM_SOURCES and paper.get("html_content"):
            lw_posts.append(paper)
        else:
            pdf_papers.append(paper)

    # Queue for PDF paths waiting to be converted
    convert_queue: asyncio.Queue[Path | None] = asyncio.Queue()

    # Thread pool for CPU-bound PDF→markdown conversion
    executor = ThreadPoolExecutor(max_workers=max_converters)
    loop = asyncio.get_event_loop()

    download_semaphore = asyncio.Semaphore(max_downloads)
    http_client = httpx.AsyncClient()
    pdf_by_path: dict[Path, dict] = {}

    async def download_and_enqueue(paper: dict) -> None:
        paper_id = get_paper_id(paper)
        pdf_url = get_pdf_url(paper)

        if not pdf_url:
            stats["download_skipped_no_url"] += 1
            return

        pdf_path = output_dir / f"{paper_id}.pdf"
        md_path = output_dir / f"{paper_id}.md"
        pdf_by_path[pdf_path] = paper

        # Skip if markdown already exists
        if md_path.exists():
            stats["convert_skipped"] += 1
            stats["ready_files"].append(str(md_path))
            print(str(md_path), flush=True)
            return

        # An existing file that is not a PDF (a landing page saved by an older run)
        # is re-downloaded rather than re-failing conversion forever.
        if pdf_path.exists() and pdf_path.read_bytes()[:5] == b"%PDF-":
            stats["download_skipped_exists"] += 1
            await convert_queue.put(pdf_path)
            return

        async with download_semaphore:
            error = await download_pdf(http_client, pdf_url, pdf_path, paper_id)

        if error is None:
            stats["downloaded"] += 1
            print(f"  Downloaded: {paper_id}", file=sys.stderr)
            await convert_queue.put(pdf_path)
        else:
            stats["download_failed"] += 1
            record_failure(paper, paper_id, "download", error)

    async def converter_worker() -> None:
        """Pull PDFs from the queue and convert to markdown.

        Pipelines with downloads — conversion starts as soon as the first
        PDF is ready rather than waiting for all downloads to finish.
        """
        while True:
            pdf_path = await convert_queue.get()
            if pdf_path is None:
                convert_queue.task_done()
                break

            md_path = pdf_path.with_suffix(".md")
            if md_path.exists():
                stats["convert_skipped"] += 1
                stats["ready_files"].append(str(md_path))
                print(str(md_path), flush=True)
                convert_queue.task_done()
                continue

            # Run CPU-bound conversion in thread pool
            success = await loop.run_in_executor(
                executor, convert_pdf_to_markdown, pdf_path, md_path
            )

            if success:
                stats["converted_pdf"] += 1
                stats["ready_files"].append(str(md_path))
                print(str(md_path), flush=True)
                print(
                    f"  Converted: {pdf_path.name} → {md_path.name}", file=sys.stderr
                )
            else:
                stats["convert_failed"] += 1
                record_failure(pdf_by_path.get(pdf_path, {}), pdf_path.stem, "convert", "see error above")

            convert_queue.task_done()

    def process_lw_posts() -> None:
        for post in lw_posts:
            title = post.get("title") or "untitled"
            paper_id = get_paper_id(post)
            md_path = output_dir / f"{paper_id}.md"
            legacy_path = output_dir / f"{legacy_forum_stem(post)}.md"
            if legacy_path.exists() and not md_path.exists():
                md_path = legacy_path  # converted by an older run under the title slug

            if md_path.exists():
                stats["convert_skipped"] += 1
                stats["ready_files"].append(str(md_path))
                print(str(md_path), flush=True)
                continue

            markdown_content = convert_lw_post(post)
            write_atomic_text(md_path, markdown_content)
            stats["converted_lw"] += 1
            stats["ready_files"].append(str(md_path))
            print(str(md_path), flush=True)
            print(f"  Converted LW/AF: {title[:50]}...", file=sys.stderr)

    # --- Run everything ---
    print(
        f"Pipeline: {len(pdf_papers)} PDFs to download/convert, "
        f"{len(lw_posts)} LW/AF posts ({max_converters} converter threads)",
        file=sys.stderr,
    )

    # Start converter workers
    converter_tasks = [
        asyncio.create_task(converter_worker()) for _ in range(max_converters)
    ]

    # Process LW/AF posts in a thread (fast, no async needed)
    lw_future = loop.run_in_executor(executor, process_lw_posts)

    # Download all PDFs (each enqueues conversion on completion)
    download_tasks = [download_and_enqueue(paper) for paper in pdf_papers]
    await asyncio.gather(*download_tasks)

    # Signal converter workers to stop (one sentinel per worker)
    for _ in range(max_converters):
        await convert_queue.put(None)

    # Wait for all conversions to finish
    await asyncio.gather(*converter_tasks)

    # Wait for LW/AF processing
    await lw_future

    await http_client.aclose()
    executor.shutdown(wait=False)
    return stats


def main():
    parser = argparse.ArgumentParser(
        description="Pipeline: download, convert, and process papers"
    )
    parser.add_argument(
        "--input",
        type=Path,
        required=True,
        help="JSON file with paper metadata",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        required=True,
        help="Directory for PDFs and markdown",
    )
    parser.add_argument(
        "--max-downloads",
        type=int,
        default=MAX_CONCURRENT_DOWNLOADS,
        help="Max concurrent downloads (default: %(default)s)",
    )
    parser.add_argument(
        "--max-converters",
        type=int,
        default=MAX_CONVERTER_WORKERS,
        help="Max converter threads (default: %(default)s)",
    )
    args = parser.parse_args()

    with open(args.input) as f:
        papers = json.load(f)

    if not isinstance(papers, list):
        print(
            "Error: input file must contain a JSON array of papers", file=sys.stderr
        )
        sys.exit(1)

    stats = asyncio.run(
        run_pipeline(papers, args.output_dir, args.max_downloads, args.max_converters)
    )

    # Print summary to stderr
    print("", file=sys.stderr)
    print("Pipeline Summary:", file=sys.stderr)
    print(f"  Total papers: {stats['total']}", file=sys.stderr)
    print(f"  PDFs downloaded: {stats['downloaded']}", file=sys.stderr)
    print(f"  PDFs already existed: {stats['download_skipped_exists']}", file=sys.stderr)
    print(f"  No PDF URL: {stats['download_skipped_no_url']}", file=sys.stderr)
    print(f"  Download failed: {stats['download_failed']}", file=sys.stderr)
    print(f"  PDFs converted: {stats['converted_pdf']}", file=sys.stderr)
    print(f"  LW/AF posts converted: {stats['converted_lw']}", file=sys.stderr)
    print(f"  Conversion failed: {stats['convert_failed']}", file=sys.stderr)
    print(f"  Already had markdown: {stats['convert_skipped']}", file=sys.stderr)
    print(f"  Total ready files: {len(stats['ready_files'])}", file=sys.stderr)
    for failure in stats["failed"]:
        print(f"  Failed ({failure['stage']}): {failure['id']} - {failure['reason']}", file=sys.stderr)

    # Save stats
    stats_path = args.output_dir / "pipeline_stats.json"
    with open(stats_path, "w") as f:
        json.dump(stats, f, indent=2)
    print(f"  Stats saved to: {stats_path}", file=sys.stderr)


if __name__ == "__main__":
    main()
