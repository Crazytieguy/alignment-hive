---
name: lit-review
description: Generate comprehensive literature review from research proposal. Use this skill when the user asks for a "literature review", "lit review", "find related papers", "search for papers about", "what papers exist on", or wants to understand the research landscape for a topic. Can also be invoked with /lit-review.
---

# Literature Review Generator

Generate a comprehensive literature review for AI safety research.

## Prerequisites

Check if uv is installed:

!`which uv || echo "UV_NOT_FOUND"`

If uv is not found, ask the user if they'd like to install it before continuing. uv is required to run the search and processing scripts. Installation: `curl -LsSf https://astral.sh/uv/install.sh | sh`

## Getting the Research Proposal

The literature review requires a research proposal to guide the search. There are two paths:

### Option A: User has a proposal file
If the user provides a path to a proposal markdown file, read it directly.

### Option B: Interactive proposal creation
If the user doesn't have a written proposal, help them create one through a brief interview:

1. **Research question**: What question are you trying to answer?
2. **Key concepts**: What are the main technical concepts involved?
3. **Related fields**: What adjacent areas might be relevant?
4. **Known works**: Are there any specific papers or authors you're already aware of?

Based on their answers, create a brief proposal document and save it to a location they specify (or suggest `./research_proposal.md`).

## Execution

Once you have the proposal content, execute the phases below. This process is largely autonomous, but you can adapt based on results—for example, stopping after one search stage if coverage is sufficient, or adding a third stage if gaps remain.

### Phase 1: Setup

1. Extract project name from the proposal filename (or ask user for a name if created interactively)
2. Create output directory: `<proposal_dir>/<project_name>_lit_review/`
3. Check if in a git repo—if so, add the output directory to `.gitignore`
4. Save the proposal content for use in later phases

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
  "neural network feature visualization safety"
]
```

### Phase 3: Run Searches

Create the raw_results directory and run all searches in parallel:

```bash
mkdir -p <output_dir>/raw_results

uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/run_searches.py \
  --queries <output_dir>/search_terms.json \
  --output-dir <output_dir>/raw_results \
  --scripts-dir ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review
```

Note: Google Scholar may fail due to rate limiting—this is expected. Continue with other sources.

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

Convert LessWrong and Alignment Forum posts to markdown:

```bash
uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/html_to_markdown.py \
  --input <output_dir>/deduplicated.json \
  --output-dir <output_dir>/papers/
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

### Phase 9: Generate Report (Stage 1)

Read the catalog and summaries. Create `<output_dir>/stage1_report.md` with:

1. Executive summary of the literature landscape
2. Top 10 most relevant papers/posts (by relevance score), each with:
   - Full title and authors
   - Source and URL
   - Why it's relevant to the proposal (2-3 sentences)
   - Key takeaways for the research
   - How it might influence the proposed work
3. Gaps identified—what important topics weren't well covered
4. **Search term analysis:**
   - Which search terms yielded relevant results
   - Which terms had poor precision (e.g., "accelerator" matching unrelated fields)
   - Terminology used in high-relevance papers that wasn't in original queries
5. Recommended refined search terms for Stage 2

---

## Stage 2: Refined Search

Stage 2 uses insights from Stage 1 to run a more targeted search. Proceed automatically.

### Phase 10: Generate Refined Search Terms

Analyze the Stage 1 results to create refined search queries. Consider:

1. **Terms from high-relevance papers**—Extract key terminology, author names, and specific concepts from papers rated HIGH or MEDIUM relevance
2. **Negative terms**—Identify terms to exclude (e.g., `-particle -collider` if "accelerator" matched physics papers)
3. **Venue-specific terms**—Note which venues (arXiv categories, journals) had relevant papers
4. **Gap-filling terms**—Create queries specifically targeting gaps identified in Stage 1
5. **Citation mining**—If high-relevance papers cite specific works, include those

Generate 8-12 refined queries and save to `<output_dir>/search_terms_stage2.json`.

### Phase 11: Run Stage 2 Searches

Run searches with refined terms:

```bash
mkdir -p <output_dir>/raw_results_stage2

uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/run_searches.py \
  --queries <output_dir>/search_terms_stage2.json \
  --output-dir <output_dir>/raw_results_stage2 \
  --scripts-dir ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review
```

### Phase 12: Merge and Deduplicate

Combine Stage 1 and Stage 2 results:

```bash
uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/dedup_papers.py \
  --input-dir <output_dir>/raw_results/ \
  --input-dir <output_dir>/raw_results_stage2/ \
  --output <output_dir>/deduplicated_merged.json \
  --threshold 0.85
```

### Phase 13: Download and Summarize New Papers

1. Compare `deduplicated_merged.json` to original `deduplicated.json` to identify NEW papers
2. Download PDFs for new papers only
3. Convert new PDFs to markdown
4. Process any new LessWrong/AF posts with `html_to_markdown.py`
5. Spawn summarizer agents for new papers (in batches of 5)

### Phase 14: Evaluate and Decide Next Steps

After processing Stage 2 results, assess the current state:

- How many high-relevance papers have been found?
- Are there still significant gaps in coverage?
- Did the refined search terms reveal new terminology or subfields worth exploring?
- Are there highly-cited papers that keep appearing in references but weren't captured?

**Based on this assessment, choose the appropriate next action:**

1. **Run another search stage** if major gaps remain or new promising search directions emerged. Repeat Phases 10-13 with further refined terms (save to `search_terms_stage3.json`, `raw_results_stage3/`, etc.).

2. **Proceed to final reporting** if coverage is sufficient. Continue to the Final Output section below.

3. **Explore a specific tangent** if the findings suggest a related but distinct area worth investigating separately.

Use your judgment based on the research proposal's goals and the quality of results so far.

---

## Final Output

When ready to conclude, generate the final deliverables:

### Generate Final Catalog

```bash
uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/generate_catalog.py \
  --summaries <output_dir>/summaries/ \
  --papers <output_dir>/deduplicated_merged.json \
  --output <output_dir>/catalog.md
```

### Generate Top 10 Report

Create `<output_dir>/top_10_report.md` with:
- Executive summary of the literature landscape
- Top 10 most relevant papers/posts, each with title, authors, source, URL, relevance explanation, and key takeaways
- Note which papers came from which search stage
- Analysis of how refined searches improved coverage
- Remaining gaps or suggested future directions

### Save Progress

Save final state to `<output_dir>/progress.json` with completion timestamp and statistics for each stage completed.

### Report to User

Summarize:
- Location of `catalog.md` and `top_10_report.md`
- Total papers found and summarized across all stages
- Search term evolution across stages
- Any issues encountered

## Error Handling

- If a search source fails completely, continue with others
- If PDF download fails, skip that paper (it will have no summary)
- If summarization fails for a paper, retry once, then mark as "summary unavailable"
- If Google Scholar blocks requests, note this but continue
- Always save progress to `progress.json` after each major phase

## Resume Capability

If `<output_dir>/progress.json` exists, check which phases completed and resume from where left off. Skip phases that already have output files.
