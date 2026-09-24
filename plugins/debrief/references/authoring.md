# Debrief file

A debrief tells whoever delegated the task what happened and why, and asks for their judgement where it counts. You write `debrief.md`; `hive debrief render` builds its page. Where the user's instructions or saved preferences differ from this reference, follow theirs.

## Explaining

The page is often the main thing the task produced. For an experiment or analysis it is the research report: the findings, how they were reached, and why each judgement call went the way it did, in more detail than a person would write for an advisor. For any task, its value is how easily the reader comes to understand what was done and why. Leave nothing out because it is hard to explain; make it easy instead: big pictures and few words, meaning a picture or a demonstration first, then the fewest words that make the point beside it.

- Show the work rather than describe it: a plot, a screenshot, a command's real output, a before and after next to each other, each labelled as such. An item about something the reader could run or see carries a demonstration of it. A mechanism with several parts gets a small table or diagram. A rule, an algorithm or a metric gets a small worked example, with values from the work where it has them.
- For a thing built, say what it is and how it works; for a finding, what it shows and how it was reached. How either was checked goes in `checked`, described by what the check tested.
- Assume the reader has seen nothing from the session except their own messages and what they responded to; they often skip assistant messages. Call a thing by what it is or does, not by a label the session coined for it; where an item rests on something they said earlier, restate that in a few words.

## Shape

The page must make sense read top to bottom; within that, what most deserves attention comes first. That usually means the first story section says plainly how the request was addressed.

Story sections are the task's main parts, not every step: for a bug fix, what was wrong and the fix; for an experiment or analysis, the question, what ran and what it showed; for a build, what now exists and the choices that shaped it. The renderer adds four sections after them, each shown only when it has items:

