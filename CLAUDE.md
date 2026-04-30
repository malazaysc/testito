# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo fmt --all -- --check        # CI gate
cargo clippy --all-targets -- -D warnings   # CI gate (warnings configured in [lints] in Cargo.toml)
cargo test --all-targets --no-fail-fast     # CI gate
cargo run -- serve                # local dev server on http://127.0.0.1:7878
cargo test <name>                 # single test by name substring
```

CI runs the three gate commands above on every push/PR. `RUSTFLAGS=-D warnings` in CI — local clippy must be clean.

After editing Rust code, the running `testito serve` does not auto-reload — kill it and re-run. The CLI subcommands talk to SQLite directly and don't need the server running.

## Architecture

**Single binary, two modes.** `testito serve` is the axum dashboard; every other subcommand (`start`, `report`, `note`, `jot`, `end`, `feedback`, `list`, `show`) is a one-shot CLI that opens the same SQLite file, writes, and exits. Both processes share the DB safely via WAL + 5s busy timeout. Default DB lives in the platform data dir (`directories` crate); override with `--db PATH`.

**Append-only event log.** The data model is `Run → RunTest → RunStep` plus `RunNote`, `Attachment`, `Feedback`. Steps are never updated — retries get a higher `--attempt` and a new row. The verdict for a test/run is computed by `rollup()` in `src/models.rs` over the latest attempt of each step. Treat this as load-bearing: don't add UPDATE paths for steps, and any new aggregation should ride on top of `rollup()` so it stays consistent with the rest of the UI.

**Render path.** Two askama templates render the same data:
- `run.html` — full page on first load (extends `base.html`)
- `run_body.html` — htmx-polled fragment, swapped every 2s

Both paths fan in to `build_run_body()` in `src/routes.rs`, which fetches everything in batched queries (one call per table, then group by target_id in-memory). When you add a field that the run page surfaces, add it to **both** `RunTpl` and `RunBodyTpl` and populate it in `build_run_body()` — otherwise the field appears on first load and disappears after the first poll (or vice versa).

**Markdown is rendered server-side and trusted in templates.** All user-provided text (note text, feedback text, run description, the PR-summary blob, step notes) is run through `md::to_html()`, which uses `pulldown-cmark` and **drops raw `Event::Html`/`Event::InlineHtml` events** and rewrites link/image URLs through `sanitize_url()` (allowlist: `http`, `https`, `mailto`, relative). The output is then injected with askama's `|safe` filter into a container with class `md-content`. The pattern is: derive an `x_html: String` field next to the raw `x: String`, render `{{ x_html|safe }}` in the template. Never inject user-provided text directly into HTML attributes or with `|safe` outside that pipeline.

**The PR-summary block is special.** It renders the same markdown twice: as HTML for reading, and via the `data-copy-text` attribute on the copy button so "📋 copy markdown" copies the raw markdown source for pasting into a PR.

**Run metadata auto-detect.** `src/auto.rs` shells out to `git rev-parse` for branch + short SHA, detects linked worktrees by comparing `--git-common-dir` to `--git-dir`, and reads `ZELLIJ_SESSION_NAME` from the env. `MetaArgs::into_meta()` fills any unset metadata field from this in `start`/`report`. Detection is best-effort — silent no-op if `git` is missing or we're outside a repo. Explicit flags always win.

**Schema migrations are idempotent.** `Db::open()` creates tables and runs ALTERs guarded by `column_exists()` / `has_legacy_table()` checks — there's no migration table. Both the CLI and `serve` run them on startup, so any schema change must be safe to apply concurrently and to re-apply.

**Screenshots are content-addressed.** `src/storage.rs` stores uploads under `<data_dir>/screenshots/<sha256>.<ext>`, dedupes across runs, and the `/screenshots/<filename>` route validates the filename matches `[a-f0-9]{64}(\.\w{1,8})?` and canonicalizes-against-dir before serving.

## Agent skill

`.claude/skills/testito/SKILL.md` teaches agents how to call the testito CLI. **When the CLI surface changes** (new flag, new subcommand, changed semantics, new metadata field), update the skill in the same change — the skill is what agents read, not the README. The user keeps a copy under `~/.claude/skills/testito/` for cross-project use, so flag this when relevant.

## QA workflow on UI changes

For dashboard changes, use the `playwright-cli` skill (`.claude/skills/playwright-cli/SKILL.md`) to actually load the page, take a screenshot, and read the DOM — don't claim a UI fix works without verifying. Restart `testito serve` first; the dashboard polls every 2s.

## Conventions worth knowing

- **Result vocabulary is closed**: `pass | fail | warning | skipped`. New nuance goes in `testito note --scope in/out`, not a new result variant.
- **Note kinds** (`bug | polish | question | info`) drive sort order in `Findings & notes` and the kind-counts pills via `Kind::sort_priority()` and `KindCounts`.
- `--scope out` notes are "findings" and surface prominently (banner, pill, end-of-run nag if zero filed). `jot` is a synonym for `note --scope out`.
- `testito report` prints a stderr nag when there's unread feedback — the nag is intentional, don't silence it.
