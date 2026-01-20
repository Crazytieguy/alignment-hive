---
name: lit-review
description: Generate comprehensive literature review from research proposal. Use this skill when the user asks for a "literature review", "lit review", "find related papers", "search for papers about", "what papers exist on", or wants to understand the research landscape for a topic. Can also be invoked with /lit-review.
---

# Literature Review Generator

Generate a comprehensive literature review for AI safety research. This skill walks you through the process step by step.

---

## Step 1: Welcome and Overview

Explain to the user what this skill does:

> "I'll help you conduct a comprehensive literature review. Here's what will happen:
>
> 1. **Setup** - We'll create a folder for your lit review outputs
> 2. **Research focus** - You'll tell me about your research topic (or I can help you develop one)
> 3. **Automated search** - I'll search academic databases, arXiv, and AI safety forums
> 4. **Processing** - I'll download papers, remove duplicates, and summarize each one
> 5. **Report** - You'll get a catalog of papers and a curated top-10 report
>
> The whole process runs mostly automatically, but I'll check in at key points. Ready to get started?"

Wait for confirmation before proceeding.

---

## Step 2: Create Output Folder

Ask the user where they'd like to save the literature review outputs:

> "Where would you like me to save the literature review? I'll create a folder for all the papers, summaries, and reports.
>
> You can give me a path like `./my_project_lit_review` or just a name like `interpretability_review` and I'll create it in your current directory."

Once they provide a location:
1. Create the directory
2. If in a git repo, offer to add it to `.gitignore` (the folder can get large with PDFs)

---

## Step 3: Research Proposal

The literature review needs a research focus to guide the search. Ask the user:

> "Do you have a research proposal or project description I should use to guide the search?
>
> - **If yes**: Give me the file path and I'll read it
> - **If no**: I can help you articulate your research focus through a few questions
>
> (If you have something in Google Docs or another cloud tool, you can copy-paste it into a local text file—something like `proposal.md` or `proposal.txt` in your project folder—and give me that path.)"

### If they have a proposal file
Read the file and confirm you understand the research focus.

### If they want help creating one
Conduct a brief interview:

1. **Research question**: "What question are you trying to answer, or what problem are you trying to solve?"
2. **Key concepts**: "What are the main technical concepts or methods involved?"
3. **Related fields**: "Are there adjacent areas that might have relevant work? (e.g., if studying AI interpretability, maybe also cognitive science or software debugging)"
4. **Known works**: "Are there any specific papers, authors, or research groups you already know are relevant?"

Based on their answers, synthesize a brief research focus document (~1 page). Show it to them and ask if it captures their intent. Save it to their output folder as `research_proposal.md`.

---

## Step 4: Permissions Overview

Before starting the automated process, explain what permissions will be needed:

> "For the literature review, I'll need to:
>
> - **Run Python scripts** - These search databases, download papers, and process results
> - **Read and write files** - To save papers, summaries, and reports to your folder
> - **Make web requests** - To search Semantic Scholar, arXiv, LessWrong, and optionally Google Scholar
>
> The scripts are part of this plugin and run locally on your machine. No data is sent anywhere except the search queries to the academic databases.
>
> You may see permission prompts as I work—these are normal. Let me know if you have any questions before we begin."

Wait for the user to confirm they're comfortable proceeding.

**Optional enhancement - Exa Search API:**

> "By the way, if you want higher-quality search results, you can optionally set up an Exa API key. Exa is a semantic search engine designed for AI—it finds more relevant results than keyword search.
>
> It's free to sign up at https://exa.ai and you get generous free credits. If you have a key, set it as `EXA_API_KEY` in your environment and I'll use it automatically.
>
> This is totally optional—the literature review works fine without it."

---

## Step 5: Prerequisites Check

Check if uv is installed:

!`which uv || echo "UV_NOT_FOUND"`

If the output shows "UV_NOT_FOUND", tell the user:

> "I need a tool called `uv` to run the search scripts, but it's not installed on your system. It's a fast Python package manager.
>
> Would you like me to install it? I'll run:
> ```
> curl -LsSf https://astral.sh/uv/install.sh | sh
> ```
>
> This is safe and widely used in the Python community."

If they agree, run the installation command and verify it worked. If they decline, explain the skill cannot proceed without it.

---

## Execution

Once setup is complete, execute the phases below. This process is largely autonomous, but you can adapt based on results—for example, adding additional search stages if gaps remain.

### Phase 1: Generate Search Queries

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

### Phase 2: Run Searches (Stage 1 - Exploratory)

**Stage 1 is intentionally small** - the goal is to quickly understand the terminology and landscape, not to be comprehensive. Limit to ~10 results per source. The refined Stage 2 search will be the thorough one.

Create the raw_results directory:

```bash
mkdir -p <output_dir>/raw_results
```

**Academic sources (Semantic Scholar, arXiv, Google Scholar):**

```bash
uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/run_searches.py \
  --queries <output_dir>/search_terms.json \
  --output-dir <output_dir>/raw_results \
  --scripts-dir ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review \
  --arxiv-limit 10 \
  --semantic-scholar-limit 10 \
  --google-scholar-limit 10
```

