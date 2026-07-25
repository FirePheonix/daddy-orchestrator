use daddy_core::ModelTier;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobRequest {
    pub goal: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub provider_order: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub goal: String,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Backend,
    Frontend,
    Tests,
    Docs,
    Refactor,
    Bugfix,
    Review,
    Research,
    General,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: String,
    pub kind: TaskKind,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub relevant_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskGraph {
    pub job: Job,
    pub tasks: Vec<Task>,
}

impl TaskGraph {
    // Verify that the task graph uses unique task ids and only references existing dependencies.
    pub fn validate(&self) -> anyhow::Result<()> {
        let mut ids = std::collections::BTreeSet::new();
        for task in &self.tasks {
            if !ids.insert(task.id.clone()) {
                anyhow::bail!("duplicate task id: {}", task.id);
            }
        }
        for task in &self.tasks {
            for dependency in &task.depends_on {
                if !ids.contains(dependency) {
                    anyhow::bail!("task {} depends on missing task {}", task.id, dependency);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerAssignment {
    pub task_id: String,
    pub provider: String,
    #[serde(default)]
    pub model_tier: Option<ModelTier>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionStage {
    pub index: usize,
    pub task_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionPlan {
    pub assignments: Vec<WorkerAssignment>,
    pub stages: Vec<ExecutionStage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextBundle {
    pub task_id: String,
    #[serde(default)]
    pub relevant_paths: Vec<String>,
    #[serde(default)]
    pub snippets: Vec<String>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HandoffArtifact {
    pub task_id: String,
    pub summary: String,
    #[serde(default)]
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub unresolved_questions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionEvictionDecision {
    pub task_id: String,
    pub should_restart: bool,
    pub reason: String,
    pub total_tokens: u64,
    pub turns: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlannedJob {
    pub graph: TaskGraph,
    pub execution: ExecutionPlan,
    #[serde(default)]
    pub contexts: Vec<ContextBundle>,
}