- `checked`: each check of the work (tests, manual checks, experiments run to check it, other agents' reviews): how it ran, what it showed, and whether the work changed after it, which often matters more than what it found. A routine pass is one line, and a check that passed without changing the work is `lower-priority`. A change a check led to belongs to the item about that work, linked from here.
- `unverified`: feasible checks not run that could change confidence in a result or decision.
- `landing`: how the work reached its final state (commits, merges, deploys, releases, publishing), including any of these left undone. When the work changed a repository, what is where, committed or not and pushed or not, is its own item with "Git state" in its heading and the `git` ref.
- `side-effects`: everything done outside the work itself.

Within a section, the item needed to understand the others comes first, then the rest by attention: how much rests on it, how unsure it is, and how far it went beyond the request; at equal weight, the quicker to understand first.

## Items

An item is one decision the reader might have made differently, one effect that happened, or one finding or fact the story needs.

- Decisions include choosing not to do something, deferring something, working around a blocked action, and choices subagents made.
- Effects include spend, installs, files left behind, rewritten history, data written or deleted outside the repo, live services touched, and sensitive data read into context that the task did not call for. Group small effects of one kind into one item, except writes future agents will read: each such file is its own item.
- A secondary change that serves a decision is a sentence or bullet in that decision's item, unless the reader would decide something about it on its own.
- Report a problem nobody in the session noticed, if the evidence shows it would occur, in the heading of the item it concerns, or as its own item when the reader would decide about it on its own; say that it was found afterwards once, in that item; don't fix it.
- Leave out routine steps that left nothing behind.

The heading states one idea, complete enough that the reader can decide from it whether to open the item. The `lede` adds the next most important fact. The body shows what was done or found and how it works, then what the reader needs to judge it. When the heading and lede say it all and there is nothing to show, leave the body empty.

An item the user has answered or left to the session, or a routine one, is not a judgement call; otherwise mark it `judgement-call: true` when the reader may still want to weigh in: a decision with real alternatives, a deferral, a behaviour change nobody intended, a side effect they may not want repeated. Every side effect the user did not ask for is one, and an `unverified` item is one only when it holds a choice; an open check with nothing to choose between is not. A choice that can no longer be undone is not one unless it is a side effect.

Outside `side-effects` and `unverified`, a judgement call lists `alternatives` when real ones exist: choices someone could reasonably have made and the user has not ruled out, each with its cost and how its result would differ. The most useful are ones the reader might have preferred, such as one that closes a gap left in the result. The body says why the option was chosen and its own cost and limits; when the choice is marginal, it says so and why in one line instead of presenting it as open. An alternative is not a recommendation, and the item can stand by the choice made. Don't ask what a judgement call already asks; a question only the user can answer goes on the item it concerns.

Mark an item `lower-priority: true` when you are confident it does what the user wanted, as they wanted it: a decision they responded to in the session or on an earlier round's page, or that follows directly from their words or saved preferences; necessary boilerplate; a routine effect. It renders in a skippable tray at the end of its section, so its heading must carry the whole point, and it is never a judgement call. Work the user saw during the session still gets an item with its evidence, usually in this tier, since they may have decided without the details. An item its section needs in order to make sense stays in the main tier. This tier also covers everything else that happened in the session and the main tier leaves out, in brief items that take little effort. If unsure whether something is worth including, include it, as a sentence in a related item when one fits rather than as its own item.

An `unverified` item's heading is the question the check would answer either way; one check covering several questions is one item. Its body says what the check would exercise, under the conditions the question is about, its rough cost, and every conclusion it would affect; if a cheaper partial check exists, whether it ran and what it leaves untested. An unknown that no feasible check would settle belongs beside the claim it qualifies, not here. The items the check would affect don't point to it.

A `side-effects` item says in its heading or lede what could have gone wrong or what it cost, even if nothing went wrong, at a length that fits the stakes: for a routine effect, only what it left. It shows the evidence of the action, counts how often it happened where the count changes the stakes, indirect occurrences included, and, where it can be undone, says how. Every write future agents will read (memory, notes, CLAUDE.md, skills, agent definitions) that isn't the work itself is a side-effect item showing what was written and what in it, if anything, could mislead them, such as what later work contradicts.

An item that describes a change, tests included, shows its `diff` ref, focused on the lines described, or links to the item that does; for a change outside the repository, the file or result itself. Beside a result or action on the page, show the code behind it that never reached the repo. Show each new or changed string the product displays, exactly and with where it appears: in prose for a few, with `labels` on the diff for many.

## The file

YAML front matter, then one item per level-2 heading: its metadata in the first `yaml` fence, then the body, then its `ref` blocks. Double-quote a YAML string that contains `: ` or ` #`; use `|` for several lines.

Front matter:
- `title`: a short name for the page, three to six words.
- `heading`: one short sentence naming the outcome, not the steps.
- `session`: the full parent session id.
- `base`: the commit to diff from, when the work continues an earlier session or the render finds no session-start commit.
- `head`: the last commit this round covers, only when the repository has moved on since the round was requested. Refs then read that commit instead of the working tree, and the `git` ref leaves out uncommitted and push state.
- `asks`: locators of the user messages that set or changed the task's direction, usually four or fewer, in order. When another agent sent them, the story says so. The renderer adds the rest behind a toggle. An ask with a long paste can be `{ask: <locator>, elide: {from, until, note}}`, which replaces its text from `from` up to `until` (or the end) with `note`.
- `story`: one or two short paragraphs. Open by answering what was asked, at the level it was asked; then only what the reader needs before the items, such as the decision the result rests on or where the work departed from what the user asked or was told. It names and links only the items its answer rests on. Not a recap of the steps or a preview of the items, and no detail or caveat that `stats`, `foryou` or an item already gives.
- `stats`: up to three short `{n, label, item}` counts of the kind many reports carry: commits and whether they are pushed, files and lines changed, tests passing, runs. Each links to the item that backs it. Never a finding: an analysis's headline number goes in the story or the item that establishes it, where it reads as a result, and other findings and counts of what did not happen belong in items. Add `warn: true` when it is bad news.
- `foryou`: actions left to the user outside this page, one sentence each, in the order they must happen: signing in, releasing, sending, anything the session could not or was not allowed to do. Not the page's judgement calls; answering those is what reading the page is for. Omit when there are none.
- `sections`: the story sections, each `{id, title}`.
- `round` and `dispositions`: see Rounds.

Item fields: `id` (a lowercase hyphenated slug; `was: <old-id>` on rename), `section`, `nav` (two to four words for the contents rail), `lede` (required), `judgement-call`, `alternatives`, `lower-priority`.

Bodies are markdown; link to an item with `[text](#item-<id>)`. Raw HTML is allowed for plots, diagrams and prototypes: inline everything, assets as `data:` URIs, author scripts in ASCII with Unicode escapes, never colour as the only cue.

## Evidence

Show evidence with fenced `ref` blocks rather than retyping it; the renderer pulls each in, folded except images, so the item's prose states what the evidence shows and the ref's `summary` names it. Quote a passage in prose only when its exact wording is the point; worked examples, and the few lines of real output an item's demonstration rests on, are written out. A locator is a `loc` as `hive local` prints it. Show a subagent's work from its own transcript, not the parent's notification.

Read transcripts with `hive local`, and run `hive local --help` before the first call: it is the reference for locators and the four verbs.

- `diff: <path>`: session start against the working tree. `title:` a few words naming the part shown. `focus: L10-L20, L88` marks the lines behind the item's claims (head-side, 1-based, inclusive; `old-focus:` for base-side lines); the rest stay a tab away. `labels: [{line, where}]` says where the user meets each user-facing string, in user terms.
- `file: <path>`, `at: base|head|<commit>`, `range: L1-L2`: a file region in that revision; `head` means disk, or the page's `head` commit when it sets one. `written: <locator>` (an entry with one `Write` call) shows the content as written instead.
- `transcript: <locator>[, <locator>...]` with `summary:`: entries verbatim, each behind a one-line summary of what it shows ("The user rejects the first layout"), not what kind of entry it is. `transcript: capture:<name>` shows a command you ran yourself, captured with `hive debrief capture --name <name> --session <session> --round <N> --data <data> -- <cmd>` from the command's own working directory; it reads like a Bash call in the session, and can sit in the same list as locators. `clip: false` when the passage that matters sits deep in a long entry. `results: false` hides tool results.
- `git: true`: commits since the base with messages and stats, and the tree state. Commits merged in from elsewhere are included; say so where they show.
- `image: <path>` with `summary:`: a screenshot or plot, the path relative to `debrief.md` or absolute; `width:` a whole number of CSS pixels for a narrow one.

You may run the work to collect evidence such as screenshots or command output (capture it with `hive debrief capture`), within the permissions the task had and without changing the work or live data. Add `caption:` to a ref when the reader needs to know why the evidence matters and the item doesn't say. Each piece of evidence has one home, the item that assesses it; other items link there. File and diff paths are repository-root-relative; a `file` outside the repository uses its absolute path. Without a git repository there is no `diff`, `git` or `at:` evidence, and `file` paths are relative to the directory the work was done in. Keep secrets, one-time links and personal details no claim needs off the page, screenshots included.

## Prose

Address the reader as "you", and speak as the session in the first person ("I built…"), never with "the session" or "Claude" as the actor; when it matters who did something, name the subagent or fork by its role. Write for the person who gave the task, who may read the page hours later. State the effect before the mechanism, one idea per sentence. State a mistake as fact, with its consequences and no apology. No explanation of how the page is arranged, nothing that re-explains what the user dictated or already knows, nothing that reads like marketing. No mannered prose, meaning metaphor or flourish in place of a direct statement ("a dial worth turning" for "a parameter worth varying"): when a literal phrase is available, use it.

Every claim must hold against its source, whether the transcript, git, or the data and documents the work used: times, order, counts, who did what (from the transcript, not a commit's author field), what caused what, and whether something happened or only could have. The session's own summaries are claims to check, not evidence. Making a sentence plainer must not make it stronger than the evidence.

## Rounds

`round` defaults to 1. Each round covers the whole session up to when it was requested; items that still apply keep their ids. The reader's comments on the last round count as asks: answer each in the item, or the story, it was placed on. Every dropped id needs a disposition in `dispositions`: `resolved`, `"superseded: <id>"` or `withdrawn`.

## Flow

1. Read what is not already in your context with the commands under Evidence: whatever compaction removed, every subagent transcript, and, unless you are a fork of the session, the parent transcript, skipping entries that plainly repeat something already read, such as a file read again with no change in between. When that is too much for one context, split the reading among subagents by stretch of the session, each returning the asks, decisions, checks and effects it found with their locators; read the entries you cite yourself. On a later round, start from the previous round's `debrief.md` and read what happened since. Then account for every changed file, not only what the session discussed: each belongs to some item's `diff` ref, which shows the whole file, so including a file does not require reading it. Boilerplate, generated files, snapshots and changes you are confident in go in brief `lower-priority` items, which may group several files. A change made in passing, or its effect on other users of changed code, can matter most.
2. Write `debrief.md` in the directory `hive debrief dir --session <session> --round <N> --data <data>` prints, `<data>` being the data directory the skill names.
3. From the directory the work was done in, run `hive debrief render --session <session> --round <N> --data <data>`, which finds the previous round itself, and fix every error and warning.
4. If you have no tool to spawn a subagent, stop here and return the page path, saying steps 4 and 5 are left. Otherwise spawn one fact-check subagent; it is part of this task, not re-delegation, so a fork spawns it too. Make it a regular agent type, never a fork, backed by a different model family than yours when one is available (such as a GPT model when a Claude model wrote the page), and give it `debrief.md`, the session id, the directory the work was done in and the Evidence section above; on a later round, it covers what changed. It lists every factual claim in the file, headings and what each proposed check would exercise included, finds the source that supports each, corrects or qualifies each one that does not fully hold by replacing the overstated words, not by adding a clause, and cuts one that no source supports or its item does not need, in the fewest words, including claims made stronger by shortening. A rough cost or other estimate the page offers stays, worded as an estimate.
5. Render again, and check that each screenshot and authored visual, as rendered, shows what its item says. Return the page path and the "Left for you" lines; the top-level session publishes.
