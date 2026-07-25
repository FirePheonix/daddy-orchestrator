use anyhow::{Context, Result, anyhow};
use daddy_orchestrator::{Job, MergeEngine, Task, WorkspaceManager};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedWorktree {
    pub task_id: String,
    pub branch: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedWorkspaceSet {
    pub repo_root: PathBuf,
    pub worktree_root: PathBuf,
    pub worktrees: Vec<PreparedWorktree>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MergeOutcome {
    pub integration_branch: String,
    pub integration_path: PathBuf,
    pub merged_branches: Vec<String>,
    pub conflict_files: Vec<String>,
    pub review_required: bool,
}

pub struct GitWorktreeManager {
    base_dir_name: String,
}

pub struct GitMergeEngine {
    base_dir_name: String,
}

impl GitWorktreeManager {
    // Create a Git worktree manager that writes under the given per-repo base directory name.
    pub fn new(base_dir_name: impl Into<String>) -> Self {
        Self {
            base_dir_name: base_dir_name.into(),
        }
    }

    // Prepare one isolated Git worktree per task and return the created workspace metadata.
    pub fn prepare_set(&self, job: &Job, tasks: &[Task]) -> Result<PreparedWorkspaceSet> {
        let repo_root = discover_repo_root(&job.cwd)?;
        let worktree_root = repo_root
            .join(&self.base_dir_name)
            .join("worktrees")
            .join(sanitize_segment(&job.id));
        std::fs::create_dir_all(&worktree_root)?;
        let mut worktrees = Vec::new();
        for task in tasks {
            let branch = format!(
                "daddy/{}/{}",
                sanitize_segment(&job.id),
                sanitize_segment(&task.id)
            );
            let path = worktree_root.join(sanitize_segment(&task.id));
            add_worktree(&repo_root, &branch, &path)?;
            worktrees.push(PreparedWorktree {
                task_id: task.id.clone(),
                branch,
                path,
            });
        }
        Ok(PreparedWorkspaceSet {
            repo_root,
            worktree_root,
            worktrees,
        })
    }
}

impl Default for GitWorktreeManager {
    // Create a worktree manager that uses the default hidden repository directory.
    fn default() -> Self {
        Self::new(".daddy")
    }
}

impl GitMergeEngine {
    // Create a Git merge engine that writes its integration worktree under the given base directory.
    pub fn new(base_dir_name: impl Into<String>) -> Self {
        Self {
            base_dir_name: base_dir_name.into(),
        }
    }

    // Merge the prepared worker branches into one integration branch and report conflicts if they occur.
    pub fn merge_prepared(
        &self,
        job: &Job,
        prepared: &PreparedWorkspaceSet,
    ) -> Result<MergeOutcome> {
        let integration_branch = format!("daddy/integration/{}", sanitize_segment(&job.id));
        let integration_path = prepared
            .repo_root
            .join(&self.base_dir_name)
            .join("integration")
            .join(sanitize_segment(&job.id));
        add_worktree(&prepared.repo_root, &integration_branch, &integration_path)?;
        let mut merged_branches = Vec::new();
        let mut conflict_files = Vec::new();
        for worktree in &prepared.worktrees {
            let output = Command::new("git")
                .current_dir(&integration_path)
                .args(["merge", "--no-edit", &worktree.branch])
                .output()
                .with_context(|| format!("failed to merge branch {}", worktree.branch))?;
            if output.status.success() {
                merged_branches.push(worktree.branch.clone());
                continue;
            }
            conflict_files = unmerged_files(&integration_path)?;
            return Ok(MergeOutcome {
                integration_branch,
                integration_path,
                merged_branches,
                conflict_files,
                review_required: true,
            });
        }
        Ok(MergeOutcome {
            integration_branch,
            integration_path,
            merged_branches,
            conflict_files,
            review_required: false,
        })
    }
}

impl Default for GitMergeEngine {
    // Create a merge engine that uses the default hidden repository directory.
    fn default() -> Self {
        Self::new(".daddy")
    }
}

impl WorkspaceManager for GitWorktreeManager {
    // Return the workspace manager name used in CLI output and telemetry.
    fn name(&self) -> &'static str {
        "git-worktree"
    }

    // Prepare isolated Git worktrees for the given job and tasks.
    fn prepare(&self, job: &Job, tasks: &[Task]) -> Result<()> {
        self.prepare_set(job, tasks).map(|_| ())
    }
}

impl MergeEngine for GitMergeEngine {
    // Return the merge engine name used in CLI output and telemetry.
    fn name(&self) -> &'static str {
        "git-merge"
    }

    // Merge currently prepared worker branches for the given job when the caller has created worktrees already.
    fn merge(&self, job: &Job) -> Result<()> {
        let repo_root = discover_repo_root(&job.cwd)?;
        let worktree_root = repo_root
            .join(&self.base_dir_name)
            .join("worktrees")
            .join(sanitize_segment(&job.id));
        let worktrees = std::fs::read_dir(&worktree_root)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .map(|entry| PreparedWorktree {
                task_id: entry.file_name().to_string_lossy().to_string(),
                branch: format!(
                    "daddy/{}/{}",
                    sanitize_segment(&job.id),
                    entry.file_name().to_string_lossy()
                ),
                path: entry.path(),
            })
            .collect();
        self.merge_prepared(
            job,
            &PreparedWorkspaceSet {
                repo_root,
                worktree_root,
                worktrees,
            },
        )
        .map(|_| ())
    }
}

