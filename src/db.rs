use std::path::Path;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use crate::models::{Result as TestResult, Run, RunMeta, RunNote, RunStep, RunTest, Scope};

pub struct Db {
    pub conn: Connection,
}

const SELECT_RUN_BASE_NO_ORDER: &str = "SELECT r.id, r.name, r.description, r.branch, r.commit_sha, r.env, r.url, r.started_at, r.completed_at,
                    (SELECT COUNT(*) FROM run_tests WHERE run_id = r.id) as test_count,
                    (SELECT COUNT(*) FROM run_steps s JOIN run_tests t ON s.run_test_id = t.id WHERE t.run_id = r.id) as step_count,
                    (SELECT COUNT(*) FROM run_notes WHERE run_id = r.id) as note_count
             FROM runs r";

const SELECT_RUN_BASE: &str = "SELECT r.id, r.name, r.description, r.branch, r.commit_sha, r.env, r.url, r.started_at, r.completed_at,
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
        started_at: r.get(7)?,
        completed_at: r.get(8)?,
        test_count: r.get(9)?,
        step_count: r.get(10)?,
        note_count: r.get(11)?,
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
    text TEXT NOT NULL,
    reported_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_run_notes_run ON run_notes(run_id);
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
        for col in ["branch", "commit_sha", "env", "url"] {
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
        Ok(Self { conn })
    }

    pub fn ensure_run(&self, name: &str, meta: &RunMeta) -> Result<i64> {
        if let Some(id) = self.find_run_id(name)? {
            self.upsert_run_metadata(id, meta)?;
            return Ok(id);
        }
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO runs(name, description, branch, commit_sha, env, url, started_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                name,
                meta.description.as_deref().unwrap_or(""),
                meta.branch.as_deref().unwrap_or(""),
                meta.commit_sha.as_deref().unwrap_or(""),
                meta.env.as_deref().unwrap_or(""),
                meta.url.as_deref().unwrap_or(""),
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
        ];
        for (col, val) in pairs {
            if let Some(v) = val {
                if !v.is_empty() {
                    let sql = format!("UPDATE runs SET {} = ?1 WHERE id = ?2", col);
                    self.conn.execute(&sql, params![v, id])?;
                }
            }
        }
        Ok(())
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

    pub fn append_note(&self, run_id: i64, scope: Scope, text: &str) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO run_notes(run_id, scope, text, reported_at) VALUES(?1, ?2, ?3, ?4)",
            params![run_id, scope.as_str(), text, now],
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
            "SELECT id, run_id, scope, text, reported_at FROM run_notes
             WHERE run_id = ?1 ORDER BY reported_at, id",
        )?;
        let rows = stmt
            .query_map(params![run_id], |r| {
                Ok(RunNote {
                    id: r.get(0)?,
                    run_id: r.get(1)?,
                    scope: Scope::parse(&r.get::<_, String>(2)?).unwrap_or(Scope::In),
                    text: r.get(3)?,
                    reported_at: r.get(4)?,
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
    fn append_note_stores_scope_and_text() {
        let (_dir, db) = open_temp();
        let r = db.ensure_run("r", &RunMeta::default()).unwrap();
        db.append_note(r, Scope::In, "in scope finding").unwrap();
        db.append_note(r, Scope::Out, "out of scope finding")
            .unwrap();
        let notes = db.notes_for_run(r).unwrap();
        assert_eq!(notes.len(), 2);
        let scopes: Vec<Scope> = notes.iter().map(|n| n.scope).collect();
        assert!(scopes.contains(&Scope::In));
        assert!(scopes.contains(&Scope::Out));
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
        db.append_note(a, Scope::In, "n").unwrap();

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
