---
name: mats-fellow-handbook
description: This skill should be used when the user asks about MATS policies, procedures, or information - including compute access, housing, illness policy, reimbursements, mentor meetings, program schedule, or any other MATS cohort logistics. Trigger phrases include "MATS handbook", "how do I get compute", "MATS housing", "illness policy", "MATS schedule", or questions about MATS program operations.
---

# MATS Fellow Handbook Lookup

Answer questions about the MATS Winter 2026 fellow handbook by extracting and verifying quotes.

## Workflow

### Step 1: Download the Handbook

Download the handbook to `/tmp/mats-handbook.html`:

```bash
curl -sL "https://matsprogram.notion.site/mats-winter-26-fellow-handbook" -o /tmp/mats-handbook.html
```

### Step 2: Extract Relevant Quotes

Launch a subagent (Task tool, subagent_type: "general-purpose") to read `/tmp/mats-handbook.html` and find information relevant to the user's question.

**Critical instruction for the subagent**: Return ONLY direct quotes from the handbook. Format each quote with its section header. Example output:

```
## Compute

"To request compute access, fellows must submit a request through the MATS portal..."

## Reimbursements

"All reimbursement requests must be submitted within 30 days..."
```

The subagent should NOT summarize or paraphrase - only extract verbatim text.

### Step 3: Verify Quotes

For each quote returned by the subagent, verify it exists in the handbook using grep:

```bash
grep -F "unique phrase from quote" /tmp/mats-handbook.html
```

If a quote cannot be verified:
1. Note which quote failed verification
2. Launch a new subagent to re-read the handbook and find the correct quote for that topic
3. Verify the new quote

### Step 4: Format Output

Present your response in two clearly separated sections:

**Quotes from the handbook:**

> [Verified quote 1 with section header]

> [Verified quote 2 with section header]

**Interpretation:**

[Your summary and interpretation of how the quotes answer the user's question. Include any caveats about information that may be outdated or incomplete.]
