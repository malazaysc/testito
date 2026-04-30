use axum::{
    extract::{Path as AxPath, Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Form, Router,
};
use serde::Deserialize;

use askama::Template;

use crate::md;
use crate::models::{
    relative_time, rollup, Attachment, Feedback, FeedbackTarget, Result as TestResult, Review, Run,
    RunNote, RunStep, RunTest,
};
use crate::storage;
use crate::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/runs", get(runs_fragment))
        .route("/runs/:id", get(run_page))
        .route("/runs/:id/body", get(run_body_fragment))
        .route("/runs/:id/export.md", get(export_markdown))
        .route("/compare", get(compare_page))
        .route("/compare/test", get(compare_test_fragment))
        .route("/screenshots/:filename", get(serve_screenshot))
        .route("/feedback", post(post_feedback))
        .with_state(state)
}

#[derive(serde::Deserialize)]
struct PostFeedbackForm {
    run_id: i64,
    target_kind: String,
    target_id: i64,
    text: String,
}

async fn post_feedback(
    State(state): State<AppState>,
    Form(form): Form<PostFeedbackForm>,
) -> Result<impl IntoResponse, AppError> {
    let target_kind = FeedbackTarget::parse(&form.target_kind)?;
    let text = form.text.trim();
    if text.is_empty() {
        return Err(AppError::Other(anyhow::anyhow!("feedback text required")));
    }
    let db = state.db.lock().await;
    // Validate the target belongs to the named run — trust nothing from the
    // form. (The dashboard is local but defense in depth is cheap.)
    let belongs = match target_kind {
        FeedbackTarget::Note => db
            .conn
            .query_row::<i64, _, _>(
                "SELECT 1 FROM run_notes WHERE id = ?1 AND run_id = ?2",
                rusqlite::params![form.target_id, form.run_id],
                |r| r.get(0),
            )
            .is_ok(),
        FeedbackTarget::Test => db
            .conn
            .query_row::<i64, _, _>(
                "SELECT 1 FROM run_tests WHERE id = ?1 AND run_id = ?2",
                rusqlite::params![form.target_id, form.run_id],
                |r| r.get(0),
            )
            .is_ok(),
        FeedbackTarget::Run => form.target_id == form.run_id,
    };
    if !belongs {
        return Err(AppError::NotFound(format!(
            "{} {} in run {}",
            target_kind.as_str(),
            form.target_id,
            form.run_id
        )));
    }
    let id = db.insert_feedback(form.run_id, target_kind, form.target_id, text)?;
    let f = Feedback {
        id,
        run_id: form.run_id,
        target_kind,
        target_id: form.target_id,
        text: text.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        seen_at: None,
    };
    Ok(render(FeedbackItemTpl::from(&f)))
}

async fn serve_screenshot(AxPath(filename): AxPath<String>) -> Response {
    let (path, mime) = match storage::open_for_serving(&filename) {
        Ok(v) => v,
        Err(_) => return (StatusCode::NOT_FOUND, "screenshot not found").into_response(),
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "public, max-age=3600, immutable"),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "screenshot not found").into_response(),
    }
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
    /// Per-kind breakdown of notes — drives the Findings column on the home table.
    kind_counts: KindCounts,
}

#[derive(Default, Clone, Copy)]
struct KindCounts {
    bug: i64,
    polish: i64,
    question: i64,
    info: i64,
}

impl KindCounts {
    fn add(&mut self, k: crate::models::Kind) {
        match k {
            crate::models::Kind::Bug => self.bug += 1,
            crate::models::Kind::Polish => self.polish += 1,
            crate::models::Kind::Question => self.question += 1,
            crate::models::Kind::Info => self.info += 1,
        }
    }
    fn total(&self) -> i64 {
        self.bug + self.polish + self.question + self.info
    }
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
    attachments: Vec<Attachment>,
}

struct NoteRow {
    note: RunNote,
    text_html: String,
    relative_reported: String,
    /// Precomputed "[Kind] body" string for one-click copy. Kept server-side
    /// so the template doesn't need to assemble it (and we don't ship the
    /// whole notes array to the client just for clipboard).
    copy_text: String,
    attachments: Vec<Attachment>,
    feedback: Vec<FeedbackItemTpl>,
}

