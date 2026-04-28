---
name: testito
description: Use this skill when running manual or automated tests where you (the agent) execute steps and need to log a structured pass/fail/warning record per step. Wraps the `testito` CLI to write each step result, attempt number, and any notes to a local SQLite log that the user views in a web dashboard. Trigger when the user says "run these tests", "verify the X flow", "QA the build", or asks you to record what you tested.
---

# testito — step-by-step test reporting

You are testing something on behalf of a human, and they want a structured log of what you did and what passed or failed. As you go, call the `testito` CLI to append each step result to a named **run**. The user opens a local web dashboard to watch the log fill up live.

## When to use

- The user asks you to test, verify, QA, smoke-test, or regression-check something.
- Any session where you'll execute a sequence of human-language verification steps and the user wants a clean record afterward.

Do not use this skill for:
- Running automated unit tests (`cargo test`, `pytest`, etc.) — those have their own reporters.
- Just describing what should be tested without actually doing it.

## The mental model

Three nouns:
- **Run** — one testing session. Identified by a name like `auth-smoke-2026-04-28`. Use a stable, descriptive name; same name = same run (resumable across messages).
- **Test** — a logical grouping of steps within a run. Phrase it as the user-facing scenario: `"check login flow with correct credentials"`, not `"login_test_1"`.
- **Step** — one concrete action or verification you perform. One step per `testito report` call. Examples: `"input email and password and click Sign in"`, `"dashboard loads within 2 seconds"`.

Steps are **append-only**. If you retry a step, log it again with `--attempt 2`. The log shows what actually happened, in order.

## CLI commands

```
testito start --run "<name>" [--description "..."] [METADATA…]
    Optional. Auto-created on first `report` if you skip this. Use it to attach
    a description and run-level metadata.

testito report --run "<name>" --test "<scenario>" --step "<action>"
              --result <pass|fail|warning|skipped> [--attempt N] [--note "..."] [METADATA…]
    The main verb. Call this once per step as you go.

testito note --run "<name>" --scope <in|out> --text "..."
    Append a free-text observation to the run. Markdown is rendered in the dashboard.
    Use scope=in for findings within the testing brief (e.g. "login is slow on first
    request"); scope=out for things you noticed that weren't asked about (e.g.
    "dashboard footer has a typo"). Default scope is in.

testito end --run "<name>"
    Mark the run completed. Do this when you finish the session.
```

**Run metadata** (`[METADATA…]`) — pass on `start` or any `report`. Each non-empty value
is upserted on the run; subsequent calls don't overwrite unless you pass them again.
The dashboard shows them in a banner so a reviewer knows *what* was tested.

```
--branch  <name>       e.g. main, feature/checkout
--commit  <sha>        e.g. abc1234 (full or short)
--env     <label>      e.g. local, staging, prod
--url     <origin>     e.g. http://localhost:3000
```

Help: `testito --help`, `testito report --help`, etc.

## How to test

1. **Pick a run name up front.** Combine purpose + date or build, e.g. `checkout-smoke-2026-04-28`. Use the **same name** for every command in the session.
2. **Capture metadata up front.** Either via `testito start --run "<name>" --description "..." --branch ... --commit ... --env ... --url ...`, or pass `--branch/--commit/--env/--url` on the first `report`. The dashboard shows these prominently — they're how a reviewer knows what was tested.
3. **For each step you take, immediately call** `testito report ... --result <pass|fail|warning|skipped>`:
   - Use `pass` when the verification succeeded.
   - Use `fail` when the expected behavior didn't happen.
   - Use `warning` when something worked but had a smell — slow, ugly, partially right.
   - Use `skipped` when you couldn't run the step (preconditions missing, blocked by an earlier failure).
   - Add `--note "..."` for failures or warnings to capture *what* went wrong (error message, screenshot path, observed values).
4. **Notes are rendered as markdown.** Use code fences for tracebacks, backticks for paths/commands, links for URLs. Multi-line notes work — quote the whole thing in a single shell argument. Example: `--note "Failed: $(cat /tmp/err.log)"` or with explicit fences:
   ```
   --note $'```\nUncaught TypeError: Cannot read properties of undefined\n  at handleSubmit (login.ts:42)\n```'
   ```
