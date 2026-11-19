# Gemini (Google) via API key

Google's Gemini 4 models on the user's own Google AI Studio API key,
billed per token by Google. Off by default.

Say only what a step asks for; the last section is for questions the
user asks.

## Steps

1. **Key.** Have the user create a key at
   <https://aistudio.google.com/api-keys> and run `<ROUTER> set-key
   gemini` in a separate terminal (give them `$ROUTER`'s expanded path),
   pasting the key at its hidden prompt; it checks the key with Google
   and saves it. Go on once it prints `Saved to`; otherwise nothing was
   saved, and they run it again (with a corrected key if it said the key
   is invalid).
2. **Enable.** Add to `~/.config/model-router/config.toml`:
   ```toml
   [gemini]
   enabled = true
   ```
   Then `$ROUTER service restart` and `$ROUTER doctor`.
3. **Billing.** Go by doctor's `gemini-billing` line:
   - `paid`: go on.
   - `free`: the key's project has no billing, so only Hydrogen can
     answer, on Google's free tier. Tell the user, and ask which they
     want:
     - Billing on the project, at <https://aistudio.google.com/billing>.
       Recommend this one.
     - Hydrogen alone on the free tier, without web search. Outside the
       EEA, Switzerland and the UK, Google's
       [terms](https://aistudio.google.com/docs/terms) say it uses
       free-tier prompts and responses to improve its products, human
       reviewers may read them, and nothing sensitive or confidential
       should be sent there.

     On billing, run doctor again once they've set it up. On an
     explicit yes to the free tier, add `allow-free-tier = true` under
     `[gemini]` in `config.toml`, restart the service, and in the steps
     below use `gemini-4-hydrogen` alone, wherever they name Neon.
   - `billed-but-empty` or `blocked`: tell the user what the line says
     and its fix, and run doctor again once they've done it.
   - `unknown`: the line says why. For an invalid key, the user runs
     step 1 again; for anything else, run doctor again in a minute.
4. **Smoke test** through the gateway, `<base_url>` being `.base_url` in
   `$ROUTER doctor --json`:
   ```
   curl -s <base_url>/v1/messages \
     -H 'content-type: application/json' -H 'anthropic-version: 2023-06-01' \
     -d '{"model":"gemini-4-neon","max_tokens":300,"messages":[{"role":"user","content":"reply with exactly: ok"}]}'
   ```
   A reply of `ok` means it works. On an error, show the user its message
   and stop.
5. **Context window**: run `$ROUTER settings windows --apply
   --project-dir <project>` as in setup step 5, then `$ROUTER doctor`,
   and apply any fix its `context-windows` line gives for a Gemini
   route.
6. **`/model` picker row and agent** for Neon, as setup step 5 and
   `references/custom-agents.md` describe for optional routes:
   ```json
   { "model": "gemini-4-neon", "label": "Gemini 4 Neon" }
   ```
   Add the others (`gemini-4-argon` "Gemini 4 Argon", `gemini-4-carbon`
   "Gemini 4 Carbon", `gemini-4-hydrogen` "Gemini 4 Hydrogen") only if
   the user asks for them.
7. Tell the user that after restarting Claude Code they can pick Gemini
   in `/model`, or ask Claude to hand work to a Gemini agent.

## If the user asks

- **To use a Google subscription (Google AI Pro or Ultra, Antigravity,
  Gemini CLI) instead of a key**: Google's
  [Antigravity FAQ](https://antigravity.google/docs/faq) says using
  third-party software such as Claude Code with an Antigravity login
  violates its terms and "can result in suspension or termination of
  your account", and recommends an API key instead; users have reported
  suspensions. So model-router takes an API key only. Pro and Ultra
  include [monthly developer credits](https://developers.google.com/profile/help/benefits)
  ($10 on Pro, $40 on Ultra 5x, $100 on Ultra 20x) that apply to their
  own key's usage after a Prepay purchase (at least $5).
- **What it costs**: per million tokens of input and of output (thinking
  is billed as output), on Google's
  [pricing page](https://aistudio.google.com/docs/pricing) on November
  18, 2026: Neon $3 and $15; Argon and Carbon $2 and $10 through
  November 30, then $4 and $20; Hydrogen $1.50 and $7.50. Google caches
  repeated input automatically when it can, and a cache hit costs a
  twentieth of the input price. A Gemini agent's WebSearch runs on
  Google Search (paid tier only), which bills each query past 5,000 a
  month at $14 per 1,000. A
  monthly spend cap can be set on AI Studio's
  [Spend](https://aistudio.google.com/spend) page; Google says it can lag
  by about ten minutes. A new billing account can spend $10 per rolling
  10 minutes (Google's
  [Tier 1](https://aistudio.google.com/docs/rate-limits)), which a few
  large-context agents in parallel can reach.
- **What Google keeps**: on the paid tier, Google's
  [terms](https://aistudio.google.com/docs/terms) say prompts and
  responses are not used to improve its products, but Google
  [retains them for 55 days](https://aistudio.google.com/docs/usage-policies)
  to detect abuse, and authorized employees may read flagged content. A
  project created on or after 2026-10-26 also
  [stores request logs](https://aistudio.google.com/docs/logs-policy)
  for its owner by default, for up to 55 days; the owner can turn that
  off on AI Studio's Logs page. One exception to the paid terms: a
  project the user enrolled in Google's invite-only
  [Contributor Tier](https://aistudio.google.com/docs/logs-policy#contributor-tier)
  is billed at a discount while its Argon and Hydrogen data falls under
  the free tier's terms.
- **About a refused Hydrogen request** (`model-router refused this
  Gemini 4 Hydrogen request`): run `$ROUTER doctor` and go by step 3. A
  project that runs out of prepaid credit gets an error from Google,
  never the free tier.
- **To turn billing on after using the free tier**: remove
  `allow-free-tier`, restart the service, run doctor until
  `gemini-billing` says `paid`, then add Neon as in step 6.
- **To remove Gemini** (or at full uninstall): delete the `[gemini]`
  blocks in `config.toml` and `secrets.toml`, then `$ROUTER service
  restart`. Delete the Gemini rows from `modelPicker` (or the
  single-slot env pair, if it names a Gemini ID) in the wired settings
  file, a `model` setting naming a Gemini route, and the Gemini agents. The key stays valid until the user
  deletes it in AI Studio.
