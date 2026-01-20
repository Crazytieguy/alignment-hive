# /// script
# requires-python = ">=3.11"
# dependencies = ["markdownify>=0.13.1"]
# ///
"""
Convert HTML content from LessWrong/Alignment Forum posts to markdown.

Reads deduplicated.json, extracts LessWrong/AF posts with html_content,
and writes markdown files to the output directory.
"""

import argparse
import json
import re
from datetime import datetime
from pathlib import Path

from markdownify import markdownify as md


def slugify(text: str) -> str:
    """Convert text to a safe filename."""
    text = text.lower()
    text = re.sub(r'[^\w\s-]', '', text)
    text = re.sub(r'[-\s]+', '-', text).strip('-')
    return text[:80]


def format_comment(comment: dict, indent_level: int = 0) -> str:
    """Format a comment and its replies recursively."""
    prefix = "#" * (3 + indent_level)
    author = comment.get("author", "Anonymous")
    score = comment.get("score", "?")
    content = comment.get("html_content") or comment.get("content", "")

    if content.startswith("<"):
        content = md(content, strip=['script', 'style'])

    lines = [f"{prefix} {author} (score: {score})", "", content, ""]

    for reply in comment.get("replies", []):
        lines.append(format_comment(reply, indent_level + 1))

    return "\n".join(lines)


def convert_post(post: dict) -> str:
    """Convert a LessWrong/AF post to markdown format."""
    title = post.get("title", "Untitled")
    author = post.get("author", "Unknown")
    date = post.get("date", post.get("published_date", "Unknown"))
    score = post.get("score", "?")
    url = post.get("url", "")
    html_content = post.get("html_content", "")
    comments = post.get("comments", [])

    # Convert HTML content to markdown
    main_content = md(html_content, strip=['script', 'style']) if html_content else ""

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
        lines.extend([
            "---",
            "",
            f"## Comments ({len(comments)} comments)",
            "",
        ])
        for comment in comments:
            lines.append(format_comment(comment))

    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description="Convert LW/AF HTML posts to markdown")
    parser.add_argument("--input", required=True, help="Path to deduplicated.json")
    parser.add_argument("--output-dir", required=True, help="Output directory for markdown files")
    args = parser.parse_args()

    input_path = Path(args.input)
    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)

    with open(input_path) as f:
        papers = json.load(f)

    converted = 0
    for paper in papers:
        source = paper.get("source", "").lower()
        if source not in ("lesswrong", "alignment_forum", "alignmentforum"):
            continue

        if not paper.get("html_content"):
            continue

        title = paper.get("title", "untitled")
        paper_id = paper.get("id", slugify(title))
        output_file = output_dir / f"{paper_id}.md"

        markdown_content = convert_post(paper)
        output_file.write_text(markdown_content)
        converted += 1
        print(f"Converted: {title[:50]}...")

    print(f"\nConverted {converted} LessWrong/AF posts to markdown")


if __name__ == "__main__":
    main()