// Discover the Git repository root that contains the given working directory.
fn discover_repo_root(cwd: &Path) -> Result<PathBuf> {
    let output = run_git(cwd, ["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(output.trim()))
}

// Create one worktree and branch for the given task under the repository root.
fn add_worktree(repo_root: &Path, branch: &str, path: &Path) -> Result<()> {
    if path.exists() {
        return Err(anyhow!("worktree path already exists: {}", path.display()));
    }
    run_git(
        repo_root,
        [
            "worktree",
            "add",
            path.to_string_lossy().as_ref(),
            "-b",
            branch,
            "HEAD",
        ],
    )?;
    Ok(())
}

// Run one Git command in the given directory and return its stdout when it succeeds.
fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// Return the list of still-unmerged files after a failed merge attempt.
fn unmerged_files(cwd: &Path) -> Result<Vec<String>> {
    let output = run_git(cwd, ["diff", "--name-only", "--diff-filter=U"])?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect())
}

// Convert ids into branch-safe and path-safe segments for worktree naming.
fn sanitize_segment(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    sanitized.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use daddy_orchestrator::{Job, Task, TaskKind};

    #[test]
    // Create per-task Git worktrees inside a temporary repository and verify their branches.
    fn git_worktree_manager_prepares_one_worktree_per_task() {
        let repo = tempfile::tempdir().unwrap();
        init_test_repo(repo.path());
        let manager = GitWorktreeManager::default();
        let prepared = manager
            .prepare_set(
                &Job {
                    id: "job-1".to_string(),
                    goal: "Build auth".to_string(),
                    cwd: repo.path().to_path_buf(),
                },
                &[test_task("backend-auth"), test_task("frontend-auth")],
            )
            .unwrap();
        assert_eq!(prepared.worktrees.len(), 2);
        assert!(prepared.worktrees[0].path.exists());
        assert!(prepared.worktrees[1].path.exists());
        let branches = run_git(repo.path(), ["branch", "--list"]).unwrap();
        assert!(branches.contains("daddy/job-1/backend-auth"));
        assert!(branches.contains("daddy/job-1/frontend-auth"));
    }

    #[test]
    // Replace unsupported characters in branch and path segments so worktree names stay portable.
    fn sanitize_segment_rewrites_unsafe_characters() {
        assert_eq!(sanitize_segment("job:1/alpha"), "job-1-alpha");
    }

    #[test]
    // Merge two prepared branches into one integration branch and report that no review is required.
    fn git_merge_engine_merges_clean_branches() {
        let repo = tempfile::tempdir().unwrap();
        init_test_repo(repo.path());
        let manager = GitWorktreeManager::default();
        let prepared = manager
            .prepare_set(
                &Job {
                    id: "job-2".to_string(),
                    goal: "Build auth".to_string(),
                    cwd: repo.path().to_path_buf(),
                },
                &[test_task("backend-auth"), test_task("frontend-auth")],
            )
            .unwrap();
        std::fs::write(prepared.worktrees[0].path.join("backend.txt"), "backend\n").unwrap();
        run_git(&prepared.worktrees[0].path, ["add", "backend.txt"]).unwrap();
        run_git(&prepared.worktrees[0].path, ["commit", "-m", "backend"]).unwrap();
        std::fs::write(
            prepared.worktrees[1].path.join("frontend.txt"),
            "frontend\n",
        )
        .unwrap();
        run_git(&prepared.worktrees[1].path, ["add", "frontend.txt"]).unwrap();
        run_git(&prepared.worktrees[1].path, ["commit", "-m", "frontend"]).unwrap();
        let outcome = GitMergeEngine::default()
            .merge_prepared(
                &Job {
                    id: "job-2".to_string(),
                    goal: "Build auth".to_string(),
                    cwd: repo.path().to_path_buf(),
                },
                &prepared,
            )
            .unwrap();
        assert!(!outcome.review_required);
        assert_eq!(outcome.merged_branches.len(), 2);
        assert!(outcome.integration_path.exists());
    }

    // Initialize a throwaway Git repository with one committed file for workspace tests.
    fn init_test_repo(path: &Path) {
        run_git(path, ["init"]).unwrap();
        run_git(path, ["config", "user.email", "daddy@example.com"]).unwrap();
        run_git(path, ["config", "user.name", "Daddy"]).unwrap();
        std::fs::write(path.join("README.md"), "seed\n").unwrap();
        run_git(path, ["add", "README.md"]).unwrap();
        run_git(path, ["commit", "-m", "seed"]).unwrap();
    }

    // Create one minimal task value for worktree manager tests.
    fn test_task(id: &str) -> Task {
        Task {
            id: id.to_string(),
            title: id.to_string(),
            description: id.to_string(),
            kind: TaskKind::General,
            depends_on: Vec::new(),
            acceptance_criteria: Vec::new(),
            relevant_paths: vec!["src".to_string()],
        }
    }
}
