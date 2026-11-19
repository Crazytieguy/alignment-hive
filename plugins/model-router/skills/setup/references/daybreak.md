# Daybreak (OpenAI Trusted Access for Cyber)

Daybreak is OpenAI's program for approved defenders: fewer cyber refusals
on the models an approval covers. model-router can select Daybreak, or
standard safeguards, on the GPT requests it sends. It grants nothing:
OpenAI must already have approved the user's ChatGPT account or API
project
([overview and how to apply](https://help.openai.com/en/articles/20001258)).
Raise it only with a user whose account has that approval. When the user
says they have it and doctor shows no sign of it, go on anyway: the
account's catalog isn't proof either way. For a user without it, link
the overview, and mention that `gpt-6-sol` is the GPT route to try for
legitimate security work the newer models refuse (the
`delegating-to-models` skill's `references/safeguards.md`).

What an approval covers, by routing ID (that overview, 2026-11-18):

- **Daybreak Blue**: `gpt-6-luna`, `gpt-6-sol` and, of the earlier models,
  `gpt-5.6-sol` (the overview names no other built-in route). Of the
  shipped agents and `/model` rows only Luna's is among them; the others
  are built-in routes reached by routing ID.
- **Daybreak Red**: those, and `gpt-6.1-sol`, `gpt-6.1-terra`,
  `gpt-6-astra`, `gpt-6.2-sol` and `gpt-6.2-astra`. Red's cyber-specialised
  models (GPT-5.6 Cyber, GPT-6 Cyber) can't be routed.

Blue and Red are approval levels: which one the user has decides which
routes are covered. The program a request selects has its own name, and
for every route listed above it is `daybreak_blue`, on a Red account
too; that is the name doctor and the config use. So doctor's line
doesn't name the level: ask the user which they have rather than assume.

## Tell the user first

Before asking what they want, tell the user:

- **Right now.** While `[gpt] daybreak` is unset, the router selects
  neither on a route with no `cyber-access` of its own, and OpenAI
  decides. On an API project OpenAI then applies
  Daybreak by itself where the project has the access the model needs:
  to `gpt-6-sol` and `gpt-6-luna` (Blue or Red) and, of Red's, only to
  `gpt-6-astra` and `gpt-6.1-sol`; `gpt-6.1-terra` and the 6.2 routes get
  it only when the request selects it. OpenAI doesn't say what a ChatGPT
  sign-in gets. So their requests through the router may already be
  Daybreak sessions.
- **Data use.** On a personal ChatGPT account (Free, Plus, Pro), OpenAI
  may use the content of Daybreak sessions to improve its models even when
  "Improve the model for everyone" is off; it says this helps it
  strengthen safeguards against cyber misuse
  ([Data controls FAQ](https://help.openai.com/en/articles/7730893-data-controls-faq)).
  Assume that covers the requests the router sends with Daybreak
  selected, and any OpenAI already applies it to ("Right now"). OpenAI
  says this of personal accounts only, not of Business,
  Enterprise or Edu workspaces or API projects; ask which theirs is when
  you don't know. Point the user at the FAQ for the current terms.
- **Scope of the approval.** OpenAI approves Daybreak for a specific
  identity, workspace or API project, model and product surface, and says
  to use it only there
  ([Models and Trusted Access](https://learn.chatgpt.com/docs/cyber-safety)).
  model-router sends requests under the user's own Codex sign-in or API
  key through CLIProxyAPI, which presents itself to OpenAI as Codex.
  OpenAI's pages don't say whether that counts as the approved surface,
  so it is for the user to judge. For enterprise access (an
  organization's workspace or API project), pass on OpenAI's advice: when
  the approved surface is unclear, stop and confirm it with their OpenAI
  representative first.
- **Untested with an approved account.** The mechanism was tested on a
  ChatGPT subscription and on an API key, neither with Daybreak: OpenAI
  accepted the `standard` program, and refused a Daybreak one on the key.
  Nobody has selected a Daybreak program through the router on an account
  that has one. Ask them to report how it goes: `/hive:feedback` with the
  hive plugin, or an issue on github.com/Crazytieguy/alignment-hive.
- **What it covers.** With Blue, of the shipped agents only Luna's; with
  Red, all of them. Where OpenAI already applies Daybreak ("Right now"),
  choosing Daybreak may change nothing, and choosing standard safeguards
  is what changes those routes.

Then ask: Daybreak, or standard safeguards? (Or confirm the choice they
already gave.) Leaving `[gpt] daybreak` unset is not a way to decline, as
"Right now" says. Write the answer either way: while `daybreak` is unset
and the catalog lists Daybreak, setup asks again and the session-start
row comes back.

On an API key, before writing Daybreak: OpenAI's API needs the approved
organization and an enabled non-default project, and a key issued by
that project. An organization owner enables it under Project settings →
General → Daybreak model access; the change can take about 15 minutes
([OpenAI's troubleshooting](https://help.openai.com/en/articles/20001259)).
Ask whether that is done, and have them do it first if not.

The keys below need a router that knows them: while `$ROUTER doctor`
fails `router-version`, run `$ROUTER service refresh` first. An older
router refuses a config that has one and doesn't start. They go under
the `[gpt]` table of `~/.config/model-router/config.toml`: create the
file when there is none, and when it already has that table (an API-key
install does), add the key there rather than a second `[gpt]` header.

## Standard safeguards

With either sign-in:

```toml
[gpt]
daybreak = false
```

Every GPT request the router sends then selects standard safeguards,
except on a route with a `cyber-access` of its own (below). Standard
safeguards don't by themselves opt out of training: on a personal
ChatGPT account "Improve the model for everyone" then decides, as for
the user's other activity, and other accounts and API projects have
their own terms. Tell a user who chose this for privacy to check theirs.

## Daybreak

With either sign-in:

```toml
[gpt]
daybreak = true
```

Each GPT request then selects the Daybreak program the account's own
Codex model catalog lists for its model, as Codex's Daybreak setting
does. A model it lists none for selects nothing, so with Blue alone only
the Blue routes above change. The router reads that catalog from
OpenAI's Codex backend (chatgpt.com) in both cases: with the Codex
login, or with the API key under `auth = "api-key"`. Doctor calls it
"the Codex plan" for a subscription and "the API key's model catalog"
for a key.

## One route's own program

A route's `cyber-access` names its program outright and wins over
`daybreak` for that route. Use it to hold one route at standard
safeguards, or for an account whose catalog doesn't list a program
OpenAI has approved (see "Apply and check"):

```toml
[[model-overrides]]
routing-id = "gpt-6.1-terra"
cyber-access = "daybreak_blue"
```

An entry sets only the keys it names, on the built-in route its
`routing-id` names; when that route already has an entry, add the key to
it. A `-fast` or `-ultrafast` route selects what its base
route does. The values are OpenAI's: `daybreak_blue` for every route
listed above, and `standard`. `daybreak_red` is for the cyber-specialised
models only, so no built-in route takes it
([OpenAI's guide](https://developers.openai.com/api/docs/guides/daybreak)).
Leave out routes the approval doesn't cover: their requests would fail.

## Apply and check

Run `$ROUTER doctor` first. With `daybreak = true` it reads the catalog
on every run, so it says at once whether the catalog lists Daybreak. If
it warns that the catalog lists no Daybreak program, go to the next
paragraph before restarting. Otherwise `$ROUTER service restart`, then
`$ROUTER doctor` again; no new sign-in and no Claude Code restart is
needed. Doctor's `daybreak` line names what each route selects. To try
it, send a request to a route that now selects Daybreak, for example
`claude -p 'reply with ok' --model gpt-6-luna`.

When doctor warns that the catalog lists no Daybreak program for any
routed model, `daybreak = true` selects none, and only a route's own
`cyber-access` still does. On an API key OpenAI may still apply Daybreak
by itself ("Right now"); the warning is about what the router selects.
The approval may be on another ChatGPT account, workspace or API project
than the one the router signs in with, or not provisioned or enabled
yet; and when several Codex logins are signed in (the `codex-*.json`
files in `~/.local/state/model-router/codex-auth/`), all of them need it.
The catalog isn't proof either way
([OpenAI's troubleshooting](https://help.openai.com/en/articles/20001259)).
To test directly, set `cyber-access = "daybreak_blue"` on one covered
route, restart the service, and send it a request.

- If that works, add an entry for each route the approval covers and
  remove `daybreak = true`, which the catalog isn't serving. Setup asks
  again only if the catalog later lists Daybreak for a route without an
  entry.
- If it fails, remove the entry and tell the user what OpenAI's message
  said (below). Then ask whether to keep `daybreak = true`, which picks
  Daybreak up once the catalog lists it and has doctor warn until then,
  or to write `daybreak = false`.

## When a request fails

A request that selects a Daybreak program the model or the account
doesn't have fails; OpenAI doesn't fall back to standard safeguards.
Claude Code shows OpenAI's message, which says which it is: the account
or project doesn't have Daybreak ("You do not have Daybreak access. ...
apply for Daybreak access via ...", with a link), the model takes a
different program (it names it), or the model has none. Tell the user
what it said, link included, and suggest they report it (as above): the
approved path is untested. A failing auto-mode classifier call doesn't
show the message: the classifier stays out or reports the call
unavailable, and the router's log
(`~/.local/state/model-router/logs/router.log`) has the refusal's
status, and for a 400 the start of OpenAI's message.

What selected the program decides what to do: the route's own
`cyber-access` when it has one (on the base route, for a `-fast` or
`-ultrafast` one), else `[gpt] daybreak`. Ask before moving a route the
user chose Daybreak for to standard safeguards.

- **A `cyber-access` entry on the route**: set it to the value OpenAI
  names, or remove it when OpenAI names none; the route then goes by
  `[gpt] daybreak` like the rest.
- **`daybreak = true`**: the catalog listed a program that OpenAI then
  refused. Run `$ROUTER doctor`: it reads the catalog again, and the
  running service goes by that reading with no restart. Then retry the
  request. If it still fails, set `cyber-access = "standard"` on the
  route, or `daybreak = false`, which covers every route without an entry
  of its own.
- **`daybreak = false`**: the router asked for standard safeguards
  outright, so the error isn't from these settings; report it.
- **`daybreak` unset**: the router selected nothing and OpenAI chose;
  `daybreak = false` makes the router ask for standard safeguards.

After a config change, apply and check as above.
