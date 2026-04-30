---
name: testito
description: Use this skill when running tests OR producing a code/security/perf review on a PR or branch — anything where the user wants the result logged as a structured artifact instead of dumped in chat. Wraps the `testito` CLI to write step results (pass/fail/warning/skipped), findings (bug/polish/question/info), and one-shot review verdicts (security/code/perf × clean/advisory/blocking) to a local SQLite log the user views in a web dashboard. Trigger on phrases like "run these tests", "verify the X flow", "QA the build", "smoke-test", "regression-check", "review this PR", "security review", "audit this diff", "code review", or any prompt that ends with "report to testito".
---

# testito — step-by-step test reporting

You are testing something on behalf of a human, and they want a structured log of what you did, what passed or failed, **and everything else you noticed along the way**. As you go, call the `testito` CLI to append each step result to a named **run** and to file every observation, including ones outside the brief. The user opens a local web dashboard to watch the log fill up live.

## When to use

- The user asks you to test, verify, QA, smoke-test, or regression-check something.
- Any session where you'll execute a sequence of human-language verification steps and the user wants a clean record afterward.

Do not use this skill for:
- Running automated unit tests (`cargo test`, `pytest`, etc.) — those have their own reporters.
- Just describing what should be tested without actually doing it.

## File every finding (this is the part agents get wrong)

While you test, you will see things outside the explicit brief — a typo, an unexpected console warning, a layout quirk, a slow page, a button that flashes the wrong state for half a second, copy that's confusing, a missing focus ring, a 404 in the network tab. **File those, even when nobody asked.** Reviewers cannot read your screen — if you don't write it down, it doesn't exist.

**The cost of silence is much higher than the cost of a tangential note.** Out-of-scope notes:
- do not affect the run's pass/fail status
- do not slow you down (`testito jot --run "<name>" --text "..." --kind <bug|polish|question|info>` is one command, run it as you go)
- do not pollute the test results — they live in a dedicated Findings & notes section
- can be filed even after the run is complete

The cost of NOT filing is real: the user has to ask you again, sometimes several times, until they trust the run is clean. That round-trip is far more expensive than 10 jot calls.

**Default to filing.** If you're hesitating ("is this worth reporting?") — yes, it is, jot it.

### Tag every finding with a kind

`testito jot` and `testito note` both take `--kind`. **Always pass it explicitly.** The dashboard sorts and colors findings by kind — bugs first, then polish, then questions, then info — so a human reviewer can triage without reading prose.

The four kinds, with the rule for picking:

- **`bug`** — *if you'd say "this is wrong"*. Something that looks broken, regressed, or in error. Console errors, 5xx responses, broken layouts, copy that contradicts behavior.
- **`polish`** — *if you'd say "this could be nicer"*. Works correctly but rough: typos, alignment off by a few px, awkward copy, jank, missing focus state.
- **`question`** — *if you'd say "is this right?"*. You're not sure if it's a bug or by design — wants human eyes.
- **`info`** — *if you'd say "for the record"*. Context, not a finding. "Tried X, it worked." "Reproduced this on Chrome 120." Default if you forget the flag, but prefer choosing.

If you'd reach for two kinds, prefer the louder one (`bug` > `polish` > `question` > `info`).

### Lead with the conclusion in your note text

The first line of every note shows up first in the dashboard. **Lead with what you saw, in one line, then expand.** Good: `"Footer copyright says 2024 on /login (expected current year)."` — followed by any details on a new paragraph. Bad: a multi-paragraph preamble that buries the actual finding.

### Examples that should always be jotted

- Typos, grammatical issues, awkward copy
- Misaligned buttons, pixels off, inconsistent spacing
- Console errors or warnings unrelated to the test you're running
- Network requests that 4xx/5xx unexpectedly
- Anything that worked but felt slow, jumpy, or twitchy
- A flow that worked but seemed needlessly complex (clicks + redirects)
- Accessibility hits: missing `alt`, low contrast, no focus state, keyboard trap
- Behavior that surprised you, even if you can't explain why

### Before calling `testito end`, walk this checklist

Ask yourself, out loud (in your reasoning), each of these. For each "yes," jot it before ending:

1. What UI quirks did I see, even small ones?
2. What console messages, network errors, or 4xx/5xx did I notice?
3. What felt slow, sluggish, or jittery?
4. What copy was confusing, wrong, or untranslated?
5. What was non-obvious or surprised me?
6. What looked broken on mobile / smaller widths?
7. What accessibility issues did I spot?

Only after you've answered all seven and filed every "yes" should you run `testito end`.

## The mental model

Three nouns:
- **Run** — one testing session. Identified by a name like `auth-smoke-2026-04-28`. Use a stable, descriptive name; same name = same run (resumable across messages).
- **Test** — a logical grouping of steps within a run. Phrase it as the user-facing scenario: `"check login flow with correct credentials"`, not `"login_test_1"`.
- **Step** — one concrete action or verification you perform. One step per `testito report` call. Examples: `"input email and password and click Sign in"`, `"dashboard loads within 2 seconds"`.

Steps are **append-only**. If you retry a step, log it again with `--attempt 2`. The log shows what actually happened, in order.

## CLI commands

```
testito start --run "<name>" [--description "..."] [METADATA…]
    Optional. Auto-created on first `report` if you skip this.

testito report --run "<name>" --test "<scenario>" --step "<action>"
              --result <pass|fail|warning|skipped> [--attempt N] [--note "..."]
              [--screenshot PATH ...] [METADATA…]
    The main verb. Call this once per step as you go.
    --screenshot is repeatable; each file is copied into testito's storage
    and rendered as an inline thumbnail under the step's note.

testito jot --run "<name>" --text "..." --kind <bug|polish|question|info>
            [--screenshot PATH ...]
    *** Use this freely. *** One-line, low-friction filing of an out-of-scope
    observation. Markdown is rendered in the dashboard. There is no downside
    to jotting too much; there is real downside to jotting too little.
    --kind defaults to info, but pass it explicitly so the dashboard can
    triage by it.

testito note --run "<name>" --scope <in|out> --kind <...> --text "..."
             [--screenshot PATH ...]
    Same idea but explicit about scope. Use scope=in for findings within the
    testing brief that don't fit the step grain (e.g. "login is unusually
    slow on first load"). Use scope=out (or just `jot`) for tangential
    observations.

testito end --run "<name>" [--fail-if-failures]
    Mark the run completed. Walk the pre-end checklist FIRST.
    --fail-if-failures exits 1 when the rollup is fail (CI hook).

testito list   [--limit N] [--json]
testito show   --run NAME [--json]
testito feedback --run NAME [--unseen] [--no-mark-seen] [--json]

testito review --run "<name>" --kind <security|code|perf|other>
              --verdict <clean|advisory|blocking|approve|approve-with-suggestions|request-changes>
              --text "..."
    File a one-shot assessment (security review, code review, perf review)
    on a run. Use this from `/security-review` / `/review` agents instead
    of dumping the verdict in chat.

testito triage --run "<name>" [--json] [--no-mark-seen] [--all]
    Actionable subset of a run for the coding agent: failed/warning steps,
    bug/polish notes, all feedback, all reviews. Bidirectional links
    (`finding_refs` per step; `feedback_ids` + `cited_by_step_ids` per
    finding). Use as the entry point when picking up a finished run.
    Read the human's feedback on this run from the dashboard. The user can
    type responses on individual findings or tests in the UI; this command
    is how you pick up answers, follow-up instructions, or "ignore this and
    move on" guidance. Default lists ALL feedback (and marks unseen as seen
    on read). --unseen filters to just-new since your last call. --json gives
    a machine-parseable shape with target_kind/target_id/target_name fields.
```

After every `testito report`, the CLI prints a one-line nag to stderr if
there's unseen feedback on the run — that's your signal to call
`testito feedback --run X` before continuing.

**Run metadata** (`[METADATA…]`) — pass on `start` or any `report`. The dashboard
shows them in a banner so a reviewer knows *what* was tested.

```
--branch  <name>       e.g. main, feature/checkout       (auto: `git rev-parse`)
--commit  <sha>        e.g. abc1234                      (auto: `git rev-parse --short HEAD`)
--env     <label>      e.g. local, staging, prod
--url     <origin>     e.g. http://localhost:3000
--workdir <label>      linked-worktree dir + zellij session  (auto-detected)
--pr      <number>     GitHub PR number                  (auto: `gh pr view`)
--pr-url  <url>        PR HTML URL                       (auto-detected with --pr)
```

