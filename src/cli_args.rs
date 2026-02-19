use clap::{Parser, ValueEnum};
use gix::Repository;
use serde::Serialize;
use std::fmt;

#[derive(Debug, Default, Clone, ValueEnum, Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum DiffStrategy {
    /// Explicit SHAs
    #[clap(skip)]
    Explicit { base: String, head: String },
    /// Compare worktree against a base branch
    #[clap(skip)]
    WorktreeVsBranch { branch: String },
    /// Compare local changes: HEAD~ vs HEAD
    /// Falls back to HEAD vs HEAD if no parent commit exists
    #[default]
    LocalChanges,
    /// No comparing, run all
    All,
}

impl DiffStrategy {
    pub fn git_commits(&self, repo: &Repository) -> anyhow::Result<(gix::ObjectId, gix::ObjectId)> {
        match self {
            DiffStrategy::Explicit { base, head } => {
                let head_id = repo.rev_parse_single(head.as_str())?.detach();
                let base_id = repo.rev_parse_single(base.as_str())?.detach();
                Ok((base_id, head_id))
            }
            DiffStrategy::LocalChanges | DiffStrategy::All => {
                let head_id = repo.rev_parse_single("HEAD")?.detach();
                let base_id = repo
                    .rev_parse_single("HEAD~")
                    .map(|id| id.detach())
                    .unwrap_or(head_id);
                Ok((base_id, head_id))
            }
            DiffStrategy::WorktreeVsBranch { branch } => {
                let head_id = repo.rev_parse_single("HEAD")?.detach();
                let base_id = repo.rev_parse_single(branch.as_str())?.detach();
                Ok((base_id, head_id))
            }
        }
    }
}

impl fmt::Display for DiffStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiffStrategy::All => write!(f, "all"),
            DiffStrategy::LocalChanges => write!(f, "local-changes"),
            DiffStrategy::WorktreeVsBranch { branch } => write!(f, "branch:{}", branch),
            DiffStrategy::Explicit { base, head } => write!(f, "{}..{}", base, head),
        }
    }
}

/// Relevant env vars are specified by Prow here:
/// <https://docs.prow.k8s.io/docs/jobs/#job-environment-variables>
#[derive(Debug, Parser, Default, Clone)]
pub struct DiffOptions {
    #[clap(long, env = "PULL_PULL_SHA")]
    pub head_sha: Option<String>,
    #[clap(long, env = "PULL_BASE_SHA")]
    pub base_sha: Option<String>,
    #[clap(long, env)]
    pub compare_branch: Option<String>,
    #[clap(long, env)]
    pub strategy: Option<DiffStrategy>,
}

