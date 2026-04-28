use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Result {
    Pass,
    Fail,
    Warning,
    Skipped,
}

impl Result {
    pub fn as_str(&self) -> &'static str {
        match self {
            Result::Pass => "pass",
            Result::Fail => "fail",
            Result::Warning => "warning",
            Result::Skipped => "skipped",
        }
    }

    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "pass" | "passed" | "ok" => Ok(Result::Pass),
            "fail" | "failed" => Ok(Result::Fail),
            "warning" | "warn" => Ok(Result::Warning),
            "skip" | "skipped" => Ok(Result::Skipped),
            other => Err(anyhow::anyhow!(
                "invalid result '{}' — use pass, fail, warning, or skipped",
                other
            )),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Result::Pass => "Pass",
            Result::Fail => "Fail",
            Result::Warning => "Warning",
            Result::Skipped => "Skipped",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    In,
    Out,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::In => "in",
            Scope::Out => "out",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Scope::In => "In scope",
            Scope::Out => "Out of scope",
        }
    }

    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "in" | "in-scope" | "inscope" => Ok(Scope::In),
            "out" | "out-of-scope" | "outofscope" => Ok(Scope::Out),
            other => Err(anyhow::anyhow!(
                "invalid scope '{}' — use 'in' or 'out'",
                other
            )),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RunMeta {
    pub description: Option<String>,
    pub branch: Option<String>,
    pub commit_sha: Option<String>,
    pub env: Option<String>,
    pub url: Option<String>,
}

