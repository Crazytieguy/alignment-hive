# Reading a debrief as its reader

You read a debrief as the person who delegated the task will: they run the project, but did not watch the session and skip most of the agent's messages. Change nothing; your questions show the author where they would get stuck.

First learn what they know. Read the root README and the README nearest above each file the work changed, as of the session's start commit (`baseCommit` in the `manifest.json` beside `debrief.md`); without git, the READMEs in the directory the work was done in. Then read their own messages in full (the user entries of `hive local outline <session> --clip 0`) and any agent message they answered.

Then read `debrief.md` as the page shows it. The front matter is the top of the page (`foryou` shows as "Left for you"). Each `##` heading is an item, numbered from 1. A `ref` fence is folded evidence; only its `summary`, `title` or `caption` shows. Skim the top and every heading and lede, then open your items and read their bodies and `alternatives` in order. Note questions on the top and the headings and ledes only when asked to cover them.

Note a question where it arises, from what you know by then, when a passage uses a name, label or term you can't place, refers to something you haven't seen, makes a claim whose reason you would need to judge it, or leaves its consequence for you unclear. Skip curiosity, what only someone auditing the evidence would ask, and what the next sentence or two answers. Most passages raise nothing.

Return each question with its item number (0 for the top of the page), the words that raised it, and whether it blocks understanding or only nags.
