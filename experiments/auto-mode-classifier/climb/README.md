# Classifier prompt tuning (offline replay)

Hill-climbs the router's classifier prompt toward Claude Code's local Sonnet classifier (arm S) without running Claude Code per candidate. Each dev case's classifier request, as the router sent it in an R run, is re-rendered with a candidate prompt, sent to a CLIProxyAPI, and the verdicts are scored against S, the adjudicated labels and the dataset labels. Usage is in the header of `climb.ts`; run it from `experiments/auto-mode-classifier` with `sh climb/run.sh <args>`, which unsets the session's proxy variables. Outputs and the response cache live in `~/.cache/auto-mode-classifier-bench/climb/`.

- **Replay fidelity.** A candidate's request is the captured body with the policy cut out and put into the candidate's template. The Rust test `the_prompt_tuning_fixture_is_what_the_router_sends` (`crates/model-router/src/auto_mode/classify.rs`) writes `src/testdata/auto-mode-climb-render.json`, and `lib/render.test.ts` checks the replay reproduces those bytes. A change to how the router builds the request needs both updated.
- **Only the prompt, model, effort, service tier and output format vary.** Changing what the classifier is shown needs a router change and a new R run.
- **Never the live router.** `run` refuses port 8787; point it only at a sandbox CLIProxyAPI. The test split loads only with `--allow-test`; tune on `tune` and look at `check` only to catch overfitting.
- **S is one Sonnet sample per case.** Agreement with S beyond S's own retest (S against S2) is fitting noise.
