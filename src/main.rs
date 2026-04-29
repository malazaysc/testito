use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::Router;
use clap::{Args, Parser, Subcommand};
use tokio::sync::Mutex;

mod db;
mod md;
mod models;
mod routes;
mod storage;

use db::Db;
use models::{
    relative_time, rollup, AttachmentTarget, Kind, Result as TestResult, RunMeta, RunStep, Scope,
};

#[derive(Parser, Debug)]
#[command(name = "testito", version, about = "Manual testing log for AI agents")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the web UI (default if no subcommand is given).
    Serve(ServeArgs),

    /// Create or touch a run (optionally sets a description). Optional — `report` auto-creates.
    Start(StartArgs),

    /// Append a step result to a run.
    Report(ReportArgs),

    /// Append a free-text note to a run, marked in or out of scope.
    Note(NoteArgs),

    /// Quick jot: file an out-of-scope observation. Low-friction synonym for
    /// `note --scope out`. Use freely as you test — small UI quirks, typos,
    /// surprising behavior, accessibility hits, anything you noticed but
    /// wasn't explicitly asked about.
    Jot(JotArgs),

    /// Mark a run as completed.
    End(EndArgs),

    /// List recent runs as a table (or JSON with --json).
    List(ListArgs),

    /// Print one run's metadata, counts, failures, tests, and notes to stdout.
    Show(ShowArgs),
}

#[derive(Args, Debug, Default)]
struct MetaArgs {
    /// Branch under test (e.g. main, feature/x).
    #[arg(long)]
    branch: Option<String>,

    /// Commit SHA under test.
    #[arg(long, alias = "commit-sha")]
    commit: Option<String>,

    /// Environment label (e.g. local, staging, prod).
    #[arg(long)]
    env: Option<String>,

    /// URL or origin under test (e.g. http://localhost:3000).
    #[arg(long)]
    url: Option<String>,
}

impl MetaArgs {
    fn into_meta(self, description: Option<String>) -> RunMeta {
        RunMeta {
            description,
            branch: self.branch,
            commit_sha: self.commit,
            env: self.env,
            url: self.url,
        }
    }
}

#[derive(Args, Debug)]
struct ServeArgs {
    /// Port for the web UI.
    #[arg(short, long, default_value_t = 7878)]
    port: u16,

