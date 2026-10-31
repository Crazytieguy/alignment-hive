"""File-stem rule shared by process_papers_pipeline.py and generate_catalog.py.

Both scripts are run with `uv run <path>`, which puts this directory on sys.path.
"""

import re


def sanitize_filename(name: str) -> str:
    name = re.sub(r'[<>:"/\\|?*]', "", name)
    name = re.sub(r"\s+", "_", name)
    return name[:100]


def get_paper_id(paper: dict) -> str:
    if paper.get("doi"):
        return sanitize_filename(paper["doi"].replace("/", "_"))
    if paper.get("arxiv_id"):
        # Older result files stored the entry URL; keep their stems stable.
        return sanitize_filename(f"arxiv_{paper['arxiv_id'].split('/')[-1]}")
    if paper.get("post_id"):
        return sanitize_filename(f"lw_{paper['post_id']}")
    if paper.get("paperId"):
        return sanitize_filename(f"s2_{paper['paperId']}")
    return sanitize_filename(paper.get("title", "unknown")[:50])


def legacy_forum_stem(post: dict) -> str:
    """Stem the pipeline used for LW/AF posts before get_paper_id covered them
    (slugified title). Kept so existing review folders resume cleanly."""
    text = (post.get("title") or "untitled").lower()
    text = re.sub(r"[^\w\s-]", "", text)
    return re.sub(r"[-\s]+", "-", text).strip("-")[:80]
