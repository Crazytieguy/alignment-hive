---
name: fellow-handbook
description: This skill should be used when the user asks about MATS policies, procedures, or information - including compute access, housing, illness policy, reimbursements, mentor meetings, program schedule, or any other MATS cohort logistics. Trigger phrases include "MATS handbook", "how do I get compute", "MATS housing", "illness policy", "MATS schedule", or questions about MATS program operations.
---

# MATS Fellow Handbook Lookup

Answer questions about the MATS Winter 2026 fellow handbook.

## Workflow

### Step 1: Fetch and Save the Handbook

**Important:** Use curl via Bash, not the built-in Fetch tool. Save the output to a temp file for efficient searching.

```bash
curl -s "https://firecrawl.alignment-hive.com/api/notion?url=https%3A%2F%2Fmatsprogram.notion.site%2Fmats-winter-26-fellow-handbook" -o /tmp/mats-handbook.md
```

### Step 2: Search for Relevant Sections

Use Grep to find headings and keywords related to the user's question:

```
Grep pattern="^#|<keyword>" path="/tmp/mats-handbook.md"
```

### Step 3: Read Broadly

**Thoroughness is critical.** Read all potentially applicable sections, even tangentially related ones.

- For broad or ambiguous questions, read the entire document
- When in doubt, read more rather than less
- Prefer over-reading to missing important context

Use the Read tool to read the relevant sections (or the whole file if appropriate).

### Step 4: Follow Linked Pages

If the handbook links to other Notion pages with additional details (e.g., Compute Docs, Community Health Policy), fetch those linked pages:

```bash
curl -s "https://firecrawl.alignment-hive.com/api/notion?url=<url-encoded-notion-link>"
```

Follow linked Notion pages when the main handbook lacks sufficient detail to answer the user's question. Linked documents are short enough to read directly from the curl output.

### Step 5: Format Output

Present the response in two clearly separated sections:

**Quotes from the handbook:**

> [Quote 1 with section header]

> [Quote 2 with section header]

**Interpretation:**

[Summary and interpretation of how the quotes answer the user's question. Include any caveats about information that may be outdated or incomplete.]