`--branch`, `--commit`, and `--workdir` are filled in automatically from the
current shell + git working tree if you don't pass them. **Don't pass them
unless you need to override the auto-detected value** (e.g. you're testing a
deployed environment whose code lives elsewhere). Always pass `--env` and
`--url` explicitly — those can't be inferred.

Help: `testito --help`, `testito report --help`, etc.

## Read feedback the human leaves

The dashboard has a `💬 Add feedback` box on each finding and on each test header. The human uses it to ask you questions ("does this also reproduce on Safari?"), answer your own (`question` kind) findings ("yes, the 60-min expiry is intentional"), or hand you instructions ("skip the rest of this test, file a Linear ticket"). Treat their feedback as the authoritative voice in the room.

**Workflow:**

1. After every `testito report`, watch stderr for `👤 N unseen feedback item(s) on this run`. That's the dashboard's polite "hey".
2. When you see it (or proactively before each test), run:
   ```bash
   testito feedback --run "$RUN" --unseen --json
   ```
   This returns the new feedback as JSON, grouped by target (note/test/run). Marks each as seen on read so the next `--unseen` call is empty unless the human added more.
3. Act on each item:
   - Question on one of YOUR findings → file the answer (e.g. `testito jot --run X --kind info --text "Re finding #3: confirmed expected behavior, see PRD link X."`).
   - Instruction on a test → follow it. If they said "skip the rest", `testito report` the remaining steps as `--result skipped` with a note pointing at the feedback.
   - Question to you → answer with `testito jot --kind info --text "..."` so the answer lives in the run.
4. Don't ignore feedback. The human typed it because they want a response — silence here is exactly the failure mode this whole skill is fighting.

`--no-mark-seen` peeks without acking — useful if you want to glance and come back. `testito feedback --run X` (no flags) lists every feedback item ever left on the run, in chronological order.

## Anchor steps to findings with `--finding-ref`

When a step's `--note` is going to repeat a finding you just filed (e.g.
the step's whole purpose is to surface i18n gaps and you just jotted
each gap as a `bug`/`polish` finding), pass `--finding-ref <id>` on
`testito report` to anchor the step to those findings. Repeatable.

```bash
# Filed two i18n bugs first
testito jot --run "$RUN" --kind bug    --text "..." --screenshot ...   # → returns id 33
testito jot --run "$RUN" --kind polish --text "..." --screenshot ...   # → returns id 34

# Now report the step that surfaced them — anchor instead of duplicating
testito report --run "$RUN" --test "i18n" --step "Switch locale to Español" \
  --result warning --note "i18n gaps below" \
  --finding-ref 33 --finding-ref 34
```

`testito triage --json` then emits `tests_with_issues[].steps[].finding_refs: [33, 34]` and `findings[].cited_by_step_ids: [<step_id>]`, and trims the step note to its first line. Without anchors, the step's full note text is the only carrier of context, so triage keeps it intact — but the result is fatter. Anchor whenever you can.

The id you pass is the one returned by `testito jot` (printed as `jotted #33 …`) or visible in the dashboard / `testito triage --json`.

## File security and code reviews to testito, not chat

When you run a `/security-review`, `/review`, or any other "audit the diff"
pass — **do not dump the verdict into the chat**. File it as a `testito
review` so the human sees it on the dashboard alongside the QA run, gets it
in the PR-summary blob, and can leave feedback on it.

```bash
# Security review — typically clean or advisory
testito review --run "$RUN" --kind security --verdict clean --text "$(cat <<'EOF'
No vulnerabilities found.

The diff is purely client-side state-management plumbing — no auth/session
paths, no new endpoints, no SSR HTML interpolation, no filesystem/process/eval
sinks.
EOF
)"

# Code review — use the GitHub-style verdict aliases the agent already thinks in
testito review --run "$RUN" --kind code --verdict approve --text "..."
testito review --run "$RUN" --kind code --verdict approve-with-suggestions --text "..."
testito review --run "$RUN" --kind code --verdict request-changes --text "..."
```

**Kind picks the icon and label**: `security` (🛡️), `code` (🔍), `perf` (⚡),
`other` (📝). **Verdict picks the banner color**: `clean` → green, `advisory` →
yellow, `blocking` → red. Aliases: `approve` = `clean`,
`approve-with-suggestions` = `advisory`, `request-changes` = `blocking`.

