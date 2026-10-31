# Automatic recall: the benchmark

The hive plugin's automatic recall (`plugins/hive/hooks/recall.ts`, `recall-text.ts`) was chosen and tuned on this benchmark: historical prompts from this project, replayed as of the moment they were typed, with what each variant handed the main agent scored against claims three judges rated. The debrief pages for the auto-retrieval work hold the results and the decisions.

Data lives outside the repo, in `~/.local/share/alignment-hive-auto-retrieval/` (`$BENCH`): the cases (`cases/<id>/prompt.txt` and `index.json`, mined in earlier rounds), every run (`runs/<case>/<variant>/`), the pooled entries, claims and verdicts (`claims/<case>/`), and analyses (`analysis/`).

## Replaying a moment

Every run sees transcripts as they stood when its prompt was typed: the bench's CLI (`$BENCH/bin5`, a build of `packages/hive-cli`) reads `HIVE_LOCAL_CUTOFF`, and its `git` (`git-at-cutoff.sh`) shows the repository as of then. Each `claude` the bench starts runs on a scratch config (`$BENCH/claude-config`) with no user settings, plugins, hooks or MCP servers.

## Scripts

From this folder:

```bash
bun recall5.ts recall8-sol,recall8-sonnet --par 6  # recall as the plugin runs it, per case -> runs/<case>/<variant>/
bun pool4.ts                                       # every entry any run cited, per case -> claims/<case>/pool.json
bun claims.ts increment all                        # claims in entries no extraction has read (Sol)
bun judge4.ts score-new fable,opus,sol all         # three blind judges on the new claims
bun judge4.ts merge all                            # -> claims/<case>/verdicts.json
BENCH_CASES=c01-d17 bun score4.ts --compare 'a:b'  # recall, precision, NONE rate, time, cost; paired bootstrap
BENCH_CASES=c01-d17 bun sweep5.ts [clips]          # no model calls: what recall's history lets a reader see
BENCH_CASES=c01-d17 bun gaps5.ts                   # what the retrieval agent delivers that recall does not, and why
bun cost5.ts 2026-09-21 2026-10-19 800 800000      # recall's cost over real use, with a simulated prompt cache
```

`corpus4.ts` writes `$BENCH/corpus4.jsonl` (`CORPUS_CLIP=0` for unclipped text), the project's messages and replies that `corpus5.ts` turns into the CLI's rows for the sweep and the cost simulation.

Effort: the bench calls `claude -p --effort low`, which sends adaptive thinking with the effort, so Sol and Sonnet ran at low. The plugin's `$.model.complete` sends the effort with no thinking block: Sonnet 5.5 thinks adaptively without one, so it matched the bench all along; Sol ran at medium until model-router pinned CLIProxyAPI 8.0.44, which reads the effort without one.

Scoring: a judged claim counts as delivered when an excerpt the main agent received contains its evidence span (`credit.ts`); recall is the share of a case's relevant judged value delivered, averaged over cases. `BENCH_CASES` limits scoring to the cases whose claims were fully judged.
