use anyhow::Result;
use chrono::Utc;
use daddy_orchestrator::{PlannedJob, SessionEvictionDecision};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct JobRecord {
    pub job_id: String,
    pub goal: String,
    pub planner: String,
    pub scheduler: String,
    pub router: String,
    pub total_tasks: usize,
    pub merge_status: String,
}

#[derive(Debug, Clone)]
pub struct TaskRunRecord {
    pub job_id: String,
    pub task_id: String,
    pub provider: String,
    pub result: String,
    pub trajectory_path: PathBuf,
    pub handoff_path: PathBuf,
    pub eviction: SessionEvictionDecision,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BenchmarkSummary {
    pub jobs: u64,
    pub task_runs: u64,
    pub merged_jobs: u64,
}

pub struct SqliteMemoryStore {
    path: PathBuf,
}

impl SqliteMemoryStore {
    // Create a SQLite-backed memory store rooted at the given database path.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let store = Self {
            path: path.as_ref().to_path_buf(),
        };
        store.init()?;
        Ok(store)
    }

    // Persist one planned or completed job row in the benchmark database.
    pub fn record_job(&self, record: &JobRecord) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT OR REPLACE INTO jobs (job_id, goal, planner, scheduler, router, total_tasks, merge_status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                record.job_id,
                record.goal,
                record.planner,
                record.scheduler,
                record.router,
                record.total_tasks as i64,
                record.merge_status,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    // Persist one worker outcome row for benchmark and routing history queries.
    pub fn record_task_run(&self, record: &TaskRunRecord) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO task_runs (job_id, task_id, provider, result, trajectory_path, handoff_path, restart_reason, total_tokens, turns, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                record.job_id,
                record.task_id,
                record.provider,
                record.result,
                record.trajectory_path.display().to_string(),
                record.handoff_path.display().to_string(),
                record.eviction.reason,
                record.eviction.total_tokens as i64,
                record.eviction.turns as i64,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    // Store one planned job snapshot so later components can inspect the original orchestration graph.
    pub fn record_planned_job(&self, planned: &PlannedJob) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT OR REPLACE INTO planned_jobs (job_id, payload, created_at)
             VALUES (?1, ?2, ?3)",
            params![
                planned.graph.job.id,
                serde_json::to_string(planned)?,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    // Return a compact benchmark summary for the CLI bench command.
    pub fn benchmark_summary(&self) -> Result<BenchmarkSummary> {
        let conn = self.connection()?;
        let jobs: u64 = conn.query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get(0))?;
        let task_runs: u64 =
            conn.query_row("SELECT COUNT(*) FROM task_runs", [], |row| row.get(0))?;
        let merged_jobs: u64 = conn.query_row(
            "SELECT COUNT(*) FROM jobs WHERE merge_status = 'merged'",
            [],
            |row| row.get(0),
        )?;
        Ok(BenchmarkSummary {
            jobs,
            task_runs,
            merged_jobs,
        })
    }

    // Open one SQLite connection to the backing database file.
    fn connection(&self) -> Result<Connection> {
        Ok(Connection::open(&self.path)?)
    }

    // Create the benchmark and memory tables if they do not exist yet.
    fn init(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = self.connection()?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS jobs (
                job_id TEXT PRIMARY KEY,
                goal TEXT NOT NULL,
                planner TEXT NOT NULL,
                scheduler TEXT NOT NULL,
                router TEXT NOT NULL,
                total_tasks INTEGER NOT NULL,
                merge_status TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS task_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                provider TEXT NOT NULL,
                result TEXT NOT NULL,
                trajectory_path TEXT NOT NULL,
                handoff_path TEXT NOT NULL,
                restart_reason TEXT NOT NULL,
                total_tokens INTEGER NOT NULL,
                turns INTEGER NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS planned_jobs (
                job_id TEXT PRIMARY KEY,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            ",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daddy_orchestrator::{ExecutionPlan, Job, PlannedJob, TaskGraph};

    #[test]
    // Persist benchmark rows and read back a compact summary for the CLI bench command.
    fn sqlite_memory_store_records_jobs_and_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteMemoryStore::new(dir.path().join("memory.db")).unwrap();
        store
            .record_job(&JobRecord {
                job_id: "job-1".to_string(),
                goal: "Build auth".to_string(),
                planner: "caveman".to_string(),
                scheduler: "basic".to_string(),
                router: "static".to_string(),
                total_tasks: 2,
                merge_status: "merged".to_string(),
            })
            .unwrap();
        store
            .record_planned_job(&PlannedJob {
                graph: TaskGraph {
                    job: Job {
                        id: "job-1".to_string(),
                        goal: "Build auth".to_string(),
                        cwd: dir.path().to_path_buf(),
                    },
                    tasks: Vec::new(),
                },
                execution: ExecutionPlan {
                    assignments: Vec::new(),
                    stages: Vec::new(),
                },
                contexts: Vec::new(),
            })
            .unwrap();
        store
            .record_task_run(&TaskRunRecord {
                job_id: "job-1".to_string(),
                task_id: "task-1".to_string(),
                provider: "codex".to_string(),
                result: "done".to_string(),
                trajectory_path: dir.path().join("trajectory.json"),
                handoff_path: dir.path().join("handoff.json"),
                eviction: SessionEvictionDecision {
                    task_id: "task-1".to_string(),
                    should_restart: true,
                    reason: "task_complete".to_string(),
                    total_tokens: 10,
                    turns: 1,
                },
            })
            .unwrap();
        let summary = store.benchmark_summary().unwrap();
        assert_eq!(summary.jobs, 1);
        assert_eq!(summary.task_runs, 1);
        assert_eq!(summary.merged_jobs, 1);
    }
}