Lead the `--text` with the verdict in plain language (the dashboard renders
the first line first). Markdown is fully supported — headings, bullets, code
fences, links. The reviewing agent doesn't have to know the run name in
advance: pick a stable name like `pr-340-review` and `testito review`
auto-creates the run, picking up branch/commit/PR via `gh pr view`.

Reviews are append-only: a follow-up `/security-review` after a code change
files a new review row instead of mutating the previous one, so the
assessment history is on the dashboard.

## Picking up an existing run (act-on-findings mode)

When the user hands you a finished run and says "act on what's there",
use `testito triage --run "<name>" --json` instead of `show`. It returns,
in one call:

- Tests with **fail** or **warning** steps (passes are filtered out).
  Each step carries `finding_refs: [...]` when it was anchored via
  `--finding-ref` — chase those into `findings[]` rather than re-reading
  the step's note.
- Findings with **kind=bug** or **kind=polish** (questions and info are
  filtered out unless you pass `--all`). Each finding carries
  `feedback_ids: [...]` and `cited_by_step_ids: [...]` so you don't have
  to cross-reference manually.
- Every feedback item left on the run, marked seen on read.

This is the right entry point for "I QA'd this PR yesterday, fix the
issues today" workflows — much shorter than walking the full `show`
output and decides nothing for you.

## How to test

1. **Pick a run name up front.** Combine purpose + date or build, e.g. `checkout-smoke-2026-04-28`. Use the **same name** for every command in the session.
2. **Capture metadata up front.** Pass `--description`, `--env`, and `--url` on `testito start` (or the first `report`). Skip `--branch`/`--commit`/`--workdir` — those auto-detect from `git` and `$ZELLIJ_SESSION_NAME` and you'll usually want the auto-detected value.
3. **For each step you take, immediately call** `testito report ... --result <pass|fail|warning|skipped>`:
   - `pass` — the verification succeeded.
   - `fail` — the expected outcome did not happen. **Always pair with `--note`** describing the symptom.
   - `warning` — it worked but with a caveat (slow, ugly, partially right). Pair with `--note`.
   - `skipped` — couldn't run (preconditions missing, blocked by an earlier failure). Pair with `--note` saying why.
