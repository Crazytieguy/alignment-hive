// Verbatim messages from real sessions, quoted in the post's threads. Bodies are HTML so the text stays exact.
import type { Message } from "./parts";

export const threads: Message[][] = [
  [
    {
      "kind": "me",
      "role": null,
      "speaker": "Me",
      "when": "Nov 11",
      "tags": [],
      "html": "<p>How hard would it be to have tldr render markdown, ideally using claude code's own markdown rendering? If it's easy please do that as well. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span></p>"
    },
    {
      "kind": "agent",
      "role": null,
      "speaker": "Agent",
      "when": "8 minutes later",
      "tags": [],
      "html": "<p>Rendering markdown turned out to be easy, so I did it: the TL;DR is now drawn by Claude Code's own markdown renderer, the same one replies use. I checked it in a live terminal session with the plugin loaded. Code spans showed in the reply's code colour, the debrief URL was a real clickable link, and the backticks were no longer visible. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span> plus one spacing fix after the <code>tl;dr</code> label. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span></p>"
    }
  ],
  [
    {
      "kind": "none",
      "role": "avoid",
      "speaker": "",
      "when": "",
      "tags": [
        "made up"
      ],
      "html": "<p>Build the <span class=\"tt\"><span class=\"tt-t\" tabindex=\"0\" role=\"button\" aria-describedby=\"tt-1\">mod version</span><span class=\"tt-b\" role=\"tooltip\" id=\"tt-1\">mods are plugins that change Claude Code's own interface, like a theme, a side panel or a status line; this one would change how replies are displayed</span></span> of the tldr plugin so I can try it live.</p>"
    },
    {
      "kind": "me",
      "role": "better",
      "speaker": "Me",
      "when": "Oct 8",
      "tags": [],
      "html": "<p>I want to discuss, design, and implement the mod version of the tldr plugin <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span>. I want to try it live in this session if possible. Things to keep in mind:</p><ul><li>I want it to be obvious to users how they can toggle it on/off in general, as well as expand a particular message (and collapse back)</li></ul><div class=\"trim trim-mid\" role=\"note\"><span>lines left out</span></div><ul><li>The main model ideally shouldn't need to know about this</li><li>A single haiku call that just gets the message and some effective tldr guidelines seems like it should be enough</li><li>I want this to work in both the cli and the desktop app (will start by testing in the cli)</li></ul><div class=\"trim trim-mid\" role=\"note\"><span>lines left out</span></div><p>If there are open design questions you can ask me, or just go ahead and build a live-reloading prototype and then we can iterate. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span></p>"
    },
    {
      "kind": "agent",
      "role": null,
      "speaker": "Agent",
      "when": "Oct 8",
      "tags": [],
      "html": "<p>I've built the tldr mod and it's ready to try live. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span></p><div class=\"trim trim-mid\" role=\"note\"><span>lines left out</span></div><ul><li><strong>Expanding one message:</strong> a long text block <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span> is drawn as <code>⏺ &lt;one sentence&gt;  [ more · 412 words ]</code>. Clicking it shows the full message with <code>[ less ]</code> underneath. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span></li></ul>"
    },
    {
      "kind": "me",
      "role": null,
      "speaker": "Me",
      "when": "Oct 8",
      "tags": [],
      "html": "<p>Ok this is pretty good, but I find it weird that [ more ] is on the right but [ less ] is below, I think I'd want them both to be below (and possibly add a line of spacing). <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span></p>"
    },
    {
      "kind": "agent",
      "role": null,
      "speaker": "Agent",
      "when": "Oct 8",
      "tags": [],
      "html": "<p>Both buttons now sit below the text with a blank line before them <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span> The changes reload when this turn ends. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span> I haven't seen the new layout drawn yet; this reply should collapse, so it will show you.</p>"
    }
  ],
  [
    {
      "kind": "none",
      "role": "avoid",
      "speaker": "",
      "when": "",
      "tags": [
        "made up"
      ],
      "html": "<p>I've been getting \"tl;dr unavailable: aborted\" in some of my sessions today, please investigate.</p>"
    },
    {
      "kind": "me",
      "role": "better",
      "speaker": "Me",
      "when": "Oct 15",
      "tags": [],
      "html": "<p>I've been getting \"tl;dr unavailable: aborted\" in some of my sessions today, please investigate. If you find the root cause and can fix it do so, and if you're confident in the fix after it's reviewed by sol you can land it, otherwise generate a <span class=\"tt\"><span class=\"tt-t\" tabindex=\"0\" role=\"button\" aria-describedby=\"tt-2\">debrief</span><span class=\"tt-b\" role=\"tooltip\" id=\"tt-2\">a report for me to review; more on these below</span></span></p>"
    },
    {
      "kind": "agent",
      "role": null,
      "speaker": "Agent",
      "when": "41 minutes later",
      "tags": [],
      "html": "<p>I found the cause of \"tl;dr unavailable: aborted\" and fixed it. Sol approved the change with no findings, and I landed it on local main as <code>08b3ac3</code>. Nothing was pushed and versions weren't bumped.</p>"
    }
  ],
  [
    {
      "kind": "comment",
      "role": null,
      "speaker": "Comment",
      "when": "on the report, Oct 8",
      "tags": [],
      "html": "<p>This seems good I think but I want it scrutinized by an adversarial gpt reviewer <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span></p>"
    },
    {
      "kind": "agent",
      "role": null,
      "speaker": "Agent",
      "when": "Oct 8",
      "tags": [],
      "html": "<div class=\"trim trim-start\" role=\"note\"><span>start left out</span></div><p>What the three rounds covered:</p><ol start=\"1\"><li><strong>First pass:</strong> found three real problems:</li></ol><ul><li>matching on text could summarize a block that was still streaming;</li><li>mismatched text left replies stuck on \"summarizing…\";</li><li>a refused Haiku call stranded the block.</li></ul><ol start=\"2\"><li><strong>Second pass:</strong> found that my fix sent hidden <code>&lt;context&gt;</code> text to Haiku.</li><li><strong>Third pass:</strong> approved the final design.</li></ol><div class=\"trim trim-end\" role=\"note\"><span>the rest left out</span></div>"
    }
  ],
  [
    {
      "kind": "agent",
      "role": null,
      "speaker": "Agent",
      "when": "Oct 8",
      "tags": [],
      "html": "<p>Both judges are done, and they agree that the summaries invent asks. GPT-6.1 Sol rated 20 of 30 faithful and Opus 22. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span></p><div class=\"trim trim-mid\" role=\"note\"><span>lines left out</span></div><p><span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span> I can re-run the same 30 replies with both judges to check it actually helps. That's about 90 calls and roughly 15 minutes.</p>"
    },
    {
      "kind": "none",
      "role": "avoid",
      "speaker": "",
      "when": "",
      "tags": [
        "made up"
      ],
      "html": "<p>Go with the fix you think is best and re-run the same 30 replies.</p>"
    },
    {
      "kind": "me",
      "role": "better",
      "speaker": "Me",
      "when": "Oct 8",
      "tags": [],
      "html": "<p>Ok tbh why don't you collect a larger eval than this and iterate on the prompt against gpt sol and opus ratings? use ultracode. Keep the prompt simple but make the summaries better. Basically what we're optimizing for is reducing the portion of time a user expands the message unnecessarily, doesn't expand when they should, or gets a false impression from the summary</p>"
    },
    {
      "kind": "agent",
      "role": null,
      "speaker": "Agent",
      "when": "48 minutes later",
      "tags": [],
      "html": "<div class=\"trim trim-start\" role=\"note\"><span>start left out</span></div><p><strong>What was measured:</strong> for each summary, the GPT-6.1 Sol and Opus judges counted three errors:</p><div class=\"trim trim-mid\" role=\"note\"><span>lines left out</span></div><p><strong>The setup:</strong></p><ul><li><strong>Data:</strong> 120 dev and 60 held-out test replies from your interactive sessions in this project.</li><li><strong>Variants:</strong> 14 prompt variants over 3 rounds.</li></ul><div class=\"trim trim-mid\" role=\"note\"><span>lines left out</span></div><p><strong>Results on the 60 held-out test replies:</strong></p><div class=\"qt\"><table><tr class=\"qth\"><td>Prompt</td><td>Errors per summary (avg of judges)</td><td>Opus</td><td>Sol</td><td>False impressions (Opus / Sol)</td></tr><tr><td data-h=\"Prompt\">Current</td><td data-h=\"Errors per summary (avg of judges)\">0.350</td><td data-h=\"Opus\">0.317</td><td data-h=\"Sol\">0.383</td><td data-h=\"False impressions (Opus / Sol)\">0.23 / 0.30</td></tr><tr><td data-h=\"Prompt\"><strong>outcome-first (applied)</strong></td><td data-h=\"Errors per summary (avg of judges)\"><strong>0.175</strong></td><td data-h=\"Opus\">0.117</td><td data-h=\"Sol\">0.233</td><td data-h=\"False impressions (Opus / Sol)\">0.08 / 0.17</td></tr></table></div><div class=\"trim trim-end\" role=\"note\"><span>the rest left out</span></div>"
    },
    {
      "kind": "me",
      "role": null,
      "speaker": "Me",
      "when": "Oct 9",
      "tags": [
        "two rounds of prompt changes later"
      ],
      "html": "<p><span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span> I actually want to do one last experiment: if I didn't care as much about it strictly being one sentence and under 30 words and we let the summaries sometimes run a bit longer, how much more accurate would they get and how much longer?</p><p>If after this experiment you don't think we can do better than the current version you can go ahead and land, otherwise report back</p>"
    },
    {
      "kind": "agent",
      "role": null,
      "speaker": "Agent",
      "when": "16 minutes later",
      "tags": [],
      "html": "<p>Longer summaries don't buy accuracy beyond the noise, so <span class=\"tt\"><span class=\"tt-t\" tabindex=\"0\" role=\"button\" aria-describedby=\"tt-3\">F3</span><span class=\"tt-b\" role=\"tooltip\" id=\"tt-3\">the prompt version it had reached by then</span></span> stays:</p><div class=\"trim trim-mid\" role=\"note\"><span>lines left out</span></div><p>Per your instruction, I'm landing F3. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span></p>"
    }
  ],
  [
    {
      "kind": "me",
      "role": null,
      "speaker": "Me",
      "when": "Nov 9",
      "tags": [],
      "html": "<p>Fable 5.2 is out, let's make an initial and conservative update to model-router based on the public material, and we can revisit with bolder claims after the discourse settles a bit</p>"
    },
    {
      "kind": "recall",
      "role": null,
      "speaker": "Recall",
      "when": "9 seconds later",
      "tags": [],
      "html": "<p>I want all copy (including old copy, why not) to go through both an independent fact-check and a fresh eyes read from an agent with no context on the code <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span> <time class=\"rdate\" datetime=\"2026-11-06\">Nov 6</time></p><div class=\"trim trim-mid\" role=\"note\"><span>other entries left out</span></div><p>copy diff that's non-trivial enough to require my approval should gate landing locally, not the release. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span> <time class=\"rdate\" datetime=\"2026-10-28\">Oct 28</time></p><div class=\"trim trim-mid\" role=\"note\"><span>other entries left out</span></div><p>if I'm going to have to look at the session anyway (for instance because you end on \"needs input\"), there's 0 point in you landing yourself - clicking the Land button costs me nothing. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span> <time class=\"rdate\" datetime=\"2026-10-29\">Oct 29</time></p>"
    },
    {
      "kind": "agent",
      "role": null,
      "speaker": "Agent",
      "when": "39 minutes later",
      "tags": [],
      "html": "<p>I've updated the delegating-to-models skill and its safeguards reference for Fable 5.2 <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span>. It's one commit (<code>688b2d11</code>) on branch <code>worktree-fable-5.2</code>, not landed, pushed or version-bumped. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span></p><div class=\"trim trim-mid\" role=\"note\"><span>lines left out</span></div><p><strong>Checks:</strong> <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span> GPT-6.1 Sol fact-checked the copy twice and a no-context Fable 5.2 agent read it twice; I applied nearly all their fixes. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span></p><div class=\"trim trim-mid\" role=\"note\"><span>lines left out</span></div><p>needs input: Fable 5.2 copy for model-router's delegating skill is committed on branch worktree-fable-5.2 (688b2d11); review the debrief and land.</p>"
    }
  ],
  [
    {
      "kind": "none",
      "role": "avoid",
      "speaker": "",
      "when": "",
      "tags": [
        "made up"
      ],
      "html": "<p>Add a line to the tldr prompt telling it to keep URLs.</p>"
    },
    {
      "kind": "me",
      "role": "better",
      "speaker": "Me",
      "when": "Nov 11",
      "tags": [],
      "html": "<p>User: \"tldr prompts should strongly encourage not hiding things like URLs behind the [more] button\", I directionally agree, but I want you to potentially generalize it, get new drafts of the prompt from a few models, and then see if you can measure the problem and whether the fix fixes it. I do sometimes hit this with things like debrief urls</p>"
    },
    {
      "kind": "agent",
      "role": null,
      "speaker": "Agent",
      "when": "44 minutes later",
      "tags": [],
      "html": "<div class=\"trim trim-start\" role=\"note\"><span>start left out</span></div><p><strong>Drafts:</strong> Opus, Fable (two independent runs), GPT-6.1 Sol and Grok each wrote a prompt from the same brief. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span></p><div class=\"trim trim-mid\" role=\"note\"><span>lines left out</span></div><ul><li><strong>Scoring each TL;DR on its own didn't work.</strong> Fable and Sol disagreed in direction: Fable said every draft beat the current prompt, Sol said most were worse. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span></li><li><strong>I switched to blind side-by-side comparisons</strong>, judged by Fable, Sol and Opus. All three agreed on direction.</li></ul><div class=\"trim trim-mid\" role=\"note\"><span>lines left out</span></div><ul><li>Links were kept in 86% of URL-bearing messages, up from 4%. <span class=\"cutin\" title=\"Words left out here\"><span aria-hidden=\"true\">[…]</span><span class=\"sr\"> (words left out) </span></span></li></ul><div class=\"trim trim-end\" role=\"note\"><span>the rest left out</span></div>"
    }
  ]
];
