use std::path::Path;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use crate::models::{
    Attachment, AttachmentTarget, Feedback, FeedbackTarget, Kind, Result as TestResult, Review,
    ReviewKind, ReviewVerdict, Run, RunMeta, RunNote, RunStep, RunTest, Scope,
};

pub struct Db {
    pub conn: Connection,
}

const SELECT_RUN_BASE_NO_ORDER: &str = "SELECT r.id, r.name, r.description, r.branch, r.commit_sha, r.env, r.url, r.workdir, r.pr_number, r.pr_url, r.started_at, r.completed_at,
                    (SELECT COUNT(*) FROM run_tests WHERE run_id = r.id) as test_count,
                    (SELECT COUNT(*) FROM run_steps s JOIN run_tests t ON s.run_test_id = t.id WHERE t.run_id = r.id) as step_count,
                    (SELECT COUNT(*) FROM run_notes WHERE run_id = r.id) as note_count
             FROM runs r";

const SELECT_RUN_BASE: &str = "SELECT r.id, r.name, r.description, r.branch, r.commit_sha, r.env, r.url, r.workdir, r.pr_number, r.pr_url, r.started_at, r.completed_at,
                    (SELECT COUNT(*) FROM run_tests WHERE run_id = r.id) as test_count,
                    (SELECT COUNT(*) FROM run_steps s JOIN run_tests t ON s.run_test_id = t.id WHERE t.run_id = r.id) as step_count,
                    (SELECT COUNT(*) FROM run_notes WHERE run_id = r.id) as note_count
             FROM runs r ORDER BY r.started_at DESC";

fn map_run(r: &rusqlite::Row) -> rusqlite::Result<Run> {
    Ok(Run {
        id: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        branch: r.get(3)?,
        commit_sha: r.get(4)?,
        env: r.get(5)?,
        url: r.get(6)?,
        workdir: r.get(7)?,
        pr_number: r.get(8)?,
        pr_url: r.get(9)?,
        started_at: r.get(10)?,
        completed_at: r.get(11)?,
        test_count: r.get(12)?,
        step_count: r.get(13)?,
        note_count: r.get(14)?,
    })
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    branch TEXT NOT NULL DEFAULT '',
    commit_sha TEXT NOT NULL DEFAULT '',
    env TEXT NOT NULL DEFAULT '',
    url TEXT NOT NULL DEFAULT '',
    workdir TEXT NOT NULL DEFAULT '',
    pr_number INTEGER,
    pr_url TEXT NOT NULL DEFAULT '',
    started_at TEXT NOT NULL,
    completed_at TEXT
);

CREATE TABLE IF NOT EXISTS run_tests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    first_reported_at TEXT NOT NULL,
    UNIQUE(run_id, name)
);

CREATE INDEX IF NOT EXISTS idx_run_tests_run ON run_tests(run_id);