    /// SQLite database file (default: platform data dir).
    #[arg(long)]
    db: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct StartArgs {
    /// Run name (acts as a stable identifier — same name = same run).
    #[arg(long)]
    run: String,

    /// Optional description shown in the dashboard.
    #[arg(long)]
    description: Option<String>,

    #[command(flatten)]
    meta: MetaArgs,

    /// SQLite database file (default: platform data dir).
    #[arg(long)]
    db: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct ReportArgs {
    /// Run name.
    #[arg(long)]
    run: String,

    /// Test name (a logical group of steps).
    #[arg(long)]
    test: String,

    /// One step within the test (a single concrete action or verification).
    #[arg(long)]
    step: String,

    /// Result for this step: pass | fail | warning | skipped.
    #[arg(long)]
    result: String,

    /// Attempt number for this step (1-indexed). Use 2+ for retries.
    #[arg(long, default_value_t = 1)]
    attempt: i64,

    /// Optional note attached to this step (e.g. error details). Markdown-rendered.
    #[arg(long)]
    note: Option<String>,

    /// Path to a screenshot to attach to this step. Repeatable. The file is
    /// copied into testito's storage dir; the original is no longer needed.
    #[arg(long = "screenshot")]
    screenshots: Vec<PathBuf>,

    #[command(flatten)]
    meta: MetaArgs,

    /// SQLite database file (default: platform data dir).
    #[arg(long)]
    db: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct NoteArgs {
    /// Run name.
    #[arg(long)]
    run: String,

    /// Scope: in (in-scope finding) or out (out-of-scope observation).
    #[arg(long, default_value = "in")]
    scope: String,

    /// What kind of observation: bug, polish, question, or info (default).
    #[arg(long, default_value = "info")]
    kind: String,

    /// The note text.
    #[arg(long)]
    text: String,

    /// Path to a screenshot to attach. Repeatable.
    #[arg(long = "screenshot")]
    screenshots: Vec<PathBuf>,

    /// SQLite database file (default: platform data dir).
    #[arg(long)]
    db: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct JotArgs {
    /// Run name.
    #[arg(long)]
    run: String,

    /// The observation. Markdown is rendered in the dashboard.
    #[arg(long)]
    text: String,

    /// What kind of observation: bug, polish, question, or info (default).
    /// Bugs surface first in the dashboard so the human can triage at a glance.
    #[arg(long, default_value = "info")]
    kind: String,

    /// Path to a screenshot to attach. Repeatable.
    #[arg(long = "screenshot")]
    screenshots: Vec<PathBuf>,

    /// SQLite database file (default: platform data dir).
    #[arg(long)]
    db: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct EndArgs {
    /// Run name.
    #[arg(long)]
    run: String,

    /// Exit with status 1 if the run's rollup is `fail` (any test ended on a failing
    /// step). Useful for wiring testito into CI: agent reports → testito end checks.
    #[arg(long)]
    fail_if_failures: bool,

    /// SQLite database file (default: platform data dir).
    #[arg(long)]
    db: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct ListArgs {
    /// Maximum number of runs to print (newest first).
    #[arg(long, default_value_t = 20)]
    limit: usize,

    /// Print as JSON instead of a human-readable table.
    #[arg(long)]
    json: bool,

    /// SQLite database file (default: platform data dir).
    #[arg(long)]
    db: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct ShowArgs {
    /// Run name.
    #[arg(long)]
    run: String,

    /// Print as JSON instead of a human-readable summary.
    #[arg(long)]
    json: bool,

    /// SQLite database file (default: platform data dir).
    #[arg(long)]
    db: Option<PathBuf>,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Db>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("testito=info,tower_http=info")
            }),
        )
        .init();

    let cli = Cli::parse();

    let cmd = cli.cmd.unwrap_or(Cmd::Serve(ServeArgs {
        port: 7878,
        db: None,
    }));
    match cmd {
        Cmd::Serve(a) => {
            let db_path = resolve_db(a.db)?;
            serve(db_path, a.port).await
        }
        Cmd::Start(a) => {
            let db_path = resolve_db(a.db.clone())?;
            cmd_start(db_path, a)
        }
        Cmd::Report(a) => {
            let db_path = resolve_db(a.db.clone())?;
            cmd_report(db_path, a)
        }
        Cmd::Note(a) => {
            let db_path = resolve_db(a.db.clone())?;
            cmd_note(db_path, a)
        }
        Cmd::Jot(a) => {
            let db_path = resolve_db(a.db.clone())?;
            cmd_jot(db_path, a)
        }
        Cmd::End(a) => {
            let db_path = resolve_db(a.db.clone())?;
            cmd_end(db_path, a)
        }
        Cmd::List(a) => {
            let db_path = resolve_db(a.db.clone())?;
            cmd_list(db_path, a)
        }
        Cmd::Show(a) => {
            let db_path = resolve_db(a.db.clone())?;
            cmd_show(db_path, a)
        }
    }
}

fn resolve_db(arg: Option<PathBuf>) -> Result<PathBuf> {
    let path = arg.unwrap_or_else(default_db_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(path)
}

async fn serve(db_path: PathBuf, port: u16) -> Result<()> {
    tracing::info!("opening db at {}", db_path.display());
    let db = Db::open(&db_path)?;
    let state = AppState {
        db: Arc::new(Mutex::new(db)),
    };
    let app: Router = routes::router(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!("listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn cmd_start(db_path: PathBuf, a: StartArgs) -> Result<()> {
    let db = Db::open(&db_path)?;
    let run_name = a.run.clone();
    let meta = a.meta.into_meta(a.description);
    let id = db.ensure_run(&run_name, &meta)?;
    println!("run {} (id {}) ready", run_name, id);
    Ok(())
}

fn cmd_report(db_path: PathBuf, a: ReportArgs) -> Result<()> {
    let result = TestResult::parse(&a.result)?;
    let db = Db::open(&db_path)?;
    let run_name = a.run.clone();
    let test_name = a.test.clone();
    let step_name = a.step.clone();
    let attempt = a.attempt;
    let meta = a.meta.into_meta(None);
    let run_id = db.ensure_run(&run_name, &meta)?;
    let test_id = db.ensure_test(run_id, &test_name)?;
    let step_id = db.append_step(
        test_id,
        &step_name,
        attempt,
        result,
        a.note.as_deref().unwrap_or(""),
    )?;
    let attached = ingest_attachments(&db, AttachmentTarget::Step, step_id, &a.screenshots)?;
    println!(
        "{} · run={} test={} step #{} attempt={}{}",
        result.label(),
        run_name,
        test_name,
        step_id,
        attempt,
        attachments_suffix(attached),
    );
    Ok(())
}

fn cmd_note(db_path: PathBuf, a: NoteArgs) -> Result<()> {
    let scope = Scope::parse(&a.scope)?;
    let kind = Kind::parse(&a.kind)?;
    let db = Db::open(&db_path)?;
    let run_id = db.ensure_run(&a.run, &RunMeta::default())?;
    let id = db.append_note(run_id, scope, kind, &a.text)?;
    let attached = ingest_attachments(&db, AttachmentTarget::Note, id, &a.screenshots)?;
    println!(
        "note #{} on {} ({} {} {}){}",
        id,
        a.run,
        kind.emoji(),
        kind.label(),
        scope.as_str(),
        attachments_suffix(attached),
    );
    Ok(())
}

fn cmd_jot(db_path: PathBuf, a: JotArgs) -> Result<()> {
    let kind = Kind::parse(&a.kind)?;
    let db = Db::open(&db_path)?;
    let run_id = db.ensure_run(&a.run, &RunMeta::default())?;
    let id = db.append_note(run_id, Scope::Out, kind, &a.text)?;
    let attached = ingest_attachments(&db, AttachmentTarget::Note, id, &a.screenshots)?;
    println!(
        "jotted #{} on {} ({} {}){}",
        id,
        a.run,
        kind.emoji(),
        kind.label(),
        attachments_suffix(attached),
    );
    Ok(())
}

fn ingest_attachments(
    db: &Db,
    target_kind: AttachmentTarget,
    target_id: i64,
    paths: &[PathBuf],
) -> Result<usize> {
    for p in paths {
        let ingested = storage::ingest_screenshot(p)?;
        db.insert_attachment(
            target_kind,
            target_id,
            &ingested.filename(),
            &ingested.original_filename,
            &ingested.mime_type,
            ingested.bytes_written as i64,
        )?;
    }
    Ok(paths.len())
}

fn attachments_suffix(n: usize) -> String {
    if n == 0 {
        String::new()
    } else {
        format!(" · 📎 {} screenshot{}", n, if n == 1 { "" } else { "s" })
    }
}

fn cmd_end(db_path: PathBuf, a: EndArgs) -> Result<()> {
    let db = Db::open(&db_path)?;
    let run_id = db
        .find_run_id(&a.run)?
        .ok_or_else(|| anyhow::anyhow!("run '{}' does not exist", a.run))?;
    db.complete_run(run_id)?;
    let run_rollup = compute_run_rollup(&db, run_id)?;
    let status = run_rollup.map(|r| r.label()).unwrap_or("no steps reported");

    // Soft nudge before exiting if the agent didn't file any tangential
    // observations. Counts out-of-scope notes only — those are the ones
    // agents most often elide.
    let findings = db
        .notes_for_run(run_id)?
        .into_iter()
        .filter(|n| n.scope == Scope::Out)
        .count();
    if findings == 0 {
        eprintln!();
        eprintln!("⚠  You filed 0 out-of-scope observations on this run.");
        eprintln!(
            "   Did you really see nothing tangential — typos, slow loads, console warnings,"
        );
        eprintln!("   layout quirks, surprises? If you did, run:");
        eprintln!("     testito jot --run \"{}\" --text \"...\"", a.run);
        eprintln!("   Filing has zero cost; silence has real cost (the user has to ask again).");
        eprintln!();
    }

    println!(
        "run {} completed · {} · 📋 {} finding{}",
        a.run,
        status,
        findings,
        if findings == 1 { "" } else { "s" }
    );
    if a.fail_if_failures && run_rollup == Some(TestResult::Fail) {
        // Distinct exit code so callers can branch on "test failure" vs "tool error".
        std::process::exit(1);
    }
    Ok(())
}

fn plural(n: i64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn compute_run_rollup(db: &Db, run_id: i64) -> Result<Option<TestResult>> {
    let mut all_latest: Vec<RunStep> = Vec::new();
    for t in db.run_tests(run_id)? {
        for s in db.steps_for_test(t.id)? {
            all_latest.push(s);
        }
    }
    Ok(rollup(&all_latest))
}

fn cmd_list(db_path: PathBuf, a: ListArgs) -> Result<()> {
    let db = Db::open(&db_path)?;
    let mut runs = db.list_runs()?;
    runs.truncate(a.limit);

    if a.json {
        // A small ad-hoc JSON shape — keeps callers from needing to model Run.
        let arr = runs
            .iter()
            .map(|r| {
                let rollup = compute_run_rollup(&db, r.id).unwrap_or(None);
                serde_json::json!({
                    "id": r.id,
                    "name": r.name,
                    "description": r.description,
                    "branch": r.branch,
                    "commit": r.commit_sha,
                    "env": r.env,
                    "url": r.url,
                    "started_at": r.started_at,
                    "completed_at": r.completed_at,
                    "tests": r.test_count,
                    "steps": r.step_count,
                    "notes": r.note_count,
                    "rollup": rollup.map(|r| r.as_str()),
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::Value::Array(arr))?
        );
        return Ok(());
    }

    if runs.is_empty() {
        println!("(no runs)");
        return Ok(());
    }

    // Table layout: name, status, rollup, tests, steps, started.
    let name_w = runs.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    let header = format!(
        "{:<name_w$}  {:<10}  {:<8}  {:>5}  {:>5}  {}",
        "NAME",
        "STATUS",
        "ROLLUP",
        "TESTS",
        "STEPS",
        "STARTED",
        name_w = name_w,
    );
    println!("{header}");
    for r in &runs {
        let status = if r.completed_at.is_some() {
            "completed"
        } else {
            "running"
        };
        let rollup_str = compute_run_rollup(&db, r.id)?
            .map(|r| r.as_str())
            .unwrap_or("-");
        println!(
            "{:<name_w$}  {:<10}  {:<8}  {:>5}  {:>5}  {}",
            r.name,
            status,
            rollup_str,
            r.test_count,
            r.step_count,
            relative_time(&r.started_at),
            name_w = name_w,
        );
    }
    Ok(())
}

fn cmd_show(db_path: PathBuf, a: ShowArgs) -> Result<()> {
    let db = Db::open(&db_path)?;
    let run_id = db
        .find_run_id(&a.run)?
        .ok_or_else(|| anyhow::anyhow!("run '{}' does not exist", a.run))?;
    let run = db
        .get_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("run disappeared mid-query"))?;

    // Aggregate counts + per-test latest-attempt steps, in one pass.
    let mut counts = (0i64, 0i64, 0i64, 0i64); // (pass, fail, warn, skip)
    let mut tests_with_steps = Vec::new();
    let mut all_latest_steps = Vec::new();
    for t in db.run_tests(run_id)? {
        let steps = db.steps_for_test(t.id)?;
        for s in &steps {
            match s.result {
                TestResult::Pass => counts.0 += 1,
                TestResult::Fail => counts.1 += 1,
                TestResult::Warning => counts.2 += 1,
                TestResult::Skipped => counts.3 += 1,
            }
            all_latest_steps.push(s.clone());
        }
        let test_rollup = rollup(&steps);
        tests_with_steps.push((t, steps, test_rollup));
    }
    let run_rollup = rollup(&all_latest_steps);
    let notes = db.notes_for_run(run_id)?;

    if a.json {
        let payload = serde_json::json!({
            "name": run.name,
            "description": run.description,
            "branch": run.branch,
            "commit": run.commit_sha,
            "env": run.env,
            "url": run.url,
            "started_at": run.started_at,
            "completed_at": run.completed_at,
            "rollup": run_rollup.map(|r| r.as_str()),
            "counts": {
                "pass": counts.0,
                "fail": counts.1,
                "warning": counts.2,
                "skipped": counts.3,
            },
            "tests": tests_with_steps.iter().map(|(t, steps, r)| serde_json::json!({
                "name": t.name,
                "rollup": r.map(|r| r.as_str()),
                "steps": steps.iter().map(|s| serde_json::json!({
                    "name": s.name,
                    "attempt": s.attempt,
                    "result": s.result.as_str(),
                    "note": s.note,
                    "reported_at": s.reported_at,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "notes": notes.iter().map(|n| serde_json::json!({
                "scope": n.scope.as_str(),
                "text": n.text,
                "reported_at": n.reported_at,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!(
        "Run: {}{}",
        run.name,
        run_rollup
            .map(|r| format!("  [{}]", r.label()))
            .unwrap_or_default()
    );
    if !run.description.is_empty() {
        println!("{}", run.description);
    }
    let meta_lines: Vec<(&str, &str)> = [
        ("Branch", run.branch.as_str()),
        ("Commit", run.commit_sha.as_str()),
        ("Env", run.env.as_str()),
        ("URL", run.url.as_str()),
    ]
    .into_iter()
    .filter(|(_, v)| !v.is_empty())
    .collect();
    if !meta_lines.is_empty() {
        println!();
        for (k, v) in meta_lines {
            println!("  {k}: {v}");
        }
    }
    println!();
    println!(
        "Started {} · {}",
        relative_time(&run.started_at),
        run.completed_at
            .as_deref()
            .map_or("in progress", |_| "completed")
    );
    println!(
        "Counts: {} pass · {} fail · {} warn · {} skip",
        counts.0, counts.1, counts.2, counts.3
    );
    // Per-kind breakdown — bugs first so they hit the eye.
    let mut by_kind: [i64; 4] = [0; 4];
    for n in &notes {
        by_kind[n.kind.sort_priority() as usize] += 1;
    }
    if notes.is_empty() {
        println!("Findings: ✓ none filed");
    } else {
        let mut parts = Vec::new();
        if by_kind[0] > 0 {
            parts.push(format!("🐛 {} bug{}", by_kind[0], plural(by_kind[0])));
        }
        if by_kind[1] > 0 {
            parts.push(format!("✏️ {} polish", by_kind[1]));
        }
        if by_kind[2] > 0 {
            parts.push(format!("❓ {} question{}", by_kind[2], plural(by_kind[2])));
        }
        if by_kind[3] > 0 {
            parts.push(format!("ℹ️  {} info", by_kind[3]));
        }
        println!("Findings: {}", parts.join(" · "));
    }

    // Findings & notes — surfaced before the long test list because the agent's
    // tangential observations are usually what the human actually wants to read.
    // Sorted by triage priority (bugs first), then chronological within each kind.
    if !notes.is_empty() {
        let mut sorted_notes = notes.clone();
        sorted_notes.sort_by_key(|n| n.kind.sort_priority());
        println!();
        println!("Findings & notes:");
        for n in &sorted_notes {
            let scope_tag = match n.scope {
                Scope::In => "in",
                Scope::Out => "out",
            };
            print!("  {} {} [{}] ", n.kind.emoji(), n.kind.label(), scope_tag);
            let mut lines = n.text.lines();
            if let Some(first) = lines.next() {
                println!("{first}");
            } else {
                println!();
            }
            for line in lines {
                println!("              {line}");
            }
        }
    }

    let failing_steps: Vec<_> = tests_with_steps
        .iter()
        .flat_map(|(t, steps, _)| {
            steps
                .iter()
                .filter(|s| s.result == TestResult::Fail)
                .map(move |s| (t.name.as_str(), s))
        })
        .collect();
    if !failing_steps.is_empty() {
        println!();
        println!("Failures:");
        for (test, s) in failing_steps {
            print!("  ✗ {test} / {}", s.name);
            if s.attempt > 1 {
                print!(" (attempt {})", s.attempt);
            }
            println!();
            if !s.note.is_empty() {
                for line in s.note.lines() {
                    println!("      {line}");
                }
            }
        }
    }

    println!();
    println!("Tests:");
    for (t, steps, t_rollup) in &tests_with_steps {
        let icon = match t_rollup {
            Some(TestResult::Pass) => "✓",
            Some(TestResult::Fail) => "✗",
            Some(TestResult::Warning) => "⚠",
            Some(TestResult::Skipped) => "⊘",
            None => "·",
        };
        println!(
            "  {icon} {} ({} step{})",
            t.name,
            steps.len(),
            if steps.len() == 1 { "" } else { "s" }
        );
    }

    Ok(())
}

fn default_db_path() -> PathBuf {
    if let Some(dirs) = directories::ProjectDirs::from("dev", "testito", "testito") {
        dirs.data_dir().join("testito.db")
    } else {
        PathBuf::from("testito.db")
    }
}
