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

use db::Db;
use models::{Result as TestResult, RunMeta, Scope};

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

    /// Mark a run as completed.
    End(EndArgs),
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

    /// The note text.
    #[arg(long)]
    text: String,

    /// SQLite database file (default: platform data dir).
    #[arg(long)]
    db: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct EndArgs {
    /// Run name.
    #[arg(long)]
    run: String,

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
        Cmd::End(a) => {
            let db_path = resolve_db(a.db.clone())?;
            cmd_end(db_path, a)
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
    println!(
        "{} · run={} test={} step #{} attempt={}",
        result.label(),
        run_name,
        test_name,
        step_id,
        attempt
    );
    Ok(())
}

fn cmd_note(db_path: PathBuf, a: NoteArgs) -> Result<()> {
    let scope = Scope::parse(&a.scope)?;
    let db = Db::open(&db_path)?;
    let run_id = db.ensure_run(&a.run, &RunMeta::default())?;
    let id = db.append_note(run_id, scope, &a.text)?;
    println!("note #{} on {} ({})", id, a.run, scope.as_str());
    Ok(())
}

fn cmd_end(db_path: PathBuf, a: EndArgs) -> Result<()> {
    let db = Db::open(&db_path)?;
    let run_id = db
        .find_run_id(&a.run)?
        .ok_or_else(|| anyhow::anyhow!("run '{}' does not exist", a.run))?;
    db.complete_run(run_id)?;
    println!("run {} completed", a.run);
    Ok(())
}

fn default_db_path() -> PathBuf {
    if let Some(dirs) = directories::ProjectDirs::from("dev", "testito", "testito") {
        dirs.data_dir().join("testito.db")
    } else {
        PathBuf::from("testito.db")
    }
}
