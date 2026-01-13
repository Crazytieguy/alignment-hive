---
description: Generate comprehensive literature review from research proposal
argument-hint: path/to/proposal.md
allowed-tools: Read, Write, Bash, Task, Glob, Grep
---

# Literature Review Generator

Generate a comprehensive, autonomous literature review for an AI safety research proposal.

**Two-Stage Process:** This command runs two search stages automatically:
1. **Stage 1:** Initial search with terms generated from the proposal
2. **Stage 2:** Refined search using improved terms discovered from Stage 1 results

This catches terminology mismatches (e.g., "accelerator" matching particle physics instead of ML hardware) and fills gaps identified in the first pass.

## Input

The argument `$ARGUMENTS` should be the path to a project proposal markdown file.

## Execution

Execute all phases in order. This is a fully autonomous process - run to completion without user interaction.

### Phase 1: Setup

1. Read the proposal file at `$ARGUMENTS`
2. Extract project name from the filename (strip `.md` extension and path)
3. Create output directory: `<proposal_dir>/<project_name>_lit_review/`
4. Check if in a git repo - if so, add the output directory to `.gitignore`
5. Save the proposal content for use in later phases

### Phase 2: Generate Search Queries

Analyze the proposal and generate 8-12 diverse search queries. Consider:
- Main research question and hypotheses
- Key technical concepts and methods
- Related fields and adjacent topics
- Specific researchers or works mentioned
- Synonyms and alternative phrasings

Save queries to `<output_dir>/search_terms.json` as a JSON array of strings.

Example format:
```json
[
  "AI alignment interpretability",
  "mechanistic interpretability transformer",
  "neural network feature visualization safety",
  ...
]
```

### Phase 3: Run Searches (Parallel)

Create the raw_results directory, then run all four search scripts in parallel:

```bash
mkdir -p <output_dir>/raw_results

# Run all searches in parallel using background processes
uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/search_semantic_scholar.py \
  --queries <output_dir>/search_terms.json \
  --output <output_dir>/raw_results/semantic_scholar.json \
  --limit 100 &

uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/search_arxiv.py \
  --queries <output_dir>/search_terms.json \
  --output <output_dir>/raw_results/arxiv.json \
  --limit 100 &

uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/search_lesswrong.py \
  --queries <output_dir>/search_terms.json \
  --output <output_dir>/raw_results/lesswrong.json \
  --limit 50 &

uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/search_google_scholar.py \
  --queries <output_dir>/search_terms.json \
  --output <output_dir>/raw_results/google_scholar.json \
  --limit 50 &

wait
```

Note: Google Scholar may fail due to rate limiting - this is expected. Continue with other sources.

### Phase 4: Deduplicate

Run the deduplication script to merge results from all sources:

```bash
uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/dedup_papers.py \
  --input-dir <output_dir>/raw_results/ \
  --output <output_dir>/deduplicated.json \
  --threshold 0.85
```

### Phase 5: Download and Convert PDFs

Create the papers directory and run download/conversion:

```bash
mkdir -p <output_dir>/papers

uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/download_pdfs.py \
  --input <output_dir>/deduplicated.json \
  --output-dir <output_dir>/papers/

uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/pdf_to_markdown.py \
  --input-dir <output_dir>/papers/ \
  --output-dir <output_dir>/papers/ \
  --ascii-width 60
```

### Phase 6: Process LessWrong/AF Posts

