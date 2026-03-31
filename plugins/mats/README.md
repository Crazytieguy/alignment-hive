# mats

Resources for MATS (ML Alignment Theory Scholars) fellows using Claude Code.

## What This Plugin Does

**Fellow handbook** (`/mats:fellow-handbook`) — Answers questions about MATS policies, logistics, and procedures by fetching and searching the fellow handbook. Covers compute access, housing, reimbursements, mentor meetings, program schedule, and more.

**Literature review** (`/mats:lit-review`) — Two-stage literature review pipeline. Searches arXiv, Semantic Scholar, and Google Scholar; collects LessWrong and Alignment Forum posts. Downloads papers, converts to markdown, generates summaries with relevance scoring, and produces a final report. Resumable if interrupted. Works for any research topic, not just AI safety.

## Requirements

- [uv](https://docs.astral.sh/uv/) for running the literature review Python scripts.
