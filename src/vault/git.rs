//! Git operations on the vault clone: the transport that carries it to a laptop and a phone.
//!
//! The vault is the working tree of a private repository. Mervyn pulls before it reconciles, so
//! it sees edits made elsewhere, and commits and pushes afterwards, so its own writes travel.
//! Obsidian Git does the same on the other devices.
//!
//! Blocking on purpose. The whole cycle — pull, merge, write, commit, push — runs under the vault
//! lock, and a lock held across an await is a deadlock waiting to happen, so the scheduler calls
//! this from `spawn_blocking` instead.

use std::path::Path;
use std::process::{Command, Output};

use anyhow::Context;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

use crate::config::VaultGitSection;

/// Token for HTTPS pushes to a private repo. GitHub expects `x-access-token` plus the token as
/// **Basic** auth, not Bearer. A classic PAT, a fine-grained PAT, or a `gh auth token` value all
/// work — but pushing needs write access, where the old worklog pull only needed read.
const PAT_ENV: &str = "MERVYN_VAULT_GITHUB_PAT";

/// Whether `path` is the working tree of a git repository.
pub fn is_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

fn git(path: &Path, cfg: &VaultGitSection, args: &[&str]) -> anyhow::Result<Output> {
    let mut cmd = Command::new("git");
    cmd.current_dir(path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("-c")
        .arg("credential.helper=")
        .arg("-c")
        .arg(format!("user.name={}", cfg.author_name))
        .arg("-c")
        .arg(format!("user.email={}", cfg.author_email));

    if let Ok(pat) = std::env::var(PAT_ENV) {
        let pat = pat.trim();
        if !pat.is_empty() {
            let basic = B64.encode(format!("x-access-token:{pat}"));
            cmd.arg("-c")
                .arg(format!("http.https://github.com/.extraheader=AUTHORIZATION: basic {basic}"));
        }
    }

    cmd.args(args);
    cmd.output()
        .with_context(|| format!("spawn git {:?} in {}", args, path.display()))
}

/// Run a git command, failing with its stderr when it does.
///
/// The error deliberately carries no command line: the PAT travels in a `-c` argument, and an
/// error string that quoted the invocation would put it in the logs.
fn run(path: &Path, cfg: &VaultGitSection, args: &[&str]) -> anyhow::Result<String> {
    let out = git(path, cfg, args)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        anyhow::bail!("git {} failed: {stderr}{stdout}", args.first().unwrap_or(&"?"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether the working tree has anything to commit.
pub fn is_dirty(path: &Path, cfg: &VaultGitSection) -> anyhow::Result<bool> {
    Ok(!run(path, cfg, &["status", "--porcelain"])?.trim().is_empty())
}

/// Commit everything in the working tree. Returns false when there was nothing to commit.
pub fn commit_all(path: &Path, cfg: &VaultGitSection, message: &str) -> anyhow::Result<bool> {
    if !is_dirty(path, cfg)? {
        return Ok(false);
    }
    run(path, cfg, &["add", "-A"])?;
    run(path, cfg, &["commit", "-m", message])?;
    Ok(true)
}

/// Outcome of trying to bring remote work in.
#[derive(Debug, PartialEq, Eq)]
pub enum PullOutcome {
    /// Up to date, or fast-forwarded / rebased cleanly.
    Ok,
    /// A rebase conflicted and was aborted. Local commits are intact and unpushed; this cycle
    /// must not write, or it would pile more work on top of a divergence a human has to settle.
    Conflicted,
}

/// `git pull --rebase`, aborting cleanly on conflict.
///
/// Conflicts are left for a person rather than resolved automatically. Picking a side here would
/// mean silently discarding either an edit made on the phone or a change the scheduler made, and
/// both are the kind of loss that is only noticed much later.
pub fn pull_rebase(path: &Path, cfg: &VaultGitSection) -> anyhow::Result<PullOutcome> {
    let out = git(
        path,
        cfg,
        &["pull", "--rebase", cfg.remote.trim(), cfg.branch.trim()],
    )?;
    if out.status.success() {
        return Ok(PullOutcome::Ok);
    }

    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    // `rebase --abort` fails harmlessly when no rebase was in progress, so its status is ignored:
    // the pull may have failed for an unrelated reason, which the caller learns from the log.
    let _ = git(path, cfg, &["rebase", "--abort"]);
    tracing::error!(
        path = %path.display(),
        detail = %stderr.trim(),
        "vault git pull could not rebase; the vault has diverged and needs resolving by hand \
         (its writes are paused until it does)"
    );
    Ok(PullOutcome::Conflicted)
}

/// Whether the local branch holds commits the remote does not.
///
/// Errs toward `true`: if the count cannot be read, pushing anyway costs a round trip, whereas
/// not pushing leaves work stranded on the server with nothing to say so.
pub fn has_unpushed(path: &Path, cfg: &VaultGitSection) -> bool {
    let range = format!("{}/{}..HEAD", cfg.remote.trim(), cfg.branch.trim());
    match run(path, cfg, &["rev-list", "--count", &range]) {
        Ok(out) => out.trim().parse::<usize>().map_or(true, |n| n > 0),
        Err(e) => {
            tracing::debug!(error = %e, "could not count unpushed commits; assuming there are some");
            true
        }
    }
}

/// `git push`, retrying once behind a rebase if the remote moved in between.
pub fn push(path: &Path, cfg: &VaultGitSection) -> anyhow::Result<()> {
    let remote = cfg.remote.trim();
    let branch = cfg.branch.trim();
    let first = git(path, cfg, &["push", remote, branch])?;
    if first.status.success() {
        return Ok(());
    }

    // Someone pushed between our pull and our push. One retry; a second failure is a real problem.
    tracing::debug!("vault git push rejected; rebasing and retrying once");
    if pull_rebase(path, cfg)? == PullOutcome::Conflicted {
        anyhow::bail!("push rejected and the rebase behind it conflicted");
    }
    run(path, cfg, &["push", remote, branch])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    fn cfg() -> VaultGitSection {
        VaultGitSection {
            enabled: true,
            remote: "origin".into(),
            branch: "main".into(),
            sync_cron: "0 */5 * * * *".into(),
            author_name: "Mervyn".into(),
            author_email: "mervyn@localhost".into(),
        }
    }

    /// A repo with one commit, plus a bare "remote" it can push to.
    fn repo_with_remote() -> (TempDir, TempDir) {
        let remote = tempdir().unwrap();
        Command::new("git")
            .args(["init", "--bare", "--initial-branch=main"])
            .current_dir(remote.path())
            .output()
            .unwrap();

        let work = tempdir().unwrap();
        let c = cfg();
        Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(work.path())
            .output()
            .unwrap();
        run(work.path(), &c, &["remote", "add", "origin", remote.path().to_str().unwrap()]).unwrap();
        std::fs::write(work.path().join("events.md"), "# Events\n").unwrap();
        commit_all(work.path(), &c, "initial").unwrap();
        run(work.path(), &c, &["push", "origin", "main"]).unwrap();
        (work, remote)
    }

    #[test]
    fn is_repo_distinguishes_a_clone_from_a_plain_directory() {
        let plain = tempdir().unwrap();
        assert!(!is_repo(plain.path()));
        let (work, _remote) = repo_with_remote();
        assert!(is_repo(work.path()));
    }

    #[test]
    fn commit_all_reports_whether_there_was_anything_to_do() {
        let (work, _remote) = repo_with_remote();
        let c = cfg();
        assert!(!commit_all(work.path(), &c, "nothing").unwrap());

        std::fs::write(work.path().join("todos.md"), "- [ ] one\n").unwrap();
        assert!(is_dirty(work.path(), &c).unwrap());
        assert!(commit_all(work.path(), &c, "mervyn: vault write-back").unwrap());
        assert!(!is_dirty(work.path(), &c).unwrap());
    }

    #[test]
    fn a_push_carries_work_to_the_remote_and_a_pull_brings_it_back() {
        let (work, remote) = repo_with_remote();
        let c = cfg();
        std::fs::write(work.path().join("todos.md"), "- [ ] one\n").unwrap();
        commit_all(work.path(), &c, "add a todo").unwrap();
        push(work.path(), &c).unwrap();

        // A second clone stands in for the phone.
        let phone = tempdir().unwrap();
        Command::new("git")
            .args(["clone", remote.path().to_str().unwrap(), "."])
            .current_dir(phone.path())
            .output()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(phone.path().join("todos.md")).unwrap(),
            "- [ ] one\n"
        );

        std::fs::write(phone.path().join("todos.md"), "- [x] one\n").unwrap();
        commit_all(phone.path(), &c, "tick it on the phone").unwrap();
        push(phone.path(), &c).unwrap();

        assert_eq!(pull_rebase(work.path(), &c).unwrap(), PullOutcome::Ok);
        assert_eq!(
            std::fs::read_to_string(work.path().join("todos.md")).unwrap(),
            "- [x] one\n",
            "the phone's edit reached the vault"
        );
    }

    #[test]
    fn edits_far_apart_in_a_file_rebase_without_conflicting() {
        let (work, remote) = repo_with_remote();
        let c = cfg();
        // Far apart: git needs a few lines between two edits before it can merge them. Two
        // adjacent lines in a short file conflict, which is the common case in a small vault —
        // see `adjacent_edits_conflict_which_is_the_realistic_case`.
        let base = "- [ ] one\n\nfiller\nfiller\nfiller\nfiller\nfiller\n\n- [ ] two\n";
        std::fs::write(work.path().join("todos.md"), base).unwrap();
        commit_all(work.path(), &c, "two todos").unwrap();
        push(work.path(), &c).unwrap();

        let phone = tempdir().unwrap();
        Command::new("git")
            .args(["clone", remote.path().to_str().unwrap(), "."])
            .current_dir(phone.path())
            .output()
            .unwrap();
        std::fs::write(phone.path().join("todos.md"), base.replace("- [ ] one", "- [x] one")).unwrap();
        commit_all(phone.path(), &c, "tick the first").unwrap();
        push(phone.path(), &c).unwrap();

        // Meanwhile Mervyn ticks the other one.
        std::fs::write(work.path().join("todos.md"), base.replace("- [ ] two", "- [x] two")).unwrap();
        commit_all(work.path(), &c, "tick the second").unwrap();

        assert_eq!(pull_rebase(work.path(), &c).unwrap(), PullOutcome::Ok);
        let merged = std::fs::read_to_string(work.path().join("todos.md")).unwrap();
        assert!(merged.contains("- [x] one") && merged.contains("- [x] two"), "both ticks survive");
    }

    #[test]
    fn adjacent_edits_conflict_which_is_the_realistic_case() {
        // Worth pinning down rather than hoping: in a short file, an edit on the phone and an
        // edit by Mervyn a line apart do not auto-merge. The vault pauses and says so.
        let (work, remote) = repo_with_remote();
        let c = cfg();
        std::fs::write(work.path().join("todos.md"), "- [ ] one\n- [ ] two\n").unwrap();
        commit_all(work.path(), &c, "two todos").unwrap();
        push(work.path(), &c).unwrap();

        let phone = tempdir().unwrap();
        Command::new("git")
            .args(["clone", remote.path().to_str().unwrap(), "."])
            .current_dir(phone.path())
            .output()
            .unwrap();
        std::fs::write(phone.path().join("todos.md"), "- [x] one\n- [ ] two\n").unwrap();
        commit_all(phone.path(), &c, "tick the first").unwrap();
        push(phone.path(), &c).unwrap();

        std::fs::write(work.path().join("todos.md"), "- [ ] one\n- [x] two\n").unwrap();
        commit_all(work.path(), &c, "tick the second").unwrap();

        assert_eq!(pull_rebase(work.path(), &c).unwrap(), PullOutcome::Conflicted);
        assert!(!work.path().join(".git/rebase-merge").exists(), "aborted cleanly");
    }

    #[test]
    fn the_same_line_edited_twice_conflicts_and_is_left_for_a_person() {
        let (work, remote) = repo_with_remote();
        let c = cfg();
        std::fs::write(work.path().join("todos.md"), "- [ ] one\n").unwrap();
        commit_all(work.path(), &c, "a todo").unwrap();
        push(work.path(), &c).unwrap();

        let phone = tempdir().unwrap();
        Command::new("git")
            .args(["clone", remote.path().to_str().unwrap(), "."])
            .current_dir(phone.path())
            .output()
            .unwrap();
        std::fs::write(phone.path().join("todos.md"), "- [ ] one, reworded on the phone\n").unwrap();
        commit_all(phone.path(), &c, "reword").unwrap();
        push(phone.path(), &c).unwrap();

        std::fs::write(work.path().join("todos.md"), "- [x] one\n").unwrap();
        commit_all(work.path(), &c, "tick").unwrap();

        assert_eq!(pull_rebase(work.path(), &c).unwrap(), PullOutcome::Conflicted);
        // Aborted cleanly: no rebase in progress, local commit intact, nothing half-applied.
        assert!(!work.path().join(".git/rebase-merge").exists());
        assert_eq!(
            std::fs::read_to_string(work.path().join("todos.md")).unwrap(),
            "- [x] one\n"
        );
    }

    #[test]
    fn push_retries_once_when_the_remote_moved_underneath_it() {
        let (work, remote) = repo_with_remote();
        let c = cfg();

        let phone = tempdir().unwrap();
        Command::new("git")
            .args(["clone", remote.path().to_str().unwrap(), "."])
            .current_dir(phone.path())
            .output()
            .unwrap();
        std::fs::write(phone.path().join("notes.md"), "from the phone\n").unwrap();
        commit_all(phone.path(), &c, "phone note").unwrap();
        push(phone.path(), &c).unwrap();

        // Mervyn commits without having seen that, so its first push is rejected.
        std::fs::write(work.path().join("todos.md"), "- [ ] one\n").unwrap();
        commit_all(work.path(), &c, "mervyn write-back").unwrap();
        push(work.path(), &c).unwrap();

        assert!(work.path().join("notes.md").exists(), "picked up the phone's commit");
        assert!(work.path().join("todos.md").exists(), "and kept its own");
    }
}
