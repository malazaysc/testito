use axum::{
    extract::{Path as AxPath, Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use serde::Deserialize;

use askama::Template;

use crate::md;
use crate::models::{relative_time, rollup, Result as TestResult, Run, RunNote, RunStep, RunTest};
use crate::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/runs", get(runs_fragment))
        .route("/runs/:id", get(run_page))
        .route("/runs/:id/body", get(run_body_fragment))
        .route("/runs/:id/export.md", get(export_markdown))
        .route("/compare", get(compare_page))
        .with_state(state)
}

// ---------- error wrapper ----------

pub enum AppError {
    NotFound(String),
    Other(anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::NotFound(what) => {
                (StatusCode::NOT_FOUND, format!("{} not found", what)).into_response()
            }
            AppError::Other(e) => {
                tracing::error!("{:#}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, format!("error: {:#}", e)).into_response()
            }
        }
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Other(e)
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Other(e.into())
    }
}

// ---------- view models ----------

struct RunRow {
    run: Run,
    relative_started: String,
    rollup: Option<TestResult>,
    counts: ResultCounts,
}

#[derive(Default, Clone)]
struct ResultCounts {
    pass: i64,
    fail: i64,
    warning: i64,
    skipped: i64,
}

impl ResultCounts {
    fn add(&mut self, r: TestResult) {
        match r {
            TestResult::Pass => self.pass += 1,
            TestResult::Fail => self.fail += 1,
            TestResult::Warning => self.warning += 1,
            TestResult::Skipped => self.skipped += 1,
        }
    }
}

struct StepRow {
    step: RunStep,
    note_html: String,
    relative_reported: String,
}

struct NoteRow {
    note: RunNote,
    text_html: String,
    relative_reported: String,
}

struct TestRow {
    test: RunTest,
    rollup: Option<TestResult>,
    rollup_str: String,
    steps: Vec<StepRow>,
    counts: ResultCounts,
}

// ---------- templates ----------

#[derive(Template)]
#[template(path = "home.html")]
struct HomeTpl {
    rows: Vec<RunRow>,
}

#[derive(Template)]
#[template(path = "runs_table.html")]
struct RunsTableTpl {
    rows: Vec<RunRow>,
}

#[derive(Template)]
#[template(path = "run.html")]
struct RunTpl {
    run: Run,
    relative_started: String,
    relative_completed: Option<String>,
    tests: Vec<TestRow>,
    notes: Vec<NoteRow>,
    counts: ResultCounts,
    rollup: Option<TestResult>,
    other_runs: Vec<Run>,
}

#[derive(Template)]
#[template(path = "run_body.html")]
struct RunBodyTpl {
    run: Run,
    relative_started: String,
    relative_completed: Option<String>,
    tests: Vec<TestRow>,
    notes: Vec<NoteRow>,
    counts: ResultCounts,
    rollup: Option<TestResult>,
}

#[derive(Template)]
#[template(path = "compare.html")]
struct CompareTpl {
    a: Run,
    b: Run,
    rows: Vec<CompareRow>,
    available: Vec<Run>,
}

struct CompareRow {
    test_name: String,
    a_status: Option<TestResult>,
    b_status: Option<TestResult>,
    differs: bool,
}

// ---------- handlers ----------

async fn home(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let rows = build_run_rows(&state).await?;
    Ok(render(HomeTpl { rows }))
}

async fn runs_fragment(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let rows = build_run_rows(&state).await?;
    Ok(render(RunsTableTpl { rows }))
}

async fn build_run_rows(state: &AppState) -> Result<Vec<RunRow>, AppError> {
    let db = state.db.lock().await;
    let runs = db.list_runs()?;
    let mut out = Vec::with_capacity(runs.len());
    for r in runs {
        let mut counts = ResultCounts::default();
        let mut all_steps: Vec<RunStep> = Vec::new();
        for t in db.run_tests(r.id)? {
            for s in db.steps_for_test(t.id)? {
                counts.add(s.result);
                all_steps.push(s);
            }
        }
        let rollup = rollup(&all_steps);
        let relative_started = relative_time(&r.started_at);
        out.push(RunRow {
            run: r,
            relative_started,
            rollup,
            counts,
        });
    }
    Ok(out)
}

async fn run_page(
    State(state): State<AppState>,
    AxPath(id): AxPath<i64>,
) -> Result<impl IntoResponse, AppError> {
    let body = build_run_body(&state, id).await?;
    let other_runs: Vec<Run> = {
        let db = state.db.lock().await;
        db.list_runs()?
            .into_iter()
            .filter(|r| r.id != body.run.id)
            .collect()
    };
    Ok(render(RunTpl {
        run: body.run,
        relative_started: body.relative_started,
        relative_completed: body.relative_completed,
        tests: body.tests,
        notes: body.notes,
        counts: body.counts,
        rollup: body.rollup,
        other_runs,
    }))
}

async fn run_body_fragment(
    State(state): State<AppState>,
    AxPath(id): AxPath<i64>,
) -> Result<impl IntoResponse, AppError> {
    let body = build_run_body(&state, id).await?;
    Ok(render(body))
}

async fn build_run_body(state: &AppState, id: i64) -> Result<RunBodyTpl, AppError> {
    let db = state.db.lock().await;
    let run = db
        .get_run(id)?
        .ok_or_else(|| AppError::NotFound(format!("run {}", id)))?;
    let relative_started = relative_time(&run.started_at);
    let relative_completed = run.completed_at.as_deref().map(relative_time);

    let mut tests_out = Vec::new();
    let mut counts = ResultCounts::default();
    let mut all_latest: Vec<RunStep> = Vec::new();

    for t in db.run_tests(id)? {
        let raw_steps = db.steps_for_test(t.id)?;
        let mut t_counts = ResultCounts::default();
        let mut steps: Vec<StepRow> = Vec::with_capacity(raw_steps.len());
        for s in &raw_steps {
            counts.add(s.result);
            t_counts.add(s.result);
            let note_html = if s.note.is_empty() {
                String::new()
            } else {
                md::to_html(&s.note)
            };
            let relative_reported = relative_time(&s.reported_at);
            steps.push(StepRow {
                step: s.clone(),
                note_html,
                relative_reported,
            });
        }
        let rollup_status = rollup(&raw_steps);
        // accumulate latest-attempt steps for run rollup
        for s in &raw_steps {
            all_latest.push(s.clone());
        }
        let rollup_str = rollup_status
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| "none".to_string());
        tests_out.push(TestRow {
            test: t,
            rollup: rollup_status,
            rollup_str,
            steps,
            counts: t_counts,
        });
    }

    let notes_raw = db.notes_for_run(id)?;
    let notes: Vec<NoteRow> = notes_raw
        .into_iter()
        .map(|n| NoteRow {
            text_html: md::to_html(&n.text),
            relative_reported: relative_time(&n.reported_at),
            note: n,
        })
        .collect();

    let run_rollup = rollup(&all_latest);

    Ok(RunBodyTpl {
        run,
        relative_started,
        relative_completed,
        tests: tests_out,
        notes,
        counts,
        rollup: run_rollup,
    })
}

async fn export_markdown(
    State(state): State<AppState>,
    AxPath(id): AxPath<i64>,
) -> Result<impl IntoResponse, AppError> {
    let body = build_run_body(&state, id).await?;
    let md = render_markdown_export(&body);
    let filename = format!("{}.md", body.run.name.replace('/', "-"));
    let headers = [
        (
            header::CONTENT_TYPE,
            "text/markdown; charset=utf-8".to_string(),
        ),
        (
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        ),
    ];
    Ok((headers, md))
}

fn render_markdown_export(b: &RunBodyTpl) -> String {
    let mut s = String::new();
    s.push_str(&format!("# Run: {}\n\n", b.run.name));
    if !b.run.description.is_empty() {
        s.push_str(&format!("> {}\n\n", b.run.description));
    }

    // Metadata block
    let meta_lines: Vec<String> = [
        ("Branch", &b.run.branch),
        ("Commit", &b.run.commit_sha),
        ("Env", &b.run.env),
        ("URL", &b.run.url),
    ]
    .iter()
    .filter_map(|(k, v)| {
        if v.is_empty() {
            None
        } else {
            Some(format!("- **{}**: `{}`", k, v))
        }
    })
    .collect();
    if !meta_lines.is_empty() {
        s.push_str(&meta_lines.join("\n"));
        s.push_str("\n\n");
    }

    s.push_str(&format!("- **Started**: {}\n", b.run.started_at));
    if let Some(ref c) = b.run.completed_at {
        s.push_str(&format!("- **Completed**: {}\n", c));
    } else {
        s.push_str("- **Status**: in progress\n");
    }
    if let Some(r) = b.rollup {
        s.push_str(&format!("- **Rollup**: {}\n", r.label()));
    }
    s.push_str(&format!(
        "- **Counts**: {} pass · {} fail · {} warn · {} skip\n\n",
        b.counts.pass, b.counts.fail, b.counts.warning, b.counts.skipped
    ));

    // Failures first
    let mut has_failures = false;
    for t in &b.tests {
        for sr in &t.steps {
            if matches!(sr.step.result, TestResult::Fail) {
                if !has_failures {
                    s.push_str("## Failures\n\n");
                    has_failures = true;
                }
                s.push_str(&format!(
                    "- **{}** → _{}_ (attempt {}): {}\n",
                    t.test.name,
                    sr.step.name,
                    sr.step.attempt,
                    if sr.step.note.is_empty() {
                        "no note".to_string()
                    } else {
                        sr.step.note.replace('\n', " ")
                    }
                ));
            }
        }
    }
    if has_failures {
        s.push('\n');
    }

    // Tests + steps
    s.push_str("## Tests\n\n");
    for t in &b.tests {
        let tag = match t.rollup {
            Some(TestResult::Pass) => "✓",
            Some(TestResult::Fail) => "✗",
            Some(TestResult::Warning) => "⚠",
            Some(TestResult::Skipped) => "⊘",
            None => "·",
        };
        s.push_str(&format!("### {} {}\n\n", tag, t.test.name));
        for sr in &t.steps {
            let icon = match sr.step.result {
                TestResult::Pass => "✓",
                TestResult::Fail => "✗",
                TestResult::Warning => "⚠",
                TestResult::Skipped => "⊘",
            };
            let attempt = if sr.step.attempt > 1 {
                format!(" _(try #{}_)", sr.step.attempt)
            } else {
                String::new()
            };
            s.push_str(&format!("- {} {}{}\n", icon, sr.step.name, attempt));
            if !sr.step.note.is_empty() {
                for line in sr.step.note.lines() {
                    s.push_str(&format!("    > {}\n", line));
                }
            }
        }
        s.push('\n');
    }

    // Notes
    if !b.notes.is_empty() {
        s.push_str("## Notes\n\n");
        for n in &b.notes {
            let scope = match n.note.scope {
                crate::models::Scope::In => "in scope",
                crate::models::Scope::Out => "out of scope",
            };
            s.push_str(&format!(
                "- _({})_ {}\n",
                scope,
                n.note.text.replace('\n', "\n    ")
            ));
        }
        s.push('\n');
    }
    s
}

#[derive(Deserialize)]
struct CompareQuery {
    a: i64,
    b: i64,
}

async fn compare_page(
    State(state): State<AppState>,
    Query(q): Query<CompareQuery>,
) -> Result<impl IntoResponse, AppError> {
    let db = state.db.lock().await;
    let a = db
        .get_run(q.a)?
        .ok_or_else(|| AppError::NotFound(format!("run {}", q.a)))?;
    let b = db
        .get_run(q.b)?
        .ok_or_else(|| AppError::NotFound(format!("run {}", q.b)))?;

    let a_map = test_rollups(&db, a.id)?;
    let b_map = test_rollups(&db, b.id)?;

    let mut all_names: std::collections::BTreeSet<String> = a_map.keys().cloned().collect();
    all_names.extend(b_map.keys().cloned());

    let rows: Vec<CompareRow> = all_names
        .into_iter()
        .map(|name| {
            let a_status = a_map.get(&name).copied().flatten();
            let b_status = b_map.get(&name).copied().flatten();
            CompareRow {
                differs: a_status != b_status,
                test_name: name,
                a_status,
                b_status,
            }
        })
        .collect();

    let available = db.list_runs()?;

    Ok(render(CompareTpl {
        a,
        b,
        rows,
        available,
    }))
}

fn test_rollups(
    db: &crate::db::Db,
    run_id: i64,
) -> anyhow::Result<std::collections::HashMap<String, Option<TestResult>>> {
    let mut out = std::collections::HashMap::new();
    for t in db.run_tests(run_id)? {
        let steps = db.steps_for_test(t.id)?;
        out.insert(t.name, rollup(&steps));
    }
    Ok(out)
}

fn render<T: Template>(t: T) -> Html<String> {
    match t.render() {
        Ok(s) => Html(s),
        Err(e) => Html(format!("<pre>template error: {}</pre>", e)),
    }
}
