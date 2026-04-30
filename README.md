# testito

[![CI](https://github.com/malazaysc/testito/actions/workflows/ci.yml/badge.svg)](https://github.com/malazaysc/testito/actions/workflows/ci.yml)

A small, single-binary log for **manual testing**, designed for AI agents to write and humans to read.

Agents call a CLI as they execute steps; the dashboard renders the run live in a browser.

```bash
testito report --run "auth-smoke-2026-04-28" \
  --test "login with valid credentials" \
  --step "redirected to /dashboard within 2s" --result warning \
  --note "Took ~3.5s on first request, ~400ms after."
```

```bash
testito                          # opens the dashboard at http://127.0.0.1:7878
```

That's the whole product.

---

## Why

Automated test runners (`pytest`, `cargo test`, `playwright test`) have great reporters. Manual / agent-driven testing doesn't. Notes get scattered across Slack, screenshots, and the agent's own context. testito gives those sessions a structured home: append-only steps with pass/fail/warning/skipped, retry attempts, in/out-of-scope notes, run metadata (branch / commit / env / url), and a markdown export for sharing.

The mental model is three nouns:

- **Run** — one testing session, identified by a name like `auth-smoke-2026-04-28`. Same name = same run.
- **Test** — a logical scenario inside a run (`"check login flow with correct credentials"`).
- **Step** — one observable action or verification, reported as it happens. Append-only; retries get higher `--attempt` numbers.

---

## Install

### From a GitHub release (no Rust toolchain needed)

```bash
# pick the asset for your platform from
#   https://github.com/malazaysc/testito/releases/latest
# example for macOS arm64:
TAG=v0.1.0
ASSET=testito-${TAG#v}-macos-aarch64
curl -L -o /tmp/testito.tar.gz \
  "https://github.com/malazaysc/testito/releases/download/${TAG}/${ASSET}.tar.gz"
tar -xzf /tmp/testito.tar.gz -C /tmp
install /tmp/${ASSET}/testito /usr/local/bin/testito
```

Available asset suffixes: `linux-x86_64`, `macos-aarch64`, `macos-x86_64`. Each release also ships a `.sha256` next to the tarball.

### From source

```bash
cargo build --release
# binary at ./target/release/testito (~3.8 MB, no runtime deps)
ln -s "$(pwd)/target/release/testito" /usr/local/bin/testito
```

---

## Quick start

In one terminal, start the dashboard:

```bash
testito          # serves http://127.0.0.1:7878 — run name auto-creates the SQLite db
```

In another terminal (or from an agent), report as you test:

```bash
RUN=checkout-smoke-2026-04-28

testito start  --run "$RUN" \
  --description "Cart + checkout regression" \
  --branch feature/checkout --commit abc1234 --env staging --url https://staging.example.com

testito report --run "$RUN" --test "add to cart" --step "open product page" --result pass
testito report --run "$RUN" --test "add to cart" --step "click add to cart"  --result pass

testito report --run "$RUN" --test "checkout flow" --step "submit payment" --result fail \
  --note $'Stripe webhook returned 500.\n\n```\nERR: webhook_signature_mismatch\n```'

testito report --run "$RUN" --test "checkout flow" --step "submit payment" --result pass --attempt 2 \
  --note "Worked after rotating webhook secret."

testito note   --run "$RUN" --scope out --text "Footer copyright still says **2024** on /login."

testito end    --run "$RUN"
```

The dashboard polls every 2 seconds, so steps appear as you go.

---

## CLI reference

```
testito [serve]               # default — runs the dashboard. --port N, --db PATH
testito start  --run NAME [--description ...] [METADATA…]
testito report --run NAME --test ... --step ... --result <pass|fail|warning|skipped>
                            [--attempt N] [--note ...] [METADATA…]
testito note   --run NAME --scope <in|out> --text ...
testito jot    --run NAME --text ...                          # synonym for note --scope out
testito end    --run NAME [--fail-if-failures]
testito list   [--limit N] [--json]
testito show   --run NAME [--json]
testito triage --run NAME [--json] [--no-mark-seen] [--all]
testito feedback --run NAME [--unseen] [--no-mark-seen] [--json]
testito review --run NAME --kind <security|code|perf|other>
                          --verdict <clean|advisory|blocking|approve|approve-with-suggestions|request-changes>
                          --text "..."
```

`review` files a one-shot assessment (security review, code review, perf review) on a run. Append-only, so re-running `/security-review` after a fix files a new review row. Renders as a color-coded banner above Findings on the dashboard (green clean / yellow advisory / red blocking) and gets included in the PR-summary blob and markdown export.

`--fail-if-failures` makes `end` exit `1` when the run's rollup is `fail`. Wire it into CI: agent reports → `testito end --run "$RUN" --fail-if-failures` is the gate.

`list` and `show` mirror what the dashboard renders, but in the terminal — handy for headless / CI runs. `--json` on either gives a stable shape for scripts.

`triage` is the "what do I need to act on?" view. One call returns failed/warning steps, bug/polish notes, and every feedback item — exactly the actionable subset for a coding agent picking up a finished run. Marks feedback as seen by default (use `--no-mark-seen` to peek). Pair with `--json` for parsing.

`jot` is the low-friction "I noticed something off, didn't fit the brief" command. It's a one-liner synonym for `note --scope out`. The skill (in `.claude/skills/testito/SKILL.md`) tells agents to use it freely as they test, so out-of-scope findings get filed in the moment instead of being lost or forgotten.

`[METADATA…]` (accepted by `start` and `report`):

```
--branch  <name>      e.g. main, feature/checkout       (auto: git rev-parse)
--commit  <sha>       e.g. abc1234                      (auto: git rev-parse --short)
--pr      <number>    GitHub PR number                  (auto: gh pr view)
--workdir <label>     worktree dir + zellij session     (auto-detected)
--env     <label>     e.g. local, staging, prod
--url     <origin>    e.g. http://localhost:3000
```

Each non-empty value is upserted on the run; subsequent calls don't overwrite unless re-passed. Any `testito report` will auto-create the run if `start` was skipped.

`testito --help`, `testito report --help`, etc. show the full set.

---

## Result vocabulary

| Result    | Meaning                                                                  |
|-----------|--------------------------------------------------------------------------|
| `pass`    | Expected outcome happened.                                               |
| `fail`    | Expected outcome did not happen. **Always pair with `--note`.**          |
| `warning` | Worked but with a caveat — slow, ugly, partial. Pair with `--note`.      |
| `skipped` | Couldn't run (preconditions missing, blocked by an earlier failure).     |

If you're tempted to invent a fifth, use `testito note --scope in --text ...` instead.

---

## Dashboard

`http://127.0.0.1:7878/` lists every run with branch, env, counts, and a rollup pill. Click into one for the live view:

- **Rollup** per test (latest attempt of each step decides the verdict).
- **Failure-first**: passing tests render collapsed; failing/warning tests open. Toggle "hide passing" to focus.
- **Filter** input (text matches across test/step names + notes).
- **Notes section** with in-scope / out-of-scope badges. Notes are markdown — code fences, links, lists, tables.
- **Compare with…** any other run for a side-by-side rollup with diff highlights.
- **Download .md** to ship the run as a markdown report (failures listed up top).

The page polls every 2 seconds, so what an agent reports appears in your browser within ~2s.

---

## The agent skill

`.claude/skills/testito/SKILL.md` teaches Claude Code (or another agent runtime) how to call the CLI: result-vocabulary discipline, retry pattern with `--attempt`, when to use `note --scope out`, and a worked example. Drop it under `.claude/skills/` in any project where an agent should report into testito.

---

## Where data lives

- SQLite database at `~/Library/Application Support/dev.testito.testito/testito.db` on macOS (platform-native data dir on Linux/Windows).
- Override with `--db PATH` on any subcommand. The CLI and `serve` can share the same file safely (WAL mode, busy timeout 5s).
- The CLI talks to SQLite directly; `serve` doesn't need to be running for `report` to work.

---

## Architecture

A boring stack on purpose:

- **Rust** + **axum** + **rusqlite** (bundled) — single static binary, no system deps.
- **askama** for compile-time HTML templates.
- **htmx** for live polling, **Alpine.js** for filter/collapse state, **pulldown-cmark** for note rendering (raw HTML stripped).
- Server-side renders relative timestamps and per-test rollups; the dashboard is read-only.

```
src/
  main.rs       # clap subcommands, server bootstrap
  routes.rs     # HTTP handlers + view models
  db.rs         # rusqlite layer + migrations
  models.rs     # Result, Scope, Run, RunStep, RunNote, rollup, relative_time
  md.rs         # markdown sanitizer (drops raw HTML events)
templates/
  base.html, home.html, runs_table.html,
  run.html, run_body.html, compare.html
.claude/skills/testito/SKILL.md
```

---

## Development

```bash
cargo fmt           # format
cargo clippy        # lint (warnings configured in Cargo.toml [lints])
cargo test          # run unit + integration tests
cargo build --release
```

The `[lints]` section in `Cargo.toml` is the gate; CI just runs the three commands above.

---

## License

MIT.
