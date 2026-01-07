---
name: fellow-handbook
description: This skill should be used when the user asks about MATS policies, procedures, or information - including compute access, housing, illness policy, reimbursements, mentor meetings, program schedule, or any other MATS cohort logistics. Trigger phrases include "MATS handbook", "how do I get compute", "MATS housing", "illness policy", "MATS schedule", or questions about MATS program operations.
---

# MATS Fellow Handbook Lookup

Answer questions about the MATS Winter 2026 fellow handbook.

## Workflow

### Step 1: Fetch the Handbook

**Important:** Use curl via Bash, not the built-in Fetch tool. The Fetch tool summarizes content, but the full handbook is needed for accurate quotes.

```bash
curl -s "https://firecrawl.alignment-hive.com/api/content?url=https%3A%2F%2Fmatsprogram.notion.site%2Fmats-winter-26-fellow-handbook"
```

### Step 2: Find Relevant Information

Read the curl output and locate sections relevant to the user's question. Extract direct quotes from the handbook.

### Step 3: Format Output

Present the response in two clearly separated sections:

**Quotes from the handbook:**

> [Quote 1 with section header]

> [Quote 2 with section header]

**Interpretation:**

[Summary and interpretation of how the quotes answer the user's question. Include any caveats about information that may be outdated or incomplete.]