impl RunMeta {
    pub fn is_empty(&self) -> bool {
        self.description.is_none()
            && self.branch.is_none()
            && self.commit_sha.is_none()
            && self.env.is_none()
            && self.url.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct Run {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub branch: String,
    pub commit_sha: String,
    pub env: String,
    pub url: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub test_count: i64,
    pub step_count: i64,
    pub note_count: i64,
}

impl Run {
    pub fn has_metadata(&self) -> bool {
        !self.branch.is_empty()
            || !self.commit_sha.is_empty()
            || !self.env.is_empty()
            || !self.url.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct RunTest {
    pub id: i64,
    pub run_id: i64,
    pub name: String,
    pub first_reported_at: String,
}

#[derive(Debug, Clone)]
pub struct RunStep {
    pub id: i64,
    pub run_test_id: i64,
    pub name: String,
    pub attempt: i64,
    pub result: Result,
    pub note: String,
    pub reported_at: String,
}

#[derive(Debug, Clone)]
pub struct RunNote {
    pub id: i64,
    pub run_id: i64,
    pub scope: Scope,
    pub text: String,
    pub reported_at: String,
}

/// Aggregate the latest attempt of each step into a single rollup verdict.
/// Returns the worst-case status: any fail → fail; else any warning → warning;
/// else if every recorded latest-attempt is skipped → skipped; else pass.
/// `None` means there are no steps.
pub fn rollup(steps: &[RunStep]) -> Option<Result> {
    use std::collections::BTreeMap;
    if steps.is_empty() {
        return None;
    }
    let mut latest: BTreeMap<&str, &RunStep> = BTreeMap::new();
    for s in steps {
        match latest.get(s.name.as_str()) {
            Some(existing) if existing.attempt >= s.attempt => {}
            _ => {
                latest.insert(&s.name, s);
            }
        }
    }
    let mut has_fail = false;
    let mut has_warn = false;
    let mut has_pass = false;
    for s in latest.values() {
        match s.result {
            Result::Fail => has_fail = true,
            Result::Warning => has_warn = true,
            Result::Pass => has_pass = true,
            Result::Skipped => {}
        }
    }
    if has_fail {
        return Some(Result::Fail);
    }
    if has_warn {
        return Some(Result::Warning);
    }
    if has_pass {
        return Some(Result::Pass);
    }
    Some(Result::Skipped)
}

/// Render an RFC3339 timestamp as a short relative string ("2m ago").
pub fn relative_time(rfc3339: &str) -> String {
    let Ok(t) = chrono::DateTime::parse_from_rfc3339(rfc3339) else {
        return rfc3339.to_string();
    };
    let now = chrono::Utc::now();
    let delta = now.signed_duration_since(t.with_timezone(&chrono::Utc));
    let secs = delta.num_seconds();
    if secs < 0 {
        return "just now".to_string();
    }
    if secs < 60 {
        return format!("{}s ago", secs);
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{}m ago", mins);
    }
    let hrs = mins / 60;
    if hrs < 24 {
        return format!("{}h ago", hrs);
    }
    let days = hrs / 24;
    if days < 30 {
        return format!("{}d ago", days);
    }
    // Beyond 30 days, show the date.
    t.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str, attempt: i64, result: Result) -> RunStep {
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
    fn result_parse_accepts_aliases() {
        assert_eq!(Result::parse("pass").unwrap(), Result::Pass);
        assert_eq!(Result::parse("PASS").unwrap(), Result::Pass);
        assert_eq!(Result::parse("passed").unwrap(), Result::Pass);
        assert_eq!(Result::parse("ok").unwrap(), Result::Pass);
        assert_eq!(Result::parse("fail").unwrap(), Result::Fail);
        assert_eq!(Result::parse("failed").unwrap(), Result::Fail);
        assert_eq!(Result::parse("warning").unwrap(), Result::Warning);
        assert_eq!(Result::parse("warn").unwrap(), Result::Warning);
        assert_eq!(Result::parse("skip").unwrap(), Result::Skipped);
        assert_eq!(Result::parse("skipped").unwrap(), Result::Skipped);
    }

    #[test]
    fn result_parse_rejects_garbage() {
        let err = Result::parse("whatever").unwrap_err().to_string();
        assert!(err.contains("invalid result"), "got: {err}");
    }

    #[test]
    fn scope_parse_accepts_aliases() {
        assert_eq!(Scope::parse("in").unwrap(), Scope::In);
        assert_eq!(Scope::parse("in-scope").unwrap(), Scope::In);
        assert_eq!(Scope::parse("InScope").unwrap(), Scope::In);
        assert_eq!(Scope::parse("out").unwrap(), Scope::Out);
        assert_eq!(Scope::parse("out-of-scope").unwrap(), Scope::Out);
        assert!(Scope::parse("sideways").is_err());
    }

    #[test]
    fn rollup_none_when_empty() {
        assert!(rollup(&[]).is_none());
    }

    #[test]
    fn rollup_pass_when_all_pass() {
        let s = vec![step("a", 1, Result::Pass), step("b", 1, Result::Pass)];
        assert_eq!(rollup(&s), Some(Result::Pass));
    }

    #[test]
    fn rollup_fail_dominates_pass() {
        let s = vec![
            step("a", 1, Result::Pass),
            step("b", 1, Result::Fail),
            step("c", 1, Result::Pass),
        ];
        assert_eq!(rollup(&s), Some(Result::Fail));
    }

    #[test]
    fn rollup_warning_when_no_fails() {
        let s = vec![step("a", 1, Result::Pass), step("b", 1, Result::Warning)];
        assert_eq!(rollup(&s), Some(Result::Warning));
    }

    #[test]
    fn rollup_uses_latest_attempt_per_step() {
        // Same step name, attempt 1 fail, attempt 2 pass — overall should be Pass.
        let s = vec![
            step("submit", 1, Result::Fail),
            step("submit", 2, Result::Pass),
            step("other", 1, Result::Pass),
        ];
        assert_eq!(rollup(&s), Some(Result::Pass));
    }

    #[test]
    fn rollup_skipped_only_when_nothing_else() {
        let s = vec![step("blocked", 1, Result::Skipped)];
        assert_eq!(rollup(&s), Some(Result::Skipped));
    }

    #[test]
    fn relative_time_returns_input_for_unparseable() {
        assert_eq!(relative_time("not-a-date"), "not-a-date");
    }

    #[test]
    fn relative_time_buckets() {
        let now = chrono::Utc::now();
        let cases = [
            (chrono::Duration::seconds(10), "s ago"),
            (chrono::Duration::minutes(3), "m ago"),
            (chrono::Duration::hours(2), "h ago"),
            (chrono::Duration::days(5), "d ago"),
        ];
        for (d, suffix) in cases {
            let t = now - d;
            let out = relative_time(&t.to_rfc3339());
            assert!(out.ends_with(suffix), "expected {suffix} suffix in {out}");
        }
    }

    #[test]
    fn relative_time_far_past_is_yyyy_mm_dd() {
        let t = chrono::Utc::now() - chrono::Duration::days(120);
        let out = relative_time(&t.to_rfc3339());
        // Format YYYY-MM-DD
        assert_eq!(out.len(), 10);
        assert!(out.chars().nth(4) == Some('-') && out.chars().nth(7) == Some('-'));
    }

    #[test]
    fn relative_time_future_is_just_now() {
        let t = chrono::Utc::now() + chrono::Duration::seconds(30);
        assert_eq!(relative_time(&t.to_rfc3339()), "just now");
    }
}