struct TestRow {
    test: RunTest,
    rollup: Option<TestResult>,
    rollup_str: String,
    steps: Vec<StepRow>,
    counts: ResultCounts,
    feedback: Vec<FeedbackItemTpl>,
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

struct ReviewRow {
    review: Review,
    text_html: String,
    relative_created: String,
}

#[derive(Template)]
#[template(path = "run.html")]
struct RunTpl {
    run: Run,
    description_html: String,
    relative_started: String,
    relative_completed: Option<String>,
    tests: Vec<TestRow>,
    notes: Vec<NoteRow>,
    reviews: Vec<ReviewRow>,
    counts: ResultCounts,
    rollup: Option<TestResult>,
    other_runs: Vec<Run>,
    findings: i64,
    kind_counts: KindCounts,
    pr_summary_md: String,
    pr_summary_html: String,
}

#[derive(Template)]
#[template(path = "run_body.html")]
struct RunBodyTpl {
    run: Run,
    description_html: String,
    relative_started: String,
    relative_completed: Option<String>,
    tests: Vec<TestRow>,
    notes: Vec<NoteRow>,
    reviews: Vec<ReviewRow>,
    counts: ResultCounts,
    rollup: Option<TestResult>,
    findings: i64,
    kind_counts: KindCounts,
    pr_summary_md: String,
    pr_summary_html: String,
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
    test_name_encoded: String,
    a_status: Option<TestResult>,
    b_status: Option<TestResult>,
    differs: bool,
}

#[derive(Template)]
#[template(path = "compare_test.html")]
struct CompareTestTpl {
    rows: Vec<StepDiffRow>,
}

#[derive(Template)]
#[template(path = "feedback_item.html")]
struct FeedbackItemTpl {
    feedback: Feedback,
    text_html: String,
    relative_created: String,
}

impl FeedbackItemTpl {
    fn from(f: &Feedback) -> Self {
        Self {
            text_html: md::to_html(&f.text),
            relative_created: relative_time(&f.created_at),
            feedback: f.clone(),
        }
    }
}

struct StepDiffRow {
    name: String,
    a_status: Option<TestResult>,
    b_status: Option<TestResult>,
    a_attempt: i64, // 0 = step did not exist on this side
    b_attempt: i64,
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
        let notes_for_r = db.notes_for_run(r.id)?;
        let mut kind_counts = KindCounts::default();
        for n in &notes_for_r {
            kind_counts.add(n.kind);
        }
        out.push(RunRow {
            run: r,
            relative_started,
            rollup,
            counts,
            kind_counts,
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
        description_html: body.description_html,
        relative_started: body.relative_started,
        relative_completed: body.relative_completed,
        tests: body.tests,
        notes: body.notes,
        counts: body.counts,
        rollup: body.rollup,
        other_runs,
        findings: body.findings,
        kind_counts: body.kind_counts,
        pr_summary_md: body.pr_summary_md,
        pr_summary_html: body.pr_summary_html,
        reviews: body.reviews,
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

    // One batched fetch for all attachments on this run, indexed by
    // (target_kind, target_id) so the per-step / per-note loops are O(1).
    let mut attachments_map = db.attachments_for_run(id)?;

    // Same idea for feedback — one fetch, group by target.
    let all_feedback = db.feedback_for_run(id)?;
    let mut feedback_by_note: std::collections::HashMap<i64, Vec<FeedbackItemTpl>> =
        std::collections::HashMap::new();
    let mut feedback_by_test: std::collections::HashMap<i64, Vec<FeedbackItemTpl>> =
        std::collections::HashMap::new();
    for f in &all_feedback {
        let item = FeedbackItemTpl::from(f);
        match f.target_kind {
            FeedbackTarget::Note => feedback_by_note.entry(f.target_id).or_default().push(item),
            FeedbackTarget::Test => feedback_by_test.entry(f.target_id).or_default().push(item),
            FeedbackTarget::Run => {} // not surfaced inline yet
        }
    }

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
            let attachments = attachments_map
                .remove(&("step".to_string(), s.id))
                .unwrap_or_default();
            steps.push(StepRow {
                step: s.clone(),
                note_html,
                relative_reported,
                attachments,
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
        let feedback = feedback_by_test.remove(&t.id).unwrap_or_default();
        tests_out.push(TestRow {
            test: t,
            rollup: rollup_status,
            rollup_str,
            steps,
            counts: t_counts,
            feedback,
        });
    }

    let mut notes_raw = db.notes_for_run(id)?;
    // Sort by triage priority (bugs first, polish, question, info), then
    // chronologically within each kind. Stable sort over a chronologically-
    // ordered list preserves time order inside each bucket.
    notes_raw.sort_by_key(|n| n.kind.sort_priority());
    let findings = notes_raw
        .iter()
        .filter(|n| matches!(n.scope, crate::models::Scope::Out))
        .count() as i64;
    let mut kind_counts = KindCounts::default();
    for n in &notes_raw {
        kind_counts.add(n.kind);
    }
    let notes: Vec<NoteRow> = notes_raw
        .into_iter()
        .map(|n| {
            let attachments = attachments_map
                .remove(&("note".to_string(), n.id))
                .unwrap_or_default();
            let feedback = feedback_by_note.remove(&n.id).unwrap_or_default();
            NoteRow {
                text_html: md::to_html(&n.text),
                relative_reported: relative_time(&n.reported_at),
                copy_text: format!("[{}] {}", n.kind.label(), n.text),
                attachments,
                feedback,
                note: n,
            }
        })
        .collect();

    let run_rollup = rollup(&all_latest);
    let reviews: Vec<ReviewRow> = db
        .reviews_for_run(id)?
        .into_iter()
        .map(|r| ReviewRow {
            text_html: md::to_html(&r.text),
            relative_created: relative_time(&r.created_at),
            review: r,
        })
        .collect();

    let pr_summary_md = render_pr_summary(&run, run_rollup, &counts, &notes, &tests_out, &reviews);
    let pr_summary_html = md::to_html(&pr_summary_md);

    let description_html = if run.description.is_empty() {
        String::new()
    } else {
        md::to_html(&run.description)
    };

    Ok(RunBodyTpl {
        run,
        description_html,
        relative_started,
        relative_completed,
        tests: tests_out,
        notes,
        counts,
        rollup: run_rollup,
        findings,
        kind_counts,
        pr_summary_md,
        pr_summary_html,
        reviews,
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

/// A tight one-block summary suitable for pasting into a PR description or
/// review comment. Shorter and flatter than the full export — leads with the
/// rollup, surfaces findings (sorted by kind), then any failing steps.
fn render_pr_summary(
    run: &Run,
    rollup: Option<TestResult>,
    counts: &ResultCounts,
    notes: &[NoteRow],
    tests: &[TestRow],
    reviews: &[ReviewRow],
) -> String {
    let mut s = String::new();
    let rollup_label = rollup.map(|r| r.label()).unwrap_or("no steps");
    s.push_str(&format!("## QA: {}  [{}]\n", run.name, rollup_label));
    if !run.description.is_empty() {
        s.push_str(&format!("\n_{}_\n", run.description));
    }

    let mut meta_bits: Vec<String> = Vec::new();
    if !run.branch.is_empty() {
        meta_bits.push(format!("branch `{}`", run.branch));
    }
    if !run.commit_sha.is_empty() {
        meta_bits.push(format!("commit `{}`", run.commit_sha));
    }
    if let Some(n) = run.pr_number {
        if run.pr_url_is_safe() {
            meta_bits.push(format!("PR [#{}]({})", n, run.pr_url));
        } else {
            meta_bits.push(format!("PR `#{}`", n));
        }
    }
    if !run.env.is_empty() {
        meta_bits.push(format!("env `{}`", run.env));
    }
    if !run.url.is_empty() {
        meta_bits.push(format!("url <{}>", run.url));
    }
    if !run.workdir.is_empty() {
        meta_bits.push(format!("workdir `{}`", run.workdir));
    }
    if !meta_bits.is_empty() {
        s.push_str(&format!("\n{}\n", meta_bits.join(" · ")));
    }

    s.push_str(&format!(
        "\n**Steps**: {} pass · {} fail · {} warn · {} skip\n",
        counts.pass, counts.fail, counts.warning, counts.skipped
    ));

    if !reviews.is_empty() {
        s.push_str(&format!("\n### Reviews ({})\n\n", reviews.len()));
        for r in reviews {
            s.push_str(&format!(
                "- {} **{} — {}**: {}\n",
                r.review.kind.emoji(),
                r.review.kind.label(),
                r.review.verdict.label(),
                r.review.text.replace('\n', " ").trim()
            ));
        }
    }

    if !notes.is_empty() {
        s.push_str(&format!("\n### Findings ({})\n\n", notes.len()));
        for n in notes {
            let icon = n.note.kind.emoji();
            // First line of body becomes the headline; collapse newlines so
            // the bullet stays on one line in the rendered PR comment.
            let one_line = n.note.text.replace('\n', " ").trim().to_string();
            s.push_str(&format!(
                "- {} **{}**: {}\n",
                icon,
                n.note.kind.label(),
                one_line
            ));
        }
    }

    // Failures (only the latest-failing attempts).
    let failing: Vec<(&str, &StepRow)> = tests
        .iter()
        .flat_map(|t| {
            t.steps
                .iter()
                .filter(|s| matches!(s.step.result, TestResult::Fail))
                .map(move |s| (t.test.name.as_str(), s))
        })
        .collect();
    if !failing.is_empty() {
        s.push_str(&format!("\n### Failures ({})\n\n", failing.len()));
        for (test, sr) in failing {
            let suffix = if sr.step.attempt > 1 {
                format!(" _(attempt {})_", sr.step.attempt)
            } else {
                String::new()
            };
            let body = if sr.step.note.is_empty() {
                "no note".to_string()
            } else {
                sr.step.note.replace('\n', " ").trim().to_string()
            };
            s.push_str(&format!(
                "- **{}** / {}{}: {}\n",
                test, sr.step.name, suffix, body
            ));
        }
    }

    s
}

fn render_markdown_export(b: &RunBodyTpl) -> String {
    let mut s = String::new();
    s.push_str(&format!("# Run: {}\n\n", b.run.name));
    if !b.run.description.is_empty() {
        s.push_str(&format!("> {}\n\n", b.run.description));
    }

    // Metadata block
    let pr_str = b.run.pr_number.map(|n| format!("#{n}")).unwrap_or_default();
    let meta_lines: Vec<String> = [
        ("Branch", b.run.branch.as_str()),
        ("Commit", b.run.commit_sha.as_str()),
        ("PR", pr_str.as_str()),
        ("Env", b.run.env.as_str()),
        ("URL", b.run.url.as_str()),
        ("Workdir", b.run.workdir.as_str()),
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

    if !b.reviews.is_empty() {
        s.push_str(&format!("## Reviews ({})\n\n", b.reviews.len()));
        for r in &b.reviews {
            s.push_str(&format!(
                "### {} {} — {}\n\n",
                r.review.kind.emoji(),
                r.review.kind.label(),
                r.review.verdict.label(),
            ));
            s.push_str(&r.review.text);
            s.push_str("\n\n");
        }
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
        "- **Counts**: {} pass · {} fail · {} warn · {} skip\n",
        b.counts.pass, b.counts.fail, b.counts.warning, b.counts.skipped
    ));
    if b.findings > 0 {
        s.push_str(&format!(
            "- **Findings**: 📋 {} observation{} filed outside the test brief — review them.\n",
            b.findings,
            if b.findings == 1 { "" } else { "s" }
        ));
    }
    s.push('\n');

    // Findings & notes — surfaced before tests because they're usually what
    // the human reviewer should read first.
    if !b.notes.is_empty() {
        s.push_str(&format!("## Findings & notes ({})\n\n", b.notes.len()));
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

    // Failures
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
            let test_name_encoded = urlencoding::encode(&name).into_owned();
            CompareRow {
                differs: a_status != b_status,
                test_name: name,
                test_name_encoded,
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

#[derive(Deserialize)]
struct CompareTestQuery {
    a: i64,
    b: i64,
    test: String,
}

async fn compare_test_fragment(
    State(state): State<AppState>,
    Query(q): Query<CompareTestQuery>,
) -> Result<impl IntoResponse, AppError> {
    let db = state.db.lock().await;
    let a_steps = steps_for_named_test(&db, q.a, &q.test)?;
    let b_steps = steps_for_named_test(&db, q.b, &q.test)?;
    let rows = step_diff_rows(&a_steps, &b_steps);
    Ok(render(CompareTestTpl { rows }))
}

/// Look up the run-test row by `(run_id, name)` and return its raw step rows
/// (all attempts). Returns an empty Vec if the test does not exist in the run.
fn steps_for_named_test(
    db: &crate::db::Db,
    run_id: i64,
    name: &str,
) -> anyhow::Result<Vec<RunStep>> {
    for t in db.run_tests(run_id)? {
        if t.name == name {
            return db.steps_for_test(t.id);
        }
    }
    Ok(Vec::new())
}

/// Merge step rows from two runs of the same logical test, comparing the
/// latest attempt of each step name. Returns one row per distinct step name.
fn step_diff_rows(a: &[RunStep], b: &[RunStep]) -> Vec<StepDiffRow> {
    use std::collections::BTreeMap;

    fn latest_by_name(steps: &[RunStep]) -> BTreeMap<&str, &RunStep> {
        let mut out: BTreeMap<&str, &RunStep> = BTreeMap::new();
        for s in steps {
            match out.get(s.name.as_str()) {
                Some(existing) if existing.attempt >= s.attempt => {}
                _ => {
                    out.insert(&s.name, s);
                }
            }
        }
        out
    }

    let a_latest = latest_by_name(a);
    let b_latest = latest_by_name(b);
    let mut names: std::collections::BTreeSet<&str> = a_latest.keys().copied().collect();
    names.extend(b_latest.keys().copied());

    names
        .into_iter()
        .map(|name| {
            let a_step = a_latest.get(name).copied();
            let b_step = b_latest.get(name).copied();
            let a_status = a_step.map(|s| s.result);
            let b_status = b_step.map(|s| s.result);
            StepDiffRow {
                name: name.to_string(),
                a_attempt: a_step.map(|s| s.attempt).unwrap_or(0),
                b_attempt: b_step.map(|s| s.attempt).unwrap_or(0),
                differs: a_status != b_status,
                a_status,
                b_status,
            }
        })
        .collect()
}

fn render<T: Template>(t: T) -> Html<String> {
    match t.render() {
        Ok(s) => Html(s),
        Err(e) => Html(format!("<pre>template error: {}</pre>", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::{step_diff_rows, RunStep};
    use crate::models::Result as TestResult;

    fn step(name: &str, attempt: i64, result: TestResult) -> RunStep {
        RunStep {
            id: 0,
            run_test_id: 0,
            name: name.to_string(),
            attempt,
            result,
            note: String::new(),
            reported_at: String::new(),
        }
    }

    #[test]
    fn step_diff_uses_latest_attempt_per_side() {
        // A: submit fail → pass. B: submit pass.
        // Latest on both sides is Pass → not a diff.
        let a = vec![
            step("submit", 1, TestResult::Fail),
            step("submit", 2, TestResult::Pass),
        ];
        let b = vec![step("submit", 1, TestResult::Pass)];
        let rows = step_diff_rows(&a, &b);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.name, "submit");
        assert_eq!(r.a_status, Some(TestResult::Pass));
        assert_eq!(r.b_status, Some(TestResult::Pass));
        assert_eq!(r.a_attempt, 2);
        assert_eq!(r.b_attempt, 1);
        assert!(!r.differs);
    }

    #[test]
    fn step_diff_marks_one_sided_steps() {
        let a = vec![step("only-a", 1, TestResult::Pass)];
        let b = vec![step("only-b", 1, TestResult::Pass)];
        let rows = step_diff_rows(&a, &b);
        assert_eq!(rows.len(), 2);
        let row_a = rows.iter().find(|r| r.name == "only-a").unwrap();
        assert_eq!(row_a.a_status, Some(TestResult::Pass));
        assert_eq!(row_a.b_status, None);
        assert_eq!(row_a.b_attempt, 0);
        assert!(row_a.differs);
        let row_b = rows.iter().find(|r| r.name == "only-b").unwrap();
        assert_eq!(row_b.a_status, None);
        assert_eq!(row_b.a_attempt, 0);
        assert_eq!(row_b.b_status, Some(TestResult::Pass));
        assert!(row_b.differs);
    }

    #[test]
    fn step_diff_flags_status_change() {
        let a = vec![step("nav", 1, TestResult::Warning)];
        let b = vec![step("nav", 1, TestResult::Pass)];
        let rows = step_diff_rows(&a, &b);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].differs);
    }

    #[test]
    fn step_diff_empty_both_sides() {
        let rows = step_diff_rows(&[], &[]);
        assert!(rows.is_empty());
    }
}
