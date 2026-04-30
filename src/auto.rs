use std::path::PathBuf;
use std::process::Command;

/// Best-effort metadata pulled from the current shell + git working tree.
/// Each field is `None` when detection fails or isn't applicable. Used to
/// fill in `--branch`, `--commit`, `--workdir`, and `--pr` when the agent
/// didn't pass them explicitly.
#[derive(Debug, Default)]
pub struct AutoMeta {
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub workdir: Option<String>,
    pub pr_number: Option<i64>,
    pub pr_url: Option<String>,
}

pub fn detect() -> AutoMeta {
    let (pr_number, pr_url) = gh_pr();
    AutoMeta {
        branch: git_branch(),
        commit: git_short_sha(),
        workdir: workdir_label(),
        pr_number,
        pr_url,
    }
}

fn git_branch() -> Option<String> {
    let b = run_git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    // Detached HEAD reports literally "HEAD" — not useful.
    if b == "HEAD" {
        None
    } else {
        Some(b)
    }
}

fn git_short_sha() -> Option<String> {
    run_git(&["rev-parse", "--short", "HEAD"])
}

/// Combines, when present:
/// - the linked-worktree dir name (only when we're in a linked worktree, not
///   the main one — because a linked worktree's branch alone doesn't tell
///   you which checkout you're in if multiple worktrees share it);
/// - the zellij session name from `ZELLIJ_SESSION_NAME`.
///
/// Returns `None` when neither signal is available.
fn workdir_label() -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(w) = linked_worktree_name() {
        parts.push(w);
    }
    if let Some(z) = zellij_session() {
        parts.push(format!("zellij:{z}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

fn linked_worktree_name() -> Option<String> {
    let common = run_git(&["rev-parse", "--git-common-dir"])?;
    let gitdir = run_git(&["rev-parse", "--git-dir"])?;
    // In the main worktree these resolve to the same path; in a linked one
    // git-dir lives under .git/worktrees/<name>. Compare canonical paths so
    // a relative ".git" vs absolute path doesn't trip the check.
    let common_abs = std::fs::canonicalize(&common).ok()?;
    let gitdir_abs = std::fs::canonicalize(&gitdir).ok()?;
    if common_abs == gitdir_abs {
        return None;
    }
    let toplevel = run_git(&["rev-parse", "--show-toplevel"])?;
    PathBuf::from(toplevel)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
}

fn zellij_session() -> Option<String> {
    std::env::var("ZELLIJ_SESSION_NAME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Returns `(pr_number, pr_html_url)` when `gh` is on PATH AND there's an
/// open PR for the current branch on the default remote. Both `None` if not.
/// `gh pr view` returns non-zero (and we silently no-op) when there's no PR,
/// no remote, no auth, or `gh` is not installed.
fn gh_pr() -> (Option<i64>, Option<String>) {
    let out = match Command::new("gh")
        .args(["pr", "view", "--json", "number,url"])
        .output()
    {
        Ok(out) if out.status.success() => out,
        _ => return (None, None),
    };
    let v: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => return (None, None),
    };
    let number = v.get("number").and_then(|n| n.as_i64());
    let url = v.get("url").and_then(|u| u.as_str()).map(|s| s.to_string());
    (number, url)
}

fn run_git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}
