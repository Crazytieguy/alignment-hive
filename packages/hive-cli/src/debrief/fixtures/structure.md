---
title: Rain-aware watering
heading: Why the garden was watered in the rain, and the fix that landed
session: 22222222-2222-4222-8222-222222222222
base: 1a2b3c4
head: 5d6e7f8
asks: [22222222:1, "22222222-2222-4222-8222-222222222222:40", 22222:52, 22222222:90]
stats:
  - { n: "1", label: "commit on main, not pushed", warn: true, item: git }
  - { n: "4", label: "files, +120 −30", item: git }
  - { n: "42", label: "tests pass, lint clean", item: tests-lint }
story: |
  The scheduler read yesterday's forecast, so a rainy morning still ran the sprinklers.

  It now reads the forecast at start time and skips a run when rain is likely.
foryou:
  - "Run `water status` once to confirm the new schedule."
  - "The fix is on main but unreleased."
sections:
  - { id: wrong, title: What was wrong }
  - { id: fix, title: The fix }
---

## The scheduler used a cached forecast from the day before

```yaml
id: diagnosis
nav: Why it watered
section: wrong
lede: "The cache was keyed by day, not by hour."
```

The cache outlived the forecast it held.

```ref
transcript: 22222222:12
summary: "The first report"
```

## The forecast is read when a run starts

```yaml
id: fresh-forecast
nav: Fresh forecast
section: fix
judgement-call: true
lede: "No cache at all."
alternatives:
  - "An hourly cache: fewer requests, but a stale hour can still water in the rain."
```

```ref
diff: src/schedule.ts
title: the start-time read
```

## `WATER_RAIN_LIMIT` sets how likely rain must be to skip

```yaml
id: rain-limit
nav: Rain limit
section: fix
judgement-call: true
lede: "Defaults to 60%."
alternatives:
  - "A fixed 50%, with no setting."
```

```ref
diff: src/config.ts
```

## A failed forecast request waters as scheduled

```yaml
id: fail-open
nav: Fail open
section: fix
judgement-call: true
lede: "Dry soil costs more than a wasted run."
alternatives:
  - "Skip on failure, and say so in `water status`."
  - "Retry once after a minute."
```

```ref
diff: src/forecast.ts
```

## One new message

```yaml
id: copy
nav: One new message
section: fix
judgement-call: true
lede: "Shown when a run is skipped."
alternatives:
  - "No message, only a log line."
```

<ul class="copy-pair">
<li><span class="where">Status line:</span><span class="str">skipped: rain likely</span></li>
</ul>

```ref
diff: src/messages.ts
```

## A second model reviewed the change

```yaml
id: model-review
section: checked
lede: "No findings."
```

```ref
transcript: 22222222:60
summary: "The review: no findings"
```

## Two dry runs against recorded forecasts

```yaml
id: dry-runs
section: checked
lede: "One rainy morning, one dry."
```

```ref
transcript: 22222222:70, 22222222:68, 22222222:69, 22222222:75
summary: ["The rainy run", "Its forecast", "The dry run", "Its forecast"]
```

## Tests and lint pass

```yaml
id: tests-lint
section: checked
lede: "42 tests."
```

```ref
transcript: 22222222:80
summary: "The test run"
```

## A real rainy morning was not observed

```yaml
id: live-unverified
section: unverified
judgement-call: true
lede: "The dry runs used recorded forecasts."
```

It needs a morning with rain in the forecast.

## Rebased over the timezone fix

```yaml
id: rebase
section: landing
lede: "No conflicts."
```

```ref
transcript: 22222222:85
summary: "The rebase"
```

## No version bump

```yaml
id: version-bump
section: landing
judgement-call: true
lede: "Left for the release."
alternatives:
  - "Bump the patch version in the landing commit."
```

## Final git state

```yaml
id: git
section: landing
lede: "One commit on main."
```

```ref
git: true
```

## A forecast request with the real API key

```yaml
id: live-request
section: side-effects
judgement-call: true
lede: "One request, read-only."
```

```ref
transcript: 22222222:30
summary: "The request"
```

## A notes file written

```yaml
id: notes-write
section: side-effects
judgement-call: true
lede: "Where the forecast API's limits are written down."
```

```ref
file: notes/forecast.md
written: 22222222:35
```

## Temp scripts

```yaml
id: temp-files
section: side-effects
lede: "Removed afterwards."
```