For posts from LessWrong and Alignment Forum (which don't have PDFs), convert the HTML content to markdown. Read the deduplicated.json, find entries with source "lesswrong" or "alignment_forum", and create markdown files in `<output_dir>/papers/` from their `html_content` field. Include the comments section.

Format each post as:
```markdown
# [Title]

**Author:** [author]
**Posted:** [date]
**Score:** [score]
**URL:** [url]

---

[Main content converted from HTML]

---

## Comments ([N] comments)

### [Author] (score: [score])
[Comment content]

#### [Reply Author] (score: [score])
[Reply content]

...
```

### Phase 7: Parallel Summarization

Create the summaries directory:

```bash
mkdir -p <output_dir>/summaries
```

List all markdown files in `<output_dir>/papers/`. For each paper/post that doesn't already have a summary in `<output_dir>/summaries/`, spawn a summarizer subagent.

**Spawn summarizer agents in parallel batches of 5.** For each batch:

Use the Task tool to spawn 5 summarizer agents simultaneously (in a single message with multiple Task tool calls). Each agent should receive:
- The paper markdown path
- The original proposal content (for relevance assessment)
- The output summary path

Example Task prompt for each agent:
```
Summarize the paper at: <output_dir>/papers/<paper_id>.md

Research proposal context:
<proposal content>

Write the summary to: <output_dir>/summaries/<paper_id>.md

Follow the summarizer agent instructions for output format.
```

Wait for each batch to complete before spawning the next batch. Continue until all papers are summarized.

### Phase 8: Generate Catalog

Run the catalog generator:

```bash
uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/generate_catalog.py \
  --summaries <output_dir>/summaries/ \
  --papers <output_dir>/deduplicated.json \
  --output <output_dir>/catalog.md
```

### Phase 9: Generate Top 10 Report (Stage 1)

Read the catalog and summaries. Create `<output_dir>/stage1_report.md` with:

1. Executive summary of the literature landscape
2. Top 10 most relevant papers/posts (by relevance score), each with:
   - Full title and authors
   - Source and URL
   - Why it's relevant to the proposal (2-3 sentences)
   - Key takeaways for the research
   - How it might influence the proposed work
3. Gaps identified - what important topics weren't well covered
4. **Search term analysis:**
   - Which search terms yielded relevant results
   - Which terms had poor precision (e.g., "accelerator" matching unrelated fields)
   - Terminology used in high-relevance papers that wasn't in original queries
5. Recommended refined search terms for Stage 2

---

## Stage 2: Refined Search

Stage 2 uses insights from Stage 1 to run a more targeted search.

### Phase 10: Generate Refined Search Terms

Analyze the Stage 1 results to create refined search queries. Consider:

1. **Terms from high-relevance papers** - Extract key terminology, author names, and specific concepts from papers rated HIGH or MEDIUM relevance
2. **Negative terms** - Identify terms to exclude (e.g., `-particle -collider` if "accelerator" matched physics papers)
3. **Venue-specific terms** - Note which venues (arXiv categories, journals) had relevant papers
4. **Gap-filling terms** - Create queries specifically targeting gaps identified in Stage 1
5. **Citation mining** - If high-relevance papers cite specific works, include those

Generate 8-12 refined queries and save to `<output_dir>/search_terms_stage2.json`.

Example refined terms:
```json
[
  "diffractive deep neural network D2NN",
  "optical computing matrix multiplication -particle",
  "photonic tensor core inference",
  "all-optical neural network waveguide",
  "author:Lin optical neural network UCLA"
]
```

### Phase 11: Run Stage 2 Searches (Parallel)

Run searches with refined terms, saving to separate files:

```bash
mkdir -p <output_dir>/raw_results_stage2

uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/search_semantic_scholar.py \
  --queries <output_dir>/search_terms_stage2.json \
  --output <output_dir>/raw_results_stage2/semantic_scholar.json \
  --limit 100 &

uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/search_arxiv.py \
  --queries <output_dir>/search_terms_stage2.json \
  --output <output_dir>/raw_results_stage2/arxiv.json \
  --limit 100 &

uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/search_lesswrong.py \
  --queries <output_dir>/search_terms_stage2.json \
  --output <output_dir>/raw_results_stage2/lesswrong.json \
  --limit 50 &

uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/search_google_scholar.py \
  --queries <output_dir>/search_terms_stage2.json \
  --output <output_dir>/raw_results_stage2/google_scholar.json \
  --limit 50 &

wait
```

### Phase 12: Merge and Deduplicate

Combine Stage 1 and Stage 2 results, then deduplicate:

```bash
# Merge all raw results from both stages
uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/dedup_papers.py \
  --input-dir <output_dir>/raw_results/ \
  --input-dir <output_dir>/raw_results_stage2/ \
  --output <output_dir>/deduplicated_merged.json \
  --threshold 0.85
```

Note: The dedup script should accept multiple `--input-dir` arguments and also skip papers already in an `--existing` file if provided.

### Phase 13: Download and Summarize New Papers

1. Compare `deduplicated_merged.json` to original `deduplicated.json` to identify NEW papers
2. Download PDFs for new papers only
3. Convert new PDFs to markdown
4. Process any new LessWrong/AF posts
5. Spawn summarizer agents for new papers (in batches of 5)

### Phase 14: Generate Final Reports

Regenerate the catalog and top 10 report with combined results:

```bash
uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/generate_catalog.py \
  --summaries <output_dir>/summaries/ \
  --papers <output_dir>/deduplicated_merged.json \
  --output <output_dir>/catalog.md
```

Create the final `<output_dir>/top_10_report.md` with:
- Combined results from both stages
- Note which papers came from Stage 1 vs Stage 2 refined search
- Analysis of how the refined search improved coverage

### Phase 15: Update Progress and Report

Save progress to `<output_dir>/progress.json`:
```json
{
  "completed_at": "<ISO timestamp>",
  "stages_completed": ["stage1", "stage2"],
  "stats": {
    "stage1": {
      "papers_found": <N>,
      "papers_after_dedup": <N>,
      "papers_summarized": <N>,
      "high_relevance_count": <N>
    },
    "stage2": {
      "refined_terms_count": <N>,
      "new_papers_found": <N>,
      "new_papers_summarized": <N>
    },
    "combined": {
      "total_papers": <N>,
      "total_summarized": <N>,
      "high_relevance_count": <N>,
      "medium_relevance_count": <N>
    }
  }
}
```

## Final Output

Report to the user:
- Location of `catalog.md` (full indexed catalog with both stages)
- Location of `top_10_report.md` (curated top picks)
- Location of `stage1_report.md` (intermediate report for reference)
- Summary statistics:
  - Stage 1: papers found, relevant papers identified
  - Stage 2: new papers from refined search, improvement in coverage
  - Combined totals
- Search term evolution (original terms → refined terms)
- Any issues encountered (failed downloads, search errors, rate limits)

## Error Handling

- If a search source fails completely, continue with others
- If PDF download fails, skip that paper (it will have no summary)
- If summarization fails for a paper, retry once, then mark as "summary unavailable"
- If Google Scholar blocks requests, note this but continue
- Always save progress to `progress.json` after each major phase

## Resume Capability

If `<output_dir>/progress.json` exists, check which phases completed and resume from where left off. Skip phases that already have output files.