impl DiffOptions {
    pub fn strategy(&self) -> DiffStrategy {
        if let Some(strategy) = self.strategy.clone() {
            return strategy;
        }
        match (&self.base_sha, &self.head_sha, &self.compare_branch) {
            (Some(base), Some(head), _) => DiffStrategy::Explicit {
                base: base.clone(),
                head: head.clone(),
            },
            (None, None, Some(branch)) => DiffStrategy::WorktreeVsBranch {
                branch: branch.clone(),
            },
            _ => DiffStrategy::LocalChanges,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn git(repo_path: &std::path::Path) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(repo_path);
        cmd.env("GIT_AUTHOR_NAME", "Test User");
        cmd.env("GIT_AUTHOR_EMAIL", "test@example.com");
        cmd.env("GIT_COMMITTER_NAME", "Test User");
        cmd.env("GIT_COMMITTER_EMAIL", "test@example.com");
        cmd
    }

    fn get_commit_sha(repo_path: &std::path::Path, rev: &str) -> String {
        let output = git(repo_path)
            .args(["rev-parse", rev])
            .output()
            .expect("Failed to get commit SHA");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    // Test helper to create a repo with commits
    fn setup_test_repo() -> (TempDir, String, String) {
        let temp_dir = TempDir::new().unwrap();

        // Initialize repo
        let output = git(temp_dir.path())
            .arg("init")
            .output()
            .expect("Failed to init repo");
        assert!(output.status.success());

        // Configure repo
        let output = git(temp_dir.path())
            .args(["config", "commit.gpgsign", "false"])
            .output()
            .expect("Failed to configure git");
        assert!(output.status.success());

        // Create first commit
        fs::write(temp_dir.path().join("file1.txt"), "content1").unwrap();
        let output = git(temp_dir.path())
            .args(["add", "file1.txt"])
            .output()
            .expect("Failed to add file");
        assert!(output.status.success());

        let output = git(temp_dir.path())
            .args(["commit", "-m", "First commit"])
            .output()
            .expect("Failed to commit");
        assert!(output.status.success());

        let first_commit = get_commit_sha(temp_dir.path(), "HEAD");

        // Create second commit
        fs::write(temp_dir.path().join("file2.txt"), "content2").unwrap();
        let output = git(temp_dir.path())
            .args(["add", "file2.txt"])
            .output()
            .expect("Failed to add file");
        assert!(output.status.success());

        let output = git(temp_dir.path())
            .args(["commit", "-m", "Second commit"])
            .output()
            .expect("Failed to commit");
        assert!(output.status.success());

        let second_commit = get_commit_sha(temp_dir.path(), "HEAD");

        (temp_dir, first_commit, second_commit)
    }

    #[test]
    fn test_diff_strategy_local_changes() {
        let (temp_dir, first_sha, second_sha) = setup_test_repo();
        let repo = gix::open(temp_dir.path()).unwrap();
        let strategy = DiffStrategy::LocalChanges;

        let (base, head) = strategy.git_commits(&repo).unwrap();

        // LocalChanges compares HEAD~ vs HEAD
        assert_eq!(base.to_string(), first_sha);
        assert_eq!(head.to_string(), second_sha);
    }

    #[test]
    fn test_diff_strategy_local_changes_single_commit() {
        // Test that LocalChanges works even with just one commit
        let temp_dir = TempDir::new().unwrap();

        // Initialize repo
        let output = git(temp_dir.path())
            .arg("init")
            .output()
            .expect("Failed to init repo");
        assert!(output.status.success());

        // Configure repo
        let output = git(temp_dir.path())
            .args(["config", "commit.gpgsign", "false"])
            .output()
            .expect("Failed to configure git");
        assert!(output.status.success());

        // Create single commit
        fs::write(temp_dir.path().join("file1.txt"), "content1").unwrap();
        let output = git(temp_dir.path())
            .args(["add", "file1.txt"])
            .output()
            .expect("Failed to add file");
        assert!(output.status.success());

        let output = git(temp_dir.path())
            .args(["commit", "-m", "Initial commit"])
            .output()
            .expect("Failed to commit");
        assert!(output.status.success());

        let commit_sha = get_commit_sha(temp_dir.path(), "HEAD");

        let repo = gix::open(temp_dir.path()).unwrap();
        let strategy = DiffStrategy::LocalChanges;
        let (base, head) = strategy.git_commits(&repo).unwrap();

        // Should successfully return HEAD vs HEAD even with single commit
        assert_eq!(base.to_string(), commit_sha);
        assert_eq!(head.to_string(), commit_sha);
    }

    #[test]
    fn test_diff_strategy_explicit() {
        let (temp_dir, first_sha, second_sha) = setup_test_repo();
        let repo = gix::open(temp_dir.path()).unwrap();
        let strategy = DiffStrategy::Explicit {
            base: first_sha.clone(),
            head: second_sha.clone(),
        };

        let (base, head) = strategy.git_commits(&repo).unwrap();

        assert_eq!(base.to_string(), first_sha);
        assert_eq!(head.to_string(), second_sha);
    }

    #[test]
    fn test_diff_strategy_explicit_short_sha() {
        let (temp_dir, first_sha, second_sha) = setup_test_repo();
        let repo = gix::open(temp_dir.path()).unwrap();
        let strategy = DiffStrategy::Explicit {
            base: first_sha[..7].to_string(),
            head: second_sha[..7].to_string(),
        };

        let (base, head) = strategy.git_commits(&repo).unwrap();

        assert_eq!(base.to_string(), first_sha);
        assert_eq!(head.to_string(), second_sha);
    }

    #[test]
    fn test_diff_strategy_worktree_vs_branch() {
        let (temp_dir, first_sha, second_sha) = setup_test_repo();

        // Create a branch pointing to first commit
        let output = git(temp_dir.path())
            .args(["branch", "test-branch", &first_sha])
            .output()
            .expect("Failed to create branch");
        assert!(output.status.success());

        let repo = gix::open(temp_dir.path()).unwrap();
        let strategy = DiffStrategy::WorktreeVsBranch {
            branch: "test-branch".to_string(),
        };

        let (base, head) = strategy.git_commits(&repo).unwrap();

        assert_eq!(base.to_string(), first_sha);
        assert_eq!(head.to_string(), second_sha); // HEAD is still at second commit
    }

    #[test]
    fn test_diff_strategy_explicit_invalid_sha() {
        let (temp_dir, _first_sha, _second_sha) = setup_test_repo();
        let repo = gix::open(temp_dir.path()).unwrap();
        let strategy = DiffStrategy::Explicit {
            base: "invalid".to_string(),
            head: "also-invalid".to_string(),
        };

        let result = strategy.git_commits(&repo);
        assert!(result.is_err());
    }

    #[test]
    fn test_diff_strategy_worktree_vs_branch_invalid() {
        let (temp_dir, _first_sha, _second_sha) = setup_test_repo();
        let repo = gix::open(temp_dir.path()).unwrap();
        let strategy = DiffStrategy::WorktreeVsBranch {
            branch: "nonexistent-branch".to_string(),
        };

        let result = strategy.git_commits(&repo);
        assert!(result.is_err());
    }

    #[test]
    fn test_display_local_changes() {
        let strategy = DiffStrategy::LocalChanges;
        assert_eq!(strategy.to_string(), "local-changes");
    }

    #[test]
    fn test_display_worktree_vs_branch() {
        let strategy = DiffStrategy::WorktreeVsBranch {
            branch: "main".to_string(),
        };
        assert_eq!(strategy.to_string(), "branch:main");
    }

    #[test]
    fn test_display_explicit() {
        let strategy = DiffStrategy::Explicit {
            base: "abc123".to_string(),
            head: "def456".to_string(),
        };
        assert_eq!(strategy.to_string(), "abc123..def456");
    }

    #[test]
    fn test_diff_options_strategy_explicit() {
        let options = DiffOptions {
            base_sha: Some("base123".to_string()),
            head_sha: Some("head456".to_string()),
            compare_branch: None,
            strategy: None,
        };

        let strategy = options.strategy();
        match strategy {
            DiffStrategy::Explicit { base, head } => {
                assert_eq!(base, "base123");
                assert_eq!(head, "head456");
            }
            _ => panic!("Expected Explicit strategy"),
        }
    }

    #[test]
    fn test_diff_options_strategy_worktree_vs_branch() {
        let options = DiffOptions {
            base_sha: None,
            head_sha: None,
            compare_branch: Some("develop".to_string()),
            strategy: None,
        };

        let strategy = options.strategy();
        match strategy {
            DiffStrategy::WorktreeVsBranch { branch } => {
                assert_eq!(branch, "develop");
            }
            _ => panic!("Expected WorktreeVsBranch strategy"),
        }
    }

    #[test]
    fn test_diff_options_strategy_local_changes_default() {
        let options = DiffOptions {
            base_sha: None,
            head_sha: None,
            compare_branch: None,
            strategy: None,
        };

        let strategy = options.strategy();
        assert!(matches!(strategy, DiffStrategy::LocalChanges));
    }

    #[test]
    fn test_diff_options_explicit_strategy_override() {
        let options = DiffOptions {
            base_sha: Some("base123".to_string()),
            head_sha: Some("head456".to_string()),
            compare_branch: Some("develop".to_string()),
            strategy: Some(DiffStrategy::LocalChanges),
        };

        let strategy = options.strategy();
        assert!(matches!(strategy, DiffStrategy::LocalChanges));
    }

    #[test]
    fn test_diff_options_explicit_sha_priority_over_branch() {
        let options = DiffOptions {
            base_sha: Some("base123".to_string()),
            head_sha: Some("head456".to_string()),
            compare_branch: Some("develop".to_string()),
            strategy: None,
        };

        let strategy = options.strategy();
        assert!(matches!(strategy, DiffStrategy::Explicit { .. }));
    }

    #[test]
    fn test_diff_options_partial_sha_ignored() {
        // Only base_sha without head_sha should fall back to LocalChanges
        let options = DiffOptions {
            base_sha: Some("base123".to_string()),
            head_sha: None,
            compare_branch: None,
            strategy: None,
        };

        let strategy = options.strategy();
        assert!(matches!(strategy, DiffStrategy::LocalChanges));
    }
}
