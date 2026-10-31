# lit-review

A literature review pipeline for Claude Code.

## What This Plugin Does

**Literature review** (`/lit-review:lit-review`) — Staged literature review pipeline. Searches arXiv, Semantic Scholar, and Google Scholar; collects LessWrong and Alignment Forum posts. Downloads papers, converts to markdown, generates summaries with relevance scoring, and produces a final report. Resumable if interrupted. Works for any research topic, not just AI safety.

## Requirements

- [uv](https://docs.astral.sh/uv/) for running the literature review Python scripts.