CREATE TABLE IF NOT EXISTS run_steps (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_test_id INTEGER NOT NULL REFERENCES run_tests(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 1,
    result TEXT NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    reported_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_run_steps_test ON run_steps(run_test_id);

CREATE TABLE IF NOT EXISTS run_notes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    scope TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'info',
    text TEXT NOT NULL,
    reported_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_run_notes_run ON run_notes(run_id);

CREATE TABLE IF NOT EXISTS attachments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    target_kind TEXT NOT NULL,           -- 'note' | 'step'
    target_id INTEGER NOT NULL,
    filename TEXT NOT NULL,              -- '<sha256>.<ext>' on disk
    original_filename TEXT NOT NULL,     -- what the agent passed in (display only)
    mime_type TEXT NOT NULL,
    bytes INTEGER NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_attachments_target ON attachments(target_kind, target_id);

CREATE TABLE IF NOT EXISTS feedback (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    target_kind TEXT NOT NULL,           -- 'note' | 'test' | 'run'
    target_id INTEGER NOT NULL,
    text TEXT NOT NULL,
    created_at TEXT NOT NULL,
    seen_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_feedback_run ON feedback(run_id);

CREATE TABLE IF NOT EXISTS step_finding_refs (
    step_id INTEGER NOT NULL REFERENCES run_steps(id) ON DELETE CASCADE,
    note_id INTEGER NOT NULL REFERENCES run_notes(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    PRIMARY KEY (step_id, note_id)
);

CREATE INDEX IF NOT EXISTS idx_step_finding_refs_step ON step_finding_refs(step_id);
CREATE INDEX IF NOT EXISTS idx_step_finding_refs_note ON step_finding_refs(note_id);

CREATE TABLE IF NOT EXISTS reviews (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,        -- 'security' | 'code' | 'perf' | 'other'
    verdict TEXT NOT NULL,     -- 'clean' | 'advisory' | 'blocking'
    text TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_reviews_run ON reviews(run_id);
"#;

const DROP_LEGACY: &str = r#"
DROP TABLE IF EXISTS plan_tests;
DROP TABLE IF EXISTS plans;
DROP TABLE IF EXISTS steps;
DROP TABLE IF EXISTS tests;
DROP TABLE IF EXISTS files;
DROP TABLE IF EXISTS settings;
"#;

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
        )?;
        // Drop tables from the previous design if present.
        if has_legacy_table(&conn)? {
            conn.execute_batch(DROP_LEGACY)?;
            // Old runs/run_tests/run_steps schema differs — drop too.
            conn.execute_batch(
                "DROP TABLE IF EXISTS run_steps; DROP TABLE IF EXISTS run_tests; DROP TABLE IF EXISTS runs;",
            )?;
        }
        conn.execute_batch(SCHEMA)?;
        // Migrate older runs tables that lack metadata columns. ADD COLUMN is
        // a no-op semantically when the column already exists, but sqlite errors,
        // so we check first.
        for col in ["branch", "commit_sha", "env", "url", "workdir", "pr_url"] {
            if !column_exists(&conn, "runs", col)? {
                conn.execute(
                    &format!(
                        "ALTER TABLE runs ADD COLUMN {} TEXT NOT NULL DEFAULT ''",
                        col
                    ),
                    [],
                )?;
            }
        }
        // pr_number is nullable INTEGER (a PR number doesn't have a sensible
        // empty value, unlike a string field).
        if !column_exists(&conn, "runs", "pr_number")? {
            conn.execute("ALTER TABLE runs ADD COLUMN pr_number INTEGER", [])?;
        }
        // Migrate existing run_notes tables to add the kind column (default 'info'
        // so legacy notes show up as informational rather than triaged).
        if !column_exists(&conn, "run_notes", "kind")? {
            conn.execute(
                "ALTER TABLE run_notes ADD COLUMN kind TEXT NOT NULL DEFAULT 'info'",
                [],
            )?;
        }
        Ok(Self { conn })
    }

    pub fn ensure_run(&self, name: &str, meta: &RunMeta) -> Result<i64> {
        if let Some(id) = self.find_run_id(name)? {
            self.upsert_run_metadata(id, meta)?;
            return Ok(id);
        }
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO runs(name, description, branch, commit_sha, env, url, workdir, pr_number, pr_url, started_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                name,
                meta.description.as_deref().unwrap_or(""),
                meta.branch.as_deref().unwrap_or(""),
                meta.commit_sha.as_deref().unwrap_or(""),
                meta.env.as_deref().unwrap_or(""),
                meta.url.as_deref().unwrap_or(""),
                meta.workdir.as_deref().unwrap_or(""),
                meta.pr_number,
                meta.pr_url.as_deref().unwrap_or(""),
                now,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn upsert_run_metadata(&self, id: i64, meta: &RunMeta) -> Result<()> {
        // Each field is set only when the caller passed something non-empty.
        // This means re-running `report --branch X` keeps X; not passing it leaves
        // the prior value intact.
        let pairs: &[(&str, Option<&String>)] = &[
            ("description", meta.description.as_ref()),
            ("branch", meta.branch.as_ref()),
            ("commit_sha", meta.commit_sha.as_ref()),
            ("env", meta.env.as_ref()),
            ("url", meta.url.as_ref()),
            ("workdir", meta.workdir.as_ref()),
            ("pr_url", meta.pr_url.as_ref()),
        ];
        for (col, val) in pairs {
            if let Some(v) = val {
                if !v.is_empty() {
                    let sql = format!("UPDATE runs SET {} = ?1 WHERE id = ?2", col);
                    self.conn.execute(&sql, params![v, id])?;
                }
            }
        }
        if let Some(n) = meta.pr_number {
            self.conn.execute(
                "UPDATE runs SET pr_number = ?1 WHERE id = ?2",
                params![n, id],
            )?;
        }
        Ok(())
    }

    /// Returns runs matching the given branch and/or PR number, ordered
    /// most-recent first. Both filters are optional; passing neither returns
    /// an empty list (use `list_runs` for that). Empty strings are treated
    /// as "don't filter on this field" so the runs table doesn't surface
    /// for runs that simply have no branch set.
    pub fn find_runs_by_filter(&self, branch: Option<&str>, pr: Option<i64>) -> Result<Vec<Run>> {
        let branch = branch.filter(|b| !b.is_empty());
        if branch.is_none() && pr.is_none() {
            return Ok(vec![]);
        }
        let mut sql = String::from(SELECT_RUN_BASE_NO_ORDER);
        sql.push_str(" WHERE ");
        let mut clauses: Vec<&str> = Vec::new();
        if branch.is_some() {
            clauses.push("r.branch = ?1");
        }
        if pr.is_some() {
            clauses.push(if branch.is_some() {
                "r.pr_number = ?2"
            } else {
                "r.pr_number = ?1"
            });
        }
        sql.push_str(&clauses.join(" AND "));
        sql.push_str(" ORDER BY r.started_at DESC");

        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<Run> = match (branch, pr) {
            (Some(b), Some(p)) => stmt
                .query_map(params![b, p], map_run)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            (Some(b), None) => stmt
                .query_map(params![b], map_run)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            (None, Some(p)) => stmt
                .query_map(params![p], map_run)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            (None, None) => unreachable!(),
        };
        Ok(rows)
    }

    pub fn find_run_id(&self, name: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT id FROM runs WHERE name = ?1", params![name], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?)
    }

    pub fn ensure_test(&self, run_id: i64, name: &str) -> Result<i64> {
        if let Some(id) = self
            .conn
            .query_row(
                "SELECT id FROM run_tests WHERE run_id = ?1 AND name = ?2",
                params![run_id, name],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO run_tests(run_id, name, first_reported_at) VALUES(?1, ?2, ?3)",
            params![run_id, name, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn append_step(
        &self,
        run_test_id: i64,
        name: &str,
        attempt: i64,
        result: TestResult,
        note: &str,
    ) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO run_steps(run_test_id, name, attempt, result, note, reported_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![run_test_id, name, attempt, result.as_str(), note, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn append_note(&self, run_id: i64, scope: Scope, kind: Kind, text: &str) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO run_notes(run_id, scope, kind, text, reported_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![run_id, scope.as_str(), kind.as_str(), text, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn complete_run(&self, run_id: i64) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE runs SET completed_at = ?1 WHERE id = ?2",
            params![now, run_id],
        )?;
        Ok(())
    }

    pub fn list_runs(&self) -> Result<Vec<Run>> {
        let mut stmt = self.conn.prepare(SELECT_RUN_BASE)?;
        let rows = stmt
            .query_map([], map_run)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_run(&self, id: i64) -> Result<Option<Run>> {
        let sql = format!("{} WHERE r.id = ?1", SELECT_RUN_BASE_NO_ORDER);
        Ok(self.conn.query_row(&sql, params![id], map_run).optional()?)
    }

    pub fn run_tests(&self, run_id: i64) -> Result<Vec<RunTest>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, name, first_reported_at FROM run_tests
             WHERE run_id = ?1 ORDER BY first_reported_at, id",
        )?;
        let rows = stmt
            .query_map(params![run_id], |r| {
                Ok(RunTest {
                    id: r.get(0)?,
                    run_id: r.get(1)?,
                    name: r.get(2)?,
                    first_reported_at: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn steps_for_test(&self, run_test_id: i64) -> Result<Vec<RunStep>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_test_id, name, attempt, result, note, reported_at
             FROM run_steps WHERE run_test_id = ?1 ORDER BY reported_at, id",
        )?;
        let rows = stmt
            .query_map(params![run_test_id], |r| {
                Ok(RunStep {
                    id: r.get(0)?,
                    run_test_id: r.get(1)?,
                    name: r.get(2)?,
                    attempt: r.get(3)?,
                    result: TestResult::parse(&r.get::<_, String>(4)?)
                        .unwrap_or(TestResult::Skipped),
                    note: r.get(5)?,
                    reported_at: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn notes_for_run(&self, run_id: i64) -> Result<Vec<RunNote>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, scope, kind, text, reported_at FROM run_notes
             WHERE run_id = ?1 ORDER BY reported_at, id",
        )?;
        let rows = stmt
            .query_map(params![run_id], |r| {
                Ok(RunNote {
                    id: r.get(0)?,
                    run_id: r.get(1)?,
                    scope: Scope::parse(&r.get::<_, String>(2)?).unwrap_or(Scope::In),
                    kind: Kind::parse(&r.get::<_, String>(3)?).unwrap_or(Kind::Info),
                    text: r.get(4)?,
                    reported_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn insert_feedback(
        &self,
        run_id: i64,
        target_kind: FeedbackTarget,
        target_id: i64,
        text: &str,
    ) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO feedback(run_id, target_kind, target_id, text, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![run_id, target_kind.as_str(), target_id, text, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn feedback_for_run(&self, run_id: i64) -> Result<Vec<Feedback>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, target_kind, target_id, text, created_at, seen_at
             FROM feedback WHERE run_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![run_id], |r| {
            Ok(Feedback {
                id: r.get(0)?,
                run_id: r.get(1)?,
                target_kind: match r.get::<_, String>(2)?.as_str() {
                    "test" => FeedbackTarget::Test,
                    "run" => FeedbackTarget::Run,
                    _ => FeedbackTarget::Note,
                },
                target_id: r.get(3)?,
                text: r.get(4)?,
                created_at: r.get(5)?,
                seen_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Mark all currently-unseen feedback for a run as seen. Returns how many
    /// rows were updated. Used by `testito feedback --run X` so the agent only
    /// sees each piece of feedback once unless `--no-mark-seen` is passed.
    pub fn mark_run_feedback_seen(&self, run_id: i64) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        let n = self.conn.execute(
            "UPDATE feedback SET seen_at = ?1
             WHERE run_id = ?2 AND seen_at IS NULL",
            params![now, run_id],
        )?;
        Ok(n as i64)
    }

    pub fn unseen_feedback_count(&self, run_id: i64) -> Result<i64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM feedback WHERE run_id = ?1 AND seen_at IS NULL",
            params![run_id],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Link a step to a finding (run_note). Many-to-many — a step can cite
    /// several findings (e.g. "this scenario surfaced bugs #28, #29") and a
    /// finding can be cited by several steps. Idempotent: re-linking the same
    /// pair is a no-op so retries are safe.
    pub fn link_step_finding(&self, step_id: i64, note_id: i64) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR IGNORE INTO step_finding_refs(step_id, note_id, created_at)
             VALUES(?1, ?2, ?3)",
            params![step_id, note_id, now],
        )?;
        Ok(())
    }

    /// Returns `(step_id, note_id)` pairs for every step→finding ref under
    /// this run. Caller groups into the shape they need (step→[notes] for
    /// triage, note→[steps] for the reverse map).
    pub fn step_finding_refs_for_run(&self, run_id: i64) -> Result<Vec<(i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.step_id, r.note_id
             FROM step_finding_refs r
             JOIN run_steps s ON s.id = r.step_id
             JOIN run_tests t ON s.run_test_id = t.id
             WHERE t.run_id = ?1",
        )?;
        let rows = stmt.query_map(params![run_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Append a review (security/code/perf/other) to a run. Reviews are
    /// append-only just like steps and notes — re-running `/security-review`
    /// against a run files a new review row rather than mutating the prior
    /// one, so the dashboard shows the assessment history.
    pub fn insert_review(
        &self,
        run_id: i64,
        kind: ReviewKind,
        verdict: ReviewVerdict,
        text: &str,
    ) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO reviews(run_id, kind, verdict, text, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![run_id, kind.as_str(), verdict.as_str(), text, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn reviews_for_run(&self, run_id: i64) -> Result<Vec<Review>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, kind, verdict, text, created_at
             FROM reviews
             WHERE run_id = ?1
             ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt.query_map(params![run_id], |r| {
            Ok(Review {
                id: r.get(0)?,
                run_id: r.get(1)?,
                kind: match r.get::<_, String>(2)?.as_str() {
                    "security" => ReviewKind::Security,
                    "code" => ReviewKind::Code,
                    "perf" => ReviewKind::Perf,
                    _ => ReviewKind::Other,
                },
                verdict: match r.get::<_, String>(3)?.as_str() {
                    "clean" => ReviewVerdict::Clean,
                    "advisory" => ReviewVerdict::Advisory,
                    "blocking" => ReviewVerdict::Blocking,
                    _ => ReviewVerdict::Advisory,
                },
                text: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn insert_attachment(
        &self,
        target_kind: AttachmentTarget,
        target_id: i64,
        filename: &str,
        original_filename: &str,
        mime_type: &str,
        bytes: i64,
    ) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO attachments(target_kind, target_id, filename, original_filename, mime_type, bytes, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                target_kind.as_str(),
                target_id,
                filename,
                original_filename,
                mime_type,
                bytes,
                now,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Read all attachments for the targets in a single run, returning a map
    /// keyed by `(target_kind, target_id)`. A single batched fetch beats
    /// N+1 lookups when the page renders dozens of notes and steps.
    pub fn attachments_for_run(
        &self,
        run_id: i64,
    ) -> Result<std::collections::HashMap<(String, i64), Vec<Attachment>>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.target_kind, a.target_id, a.filename, a.original_filename,
                    a.mime_type, a.bytes, a.created_at
             FROM attachments a
             WHERE
                (a.target_kind = 'note' AND a.target_id IN (SELECT id FROM run_notes WHERE run_id = ?1))
                OR
                (a.target_kind = 'step' AND a.target_id IN (
                    SELECT s.id FROM run_steps s
                    JOIN run_tests t ON s.run_test_id = t.id
                    WHERE t.run_id = ?1
                ))
             ORDER BY a.id",
        )?;
        let rows = stmt.query_map(params![run_id], |r| {
            Ok(Attachment {
                id: r.get(0)?,
                target_kind: match r.get::<_, String>(1)?.as_str() {
                    "step" => AttachmentTarget::Step,
                    _ => AttachmentTarget::Note,
                },
                target_id: r.get(2)?,
                filename: r.get(3)?,
                original_filename: r.get(4)?,
                mime_type: r.get(5)?,
                bytes: r.get(6)?,
                created_at: r.get(7)?,
            })
        })?;
        let mut out: std::collections::HashMap<(String, i64), Vec<Attachment>> =
            std::collections::HashMap::new();
        for a in rows {
            let a = a?;
            out.entry((a.target_kind.as_str().to_string(), a.target_id))
                .or_default()
                .push(a);
        }
        Ok(out)
    }

    /// Stream read for `testito tail`: step rows whose id is greater than
    /// `after_id`, ordered by id ascending. Each tuple is `(test_name, step)`
    /// so the caller can render in one pass without a second lookup.
    /// Pass `after_id = 0` for the full history.
    pub fn tail_steps_after(&self, run_id: i64, after_id: i64) -> Result<Vec<(String, RunStep)>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.name, s.id, s.run_test_id, s.name, s.attempt, s.result, s.note, s.reported_at
             FROM run_steps s JOIN run_tests t ON t.id = s.run_test_id
             WHERE t.run_id = ?1 AND s.id > ?2
             ORDER BY s.id",
        )?;
        let rows = stmt
            .query_map(params![run_id, after_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    RunStep {
                        id: r.get(1)?,
                        run_test_id: r.get(2)?,
                        name: r.get(3)?,
                        attempt: r.get(4)?,
                        result: TestResult::parse(&r.get::<_, String>(5)?)
                            .unwrap_or(TestResult::Skipped),
                        note: r.get(6)?,
                        reported_at: r.get(7)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Stream read for `testito tail`: notes whose id is greater than `after_id`,
    /// ordered by id ascending.
    pub fn notes_after(&self, run_id: i64, after_id: i64) -> Result<Vec<RunNote>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, scope, kind, text, reported_at FROM run_notes
             WHERE run_id = ?1 AND id > ?2 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![run_id, after_id], |r| {
                Ok(RunNote {
                    id: r.get(0)?,
                    run_id: r.get(1)?,
                    scope: Scope::parse(&r.get::<_, String>(2)?).unwrap_or(Scope::In),
                    kind: Kind::parse(&r.get::<_, String>(3)?).unwrap_or(Kind::Info),
                    text: r.get(4)?,
                    reported_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

fn column_exists(conn: &Connection, table: &str, col: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.iter().any(|n| n == col))
}

fn has_legacy_table(conn: &Connection) -> rusqlite::Result<bool> {
    let exists: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name IN ('files','tests','steps','plans','plan_tests','settings') LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Result as TestResult, RunMeta, Scope};
    use tempfile::tempdir;

    fn open_temp() -> (tempfile::TempDir, Db) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t.db");
        let db = Db::open(&path).unwrap();
        (dir, db)
    }

    #[test]
    fn schema_creates_expected_tables() {
        let (_dir, db) = open_temp();
        let mut stmt = db
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|n| !n.starts_with("sqlite_"))
            .collect();
        for required in ["runs", "run_tests", "run_steps", "run_notes"] {
            assert!(
                names.iter().any(|n| n == required),
                "missing table {required} in {names:?}"
            );
        }
    }

    #[test]
    fn ensure_run_is_idempotent_and_upserts_metadata() {
        let (_dir, db) = open_temp();
        let id1 = db
            .ensure_run(
                "smoke",
                &RunMeta {
                    description: Some("first".to_string()),
                    branch: Some("main".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        let id2 = db
            .ensure_run(
                "smoke",
                &RunMeta {
                    commit_sha: Some("abc1234".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(id1, id2, "same name should return same run id");

        let run = db.get_run(id1).unwrap().unwrap();
        assert_eq!(
            run.description, "first",
            "description set on first call should stick"
        );
        assert_eq!(run.branch, "main", "branch from first call should stick");
        assert_eq!(run.commit_sha, "abc1234", "second call upserts commit");
    }

    #[test]
    fn empty_meta_does_not_overwrite_existing_fields() {
        let (_dir, db) = open_temp();
        let id = db
            .ensure_run(
                "r",
                &RunMeta {
                    branch: Some("main".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        // Calling again with all-empty meta should NOT clear the branch.
        db.ensure_run("r", &RunMeta::default()).unwrap();
        let run = db.get_run(id).unwrap().unwrap();
        assert_eq!(run.branch, "main");
    }

    #[test]
    fn ensure_test_dedupes_by_run_and_name() {
        let (_dir, db) = open_temp();
        let run_id = db.ensure_run("r", &RunMeta::default()).unwrap();
        let t1 = db.ensure_test(run_id, "login").unwrap();
        let t2 = db.ensure_test(run_id, "login").unwrap();
        let t3 = db.ensure_test(run_id, "logout").unwrap();
        assert_eq!(t1, t2);
        assert_ne!(t1, t3);
        assert_eq!(db.run_tests(run_id).unwrap().len(), 2);
    }

    #[test]
    fn append_step_is_append_only_with_attempts() {
        let (_dir, db) = open_temp();
        let r = db.ensure_run("r", &RunMeta::default()).unwrap();
        let t = db.ensure_test(r, "login").unwrap();
        db.append_step(t, "click", 1, TestResult::Fail, "first try")
            .unwrap();
        db.append_step(t, "click", 2, TestResult::Pass, "fixed")
            .unwrap();
        db.append_step(t, "click", 3, TestResult::Pass, "").unwrap();
        let steps = db.steps_for_test(t).unwrap();
        assert_eq!(steps.len(), 3, "all attempts persisted");
        let attempts: Vec<i64> = steps.iter().map(|s| s.attempt).collect();
        assert_eq!(attempts, vec![1, 2, 3]);
        assert_eq!(steps[0].result, TestResult::Fail);
        assert_eq!(steps[2].result, TestResult::Pass);
    }

    #[test]
    fn append_note_stores_scope_kind_and_text() {
        let (_dir, db) = open_temp();
        let r = db.ensure_run("r", &RunMeta::default()).unwrap();
        db.append_note(r, Scope::In, Kind::Info, "in scope finding")
            .unwrap();
        db.append_note(r, Scope::Out, Kind::Bug, "out of scope finding")
            .unwrap();
        let notes = db.notes_for_run(r).unwrap();
        assert_eq!(notes.len(), 2);
        let scopes: Vec<Scope> = notes.iter().map(|n| n.scope).collect();
        let kinds: Vec<Kind> = notes.iter().map(|n| n.kind).collect();
        assert!(scopes.contains(&Scope::In));
        assert!(scopes.contains(&Scope::Out));
        assert!(kinds.contains(&Kind::Info));
        assert!(kinds.contains(&Kind::Bug));
    }

    #[test]
    fn migration_adds_kind_to_legacy_run_notes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t.db");
        // Pre-create a run_notes table that lacks the kind column.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE runs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL UNIQUE,
                    description TEXT NOT NULL DEFAULT '',
                    started_at TEXT NOT NULL,
                    completed_at TEXT
                );
                 CREATE TABLE run_tests (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    run_id INTEGER NOT NULL,
                    name TEXT NOT NULL,
                    first_reported_at TEXT NOT NULL,
                    UNIQUE(run_id, name));
                 CREATE TABLE run_steps (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    run_test_id INTEGER NOT NULL,
                    name TEXT NOT NULL,
                    attempt INTEGER NOT NULL DEFAULT 1,
                    result TEXT NOT NULL,
                    note TEXT NOT NULL DEFAULT '',
                    reported_at TEXT NOT NULL);
                 CREATE TABLE run_notes (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    run_id INTEGER NOT NULL,
                    scope TEXT NOT NULL,
                    text TEXT NOT NULL,
                    reported_at TEXT NOT NULL);
                 INSERT INTO runs(name, started_at) VALUES('legacy', '2026-01-01T00:00:00Z');
                 INSERT INTO run_notes(run_id, scope, text, reported_at)
                    VALUES(1, 'in', 'pre-kind note', '2026-01-01T00:00:01Z');",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let notes = db.notes_for_run(1).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].kind, Kind::Info, "legacy notes default to info");
        // Now write a new note with an explicit kind and read it back.
        db.append_note(1, Scope::Out, Kind::Bug, "new bug").unwrap();
        let notes = db.notes_for_run(1).unwrap();
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().any(|n| n.kind == Kind::Bug));
    }

    #[test]
    fn complete_run_sets_completed_at() {
        let (_dir, db) = open_temp();
        let r = db.ensure_run("r", &RunMeta::default()).unwrap();
        assert!(db.get_run(r).unwrap().unwrap().completed_at.is_none());
        db.complete_run(r).unwrap();
        let run = db.get_run(r).unwrap().unwrap();
        assert!(run.completed_at.is_some());
    }

    #[test]
    fn list_runs_orders_newest_first_and_counts_aggregate() {
        let (_dir, db) = open_temp();
        let a = db.ensure_run("a", &RunMeta::default()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let b = db.ensure_run("b", &RunMeta::default()).unwrap();

        let t = db.ensure_test(a, "x").unwrap();
        db.append_step(t, "s1", 1, TestResult::Pass, "").unwrap();
        db.append_step(t, "s2", 1, TestResult::Fail, "").unwrap();
        db.append_note(a, Scope::In, Kind::Info, "n").unwrap();

        let runs = db.list_runs().unwrap();
        assert_eq!(runs.len(), 2);
        // Newest first
        assert_eq!(runs[0].id, b);
        assert_eq!(runs[1].id, a);
        // Aggregate counts on `a`
        assert_eq!(runs[1].test_count, 1);
        assert_eq!(runs[1].step_count, 2);
        assert_eq!(runs[1].note_count, 1);
        // Empty `b`
        assert_eq!(runs[0].test_count, 0);
        assert_eq!(runs[0].step_count, 0);
    }

    #[test]
    fn legacy_tables_are_dropped_on_open() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t.db");
        // Pre-create a database in the *old* schema (settings table is enough to trigger).
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT);
                 INSERT INTO settings(key, value) VALUES('root_path', '/old');",
            )
            .unwrap();
        }
        // Opening with the new code should drop the legacy tables and create the new schema.
        let db = Db::open(&path).unwrap();
        let mut stmt = db
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(std::result::Result::ok)
            .collect();
        assert!(
            !names.iter().any(|n| n == "settings"),
            "legacy settings table should be dropped"
        );
        assert!(
            !names.iter().any(|n| n == "files"),
            "legacy files table should be dropped"
        );
        assert!(names.iter().any(|n| n == "runs"));
    }

    #[test]
    fn migration_adds_metadata_columns_to_old_runs_table() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t.db");
        // Simulate an older runs table that lacks branch/commit_sha/env/url.
        // (No legacy-table sentinel is present, so the legacy-drop path won't fire.)
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE runs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL UNIQUE,
                    description TEXT NOT NULL DEFAULT '',
                    started_at TEXT NOT NULL,
                    completed_at TEXT
                );
                 CREATE TABLE run_tests (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    run_id INTEGER NOT NULL,
                    name TEXT NOT NULL,
                    first_reported_at TEXT NOT NULL,
                    UNIQUE(run_id, name));
                 CREATE TABLE run_steps (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    run_test_id INTEGER NOT NULL,
                    name TEXT NOT NULL,
                    attempt INTEGER NOT NULL DEFAULT 1,
                    result TEXT NOT NULL,
                    note TEXT NOT NULL DEFAULT '',
                    reported_at TEXT NOT NULL);
                 CREATE TABLE run_notes (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    run_id INTEGER NOT NULL,
                    scope TEXT NOT NULL,
                    text TEXT NOT NULL,
                    reported_at TEXT NOT NULL);
                 INSERT INTO runs(name, description, started_at)
                    VALUES('legacy', 'pre-metadata run', '2026-01-01T00:00:00Z');",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        // The pre-existing run is preserved, with empty metadata defaults.
        let run = db
            .get_run(1)
            .unwrap()
            .expect("legacy run should still exist");
        assert_eq!(run.name, "legacy");
        assert_eq!(run.branch, "");
        assert_eq!(run.commit_sha, "");
        // The new columns now exist and accept writes.
        db.ensure_run(
            "legacy",
            &RunMeta {
                branch: Some("main".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        let run = db.get_run(1).unwrap().unwrap();
        assert_eq!(run.branch, "main");
    }
}