4. **Jot tangential findings as you see them, not at the end.** `testito jot --run "<name>" --text "..."`. Don't batch — each observation goes in immediately so you don't forget.
5. **Attach screenshots when they help.** When you're testing in a browser via playwright-cli (or any tool that drops PNGs on disk), pass `--screenshot PATH` on `report`, `jot`, or `note`. The CLI accepts the flag repeatedly so you can attach multiple. Strong cases: any UI bug, any layout/polish observation, any "the page looked weird" warning. The file is copied into testito's storage (you don't need to keep the original around) and the dashboard shows it as a clickable thumbnail under the finding. Example: `testito jot --run X --kind bug --text "Dashboard 500s" --screenshot /tmp/playwright-cli/page-2026-04-29.png`.

6. **Notes are markdown.** Code fences for tracebacks, backticks for paths/commands, links for URLs. Multi-line notes are fine — quote the whole thing in a single shell argument:
   ```
   --note $'```\nUncaught TypeError: Cannot read properties of undefined\n  at handleSubmit (login.ts:42)\n```'
   ```
7. **If you retry a step**, log the second attempt with `--attempt 2` (and so on). Don't overwrite — re-call `report` with the same `--test` and `--step` and a higher `--attempt`.
8. **Watch stderr after every report** for the `👤 N unseen feedback items` nag. The user might have responded to a finding or left an instruction on a test — read it via `testito feedback --run X --unseen` before continuing.
9. **Walk the checklist** above before `end`. Jot anything left.
10. **End the run.** `testito end --run "<name>"`.
11. Tell the user the run name and (if `testito serve` is running) the dashboard URL: `http://127.0.0.1:7878/runs/<id>`. Mention the count of jotted findings (`testito show` will summarize) and any screenshots you attached.

## Result-vocabulary discipline

- **`pass`**: the step's expected outcome happened. Don't use pass for "kind of worked".
- **`fail`**: the expected outcome did not happen. Always pair with `--note`.
- **`warning`**: it worked but with a caveat. Pair with `--note`.
- **`skipped`**: you didn't run the step. Pair with `--note` saying why.

If you're tempted to invent new categories, that's a sign the observation belongs in `testito jot`, not in a step result.

## Phrasing guidance

- **Test names** should describe the user-facing scenario in plain language. Good: `"create a customer with valid required fields"`. Bad: `"test_customer_create_valid"`, `"happy path"`.
- **Step names** should describe one observable action or assertion. Good: `"submit the form and confirm a 'created' toast appears"`. Bad: `"step 1"`, `"check stuff"`.
- **Jot text** is markdown. Lead with what you saw, not what you think it means: `"Footer copyright says 2024 on /login (expected current year)"` is better than `"Possible footer issue."`

## Worked example

```bash
RUN=auth-smoke-2026-04-28

testito start --run "$RUN" \
  --description "Login + password reset on staging" \
  --branch main --commit abc1234 --env staging --url https://staging.example.com

# Test 1
testito report --run "$RUN" \
  --test "login with valid credentials" \
  --step "navigate to /login" --result pass

# I noticed something tangential while loading the page — file it now, don't wait
# (and attach the screenshot I just took so the human can see it without repro)
testito jot --run "$RUN" --kind polish \
  --text "Login page logo is slightly blurry on retina; suspect missing 2x asset." \
  --screenshot /tmp/playwright-cli/page-2026-04-29-login-logo.png

testito report --run "$RUN" \
  --test "login with valid credentials" \
  --step "enter valid email and password and click Sign in" --result pass

testito report --run "$RUN" \
  --test "login with valid credentials" \
  --step "redirected to /dashboard within 2s" --result warning \
  --note "Took ~3.5s on first request, ~400ms after."

# Test 2 — failure + retry pattern, with a code-block in the note
testito report --run "$RUN" \
  --test "password reset email arrives" \
  --step "submit reset form with registered email" --result pass

testito report --run "$RUN" \
  --test "password reset email arrives" \
  --step "reset email arrives within 30s" --result fail \
  --note $'Waited 90s, no email.\n\n```\nWorker log: redis: connection refused (172.18.0.4:6379)\n```' \
  --screenshot /tmp/playwright-cli/reset-form-still-loading.png
# (after this report, stderr might say "👤 1 unseen feedback item on this run".
#  if it does, run `testito feedback --run "$RUN" --unseen` to read what the user said.)

testito report --run "$RUN" \
  --test "password reset email arrives" \
  --step "reset email arrives within 30s" --result pass --attempt 2 \
  --note "Arrived in ~12s after worker restart."

# More tangential things noticed during the session — note the explicit --kind
testito jot --run "$RUN" --kind polish \
  --text "Footer copyright still says **2024** on the login page."
testito jot --run "$RUN" --kind polish \
  --text "Reset email subject is 'Reset Your Password.' (lowercase Y in actual mail), inconsistent with the heading."
testito jot --run "$RUN" --kind bug \
  --text "Console: \`Warning: validateDOMNesting(...): <div> cannot appear as a descendant of <p>.\` on /reset."
testito jot --run "$RUN" --kind question \
  --text "Reset link expires after 60 min — is that intentional? Couldn't find it documented."

# Walk the pre-end checklist above. Jot anything else. THEN:
testito end --run "$RUN"
```

## What the user sees

The user runs `testito` (or `testito serve`) in a separate terminal. The dashboard at `http://127.0.0.1:7878/` lists every run; opening one shows tests grouped by name with each step's status, attempt number, note, and timestamp, plus a separate Notes section with in-scope and out-of-scope items. The page polls every 2 seconds, so your reports appear as you go.

## Common mistakes to avoid

- **Don't batch notes at the end** — call `testito jot` (or `testito report`) the moment you notice something. Half of agent omissions happen because the agent forgot a finding by the time it got around to filing.
- **Don't filter for relevance.** Filing has no downside. If you're unsure whether something belongs, jot it.
- **Don't skip `--note`** on failures and warnings.
- **Don't rename the test mid-session** — pick the `--test "..."` string once and reuse it verbatim for all steps in that scenario.
- **Don't call `testito end` until you've walked the pre-end checklist** above.