5. **If you retry a step** (e.g. it failed once, you fixed something, you're trying again), log the second attempt with `--attempt 2` (and so on). Don't overwrite — re-call `report` with the same `--test` and `--step` and a higher `--attempt`. The dashboard groups attempts under the same step.
6. **Use `testito note` for anything outside the step grain.** Markdown is rendered. Findings unrelated to the current test, environmental observations, things to follow up on.
7. **At the end**, call `testito end --run "<name>"`.
8. Tell the user the run name and (if `testito serve` is running) the dashboard URL: `http://127.0.0.1:7878/runs/<id>` — they can also export the run as markdown via the **Download .md** button or compare it against any other run via **Compare with…**.

## Result-vocabulary discipline

- **`pass`**: the step's expected outcome happened. Don't use pass for "kind of worked".
- **`fail`**: the expected outcome did not happen. Always pair with `--note` describing the symptom.
- **`warning`**: it worked but with a caveat (e.g. unexpected delay, layout issue, console warning). Pair with `--note`.
- **`skipped`**: you didn't run the step. Pair with `--note` saying why.

If you're tempted to invent new categories, use a `testito note --scope in` instead.

## Phrasing guidance

- **Test names** should describe the user-facing scenario in plain language. Good: `"create a customer with valid required fields"`. Bad: `"test_customer_create_valid"`, `"happy path"`.
- **Step names** should describe one observable action or assertion. Good: `"submit the form and confirm a 'created' toast appears"`. Bad: `"step 1"`, `"check stuff"`.
- Reuse exactly the same `--test` string when reporting multiple steps under the same scenario — the dashboard groups by it.

## Worked example

```bash
RUN=auth-smoke-2026-04-28

# kicking off — capture metadata so the reviewer knows what was tested
testito start --run "$RUN" \
  --description "Login + password reset on staging" \
  --branch main --commit abc1234 --env staging --url https://staging.example.com

# Test 1
testito report --run "$RUN" \
  --test "login with valid credentials" \
  --step "navigate to /login" --result pass

testito report --run "$RUN" \
  --test "login with valid credentials" \
  --step "enter valid email and password and click Sign in" --result pass

testito report --run "$RUN" \
  --test "login with valid credentials" \
  --step "redirected to /dashboard within 2s" --result warning \
  --note "Took ~3.5s on first request. Subsequent loads were ~400ms."

# Test 2 — failure + retry pattern, with a code-block in the note
testito report --run "$RUN" \
  --test "password reset email arrives" \
  --step "submit reset form with registered email" --result pass

testito report --run "$RUN" \
  --test "password reset email arrives" \
  --step "reset email arrives within 30s" --result fail \
  --note $'Waited 90s, no email.\n\n```\nWorker log: redis: connection refused (172.18.0.4:6379)\n```'

# Cleared a stuck queue, retrying
testito report --run "$RUN" \
  --test "password reset email arrives" \
  --step "reset email arrives within 30s" --result pass --attempt 2 \
  --note "Arrived in ~12s after worker restart."

# Out-of-scope finding (markdown supported)
testito note --run "$RUN" --scope out \
  --text "Footer copyright still says **2024** on the login page."

# Done
testito end --run "$RUN"
```

## What the user sees

The user runs `testito` (or `testito serve`) in a separate terminal. The dashboard at `http://127.0.0.1:7878/` lists every run; opening one shows tests grouped by name with each step's status, attempt number, note, and timestamp, plus a separate Notes section with in-scope and out-of-scope items. The page polls every 2 seconds, so your reports appear as you go.

## Common mistakes to avoid

- **Don't batch** — call `testito report` after each step, not at the end. The point is a live log.
- **Don't pre-write the steps** — the agent should report what *actually* happened, in order, including ones that weren't planned.
- **Don't skip `--note`** on failures and warnings. The single line of context is what makes the log useful for the human reviewer.
- **Don't rename the test mid-session** — pick the `--test "..."` string once and reuse it verbatim for all steps in that scenario.