Note: Google Scholar may fail due to rate limiting—this is expected.

**LessWrong and Alignment Forum:**

For LW/AF posts, use the WebSearch tool directly. Keep it brief for Stage 1—just 1-2 searches to get a sense of the relevant terminology:

```
site:lesswrong.com [main query]
```

Collect the URLs (aim for ~5-10 posts) and save them to `<output_dir>/raw_results/lesswrong_urls.json` as a JSON array:

```json
[
  {"url": "https://www.lesswrong.com/posts/...", "title": "Post Title"},
  {"url": "https://www.alignmentforum.org/posts/...", "title": "Another Post"}
]
```

Then fetch full content (including comments) via GraphQL:

```bash
uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/fetch_lesswrong.py \
  --urls <output_dir>/raw_results/lesswrong_urls.json \
  --output <output_dir>/raw_results/lesswrong.json
```

<!--
INTERNAL NOTES FOR FUTURE CLAUDES - Exa Search API:

If the user has an Exa API key (EXA_API_KEY environment variable), you can use the
Exa search script for higher-quality semantic search results:

```bash
uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/search_exa.py \
  --queries <output_dir>/search_terms.json \
  --output <output_dir>/raw_results/exa_results.json \
  --ai-safety-domains
```

Exa (https://exa.ai) provides:
- Semantic/neural search (not just keyword matching)
- High-quality curated index
- Better results for technical/research queries
- Free tier with generous limits

To use Exa, the user needs to:
1. Sign up at https://exa.ai (free)
2. Get an API key from the dashboard
3. Set EXA_API_KEY environment variable or add to .env file

Exa search can supplement or replace the WebSearch approach for LW/AF when available.
-->

### Phase 3: Deduplicate

Run the deduplication script to merge results from all sources:

```bash
uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/dedup_papers.py \
  --input-dir <output_dir>/raw_results/ \
  --output <output_dir>/deduplicated.json \
  --threshold 0.85
```

### Phase 4: Download and Convert PDFs

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

### Phase 5: Process LessWrong/AF Posts

Convert LessWrong and Alignment Forum posts to markdown:

```bash
uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/html_to_markdown.py \
  --input <output_dir>/deduplicated.json \
  --output-dir <output_dir>/papers/
```

### Phase 6: Parallel Summarization

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

### Phase 7: Generate Catalog

Run the catalog generator:

```bash
uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/generate_catalog.py \
  --summaries <output_dir>/summaries/ \
  --papers <output_dir>/deduplicated.json \
  --output <output_dir>/catalog.md
```

### Phase 8: Generate Report (Stage 1)

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

## Stage 2: Refined Search (Comprehensive)

**This is the main search.** Now that you understand the terminology and landscape from Stage 1, run a comprehensive search with the refined queries. Use full limits (100 per source).

### Phase 9: Generate Refined Search Terms

Analyze the Stage 1 results to create refined search queries. Consider:

1. **Terms from high-relevance papers**—Extract key terminology, author names, and specific concepts from papers rated HIGH or MEDIUM relevance
2. **Negative terms**—Identify terms to exclude (e.g., `-particle -collider` if "accelerator" matched physics papers)
3. **Venue-specific terms**—Note which venues (arXiv categories, journals) had relevant papers
4. **Gap-filling terms**—Create queries specifically targeting gaps identified in Stage 1
5. **Citation mining**—If high-relevance papers cite specific works, include those

Generate 8-12 refined queries and save to `<output_dir>/search_terms_stage2.json`.

### Phase 10: Run Stage 2 Searches

Run comprehensive searches with refined terms (full limits):

```bash
mkdir -p <output_dir>/raw_results_stage2

uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/run_searches.py \
  --queries <output_dir>/search_terms_stage2.json \
  --output-dir <output_dir>/raw_results_stage2 \
  --scripts-dir ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review \
  --arxiv-limit 100 \
  --semantic-scholar-limit 100 \
  --google-scholar-limit 50
```

**LessWrong and Alignment Forum (comprehensive):**

Now do thorough LW/AF searches with all refined queries:
```
site:lesswrong.com [refined query 1]
site:lesswrong.com [refined query 2]
site:alignmentforum.org [refined query 1]
...
```

Collect all URLs and fetch via `fetch_lesswrong.py` as in Stage 1.

### Phase 11: Merge and Deduplicate

Combine Stage 1 and Stage 2 results:

```bash
uv run ${CLAUDE_PLUGIN_ROOT}/scripts/lit-review/dedup_papers.py \
  --input-dir <output_dir>/raw_results/ \
  --input-dir <output_dir>/raw_results_stage2/ \
  --output <output_dir>/deduplicated_merged.json \
  --threshold 0.85
```

### Phase 12: Download and Summarize New Papers

1. Compare `deduplicated_merged.json` to original `deduplicated.json` to identify NEW papers
2. Download PDFs for new papers only
3. Convert new PDFs to markdown
4. Process any new LessWrong/AF posts with `html_to_markdown.py`
5. Spawn summarizer agents for new papers (in batches of 5)

### Phase 13: Evaluate and Decide Next Steps

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
