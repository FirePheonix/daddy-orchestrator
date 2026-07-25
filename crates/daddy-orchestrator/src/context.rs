use crate::models::{ContextBundle, TaskGraph};

pub trait ContextRouter: Send + Sync {
    fn name(&self) -> &'static str;
    fn route(&self, graph: &TaskGraph) -> anyhow::Result<Vec<ContextBundle>>;
}

pub struct StaticContextRouter;

impl ContextRouter for StaticContextRouter {
    // Return the router name used in CLI output and telemetry.
    fn name(&self) -> &'static str {
        "static"
    }

    // Build one minimal context bundle per task from the task's explicit relevant paths and criteria.
    fn route(&self, graph: &TaskGraph) -> anyhow::Result<Vec<ContextBundle>> {
        Ok(graph
            .tasks
            .iter()
            .map(|task| ContextBundle {
                task_id: task.id.clone(),
                relevant_paths: task.relevant_paths.clone(),
                snippets: Vec::new(),
                notes: task.acceptance_criteria.clone(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Job, Task, TaskGraph, TaskKind};
    use std::path::PathBuf;

    #[test]
    // Confirm the static router preserves task-scoped path hints and acceptance notes.
    fn static_router_builds_task_scoped_context() {
        let graph = TaskGraph {
            job: Job {
                id: "job-1".to_string(),
                goal: "x".to_string(),
                cwd: PathBuf::from("."),
            },
            tasks: vec![Task {
                id: "task-1".to_string(),
                title: "Task 1".to_string(),
                description: "x".to_string(),
                kind: TaskKind::General,
                depends_on: Vec::new(),
                acceptance_criteria: vec!["Must pass tests.".to_string()],
                relevant_paths: vec!["src".to_string(), "tests".to_string()],
            }],
        };
        let bundles = StaticContextRouter.route(&graph).unwrap();
        assert_eq!(bundles.len(), 1);
        assert_eq!(bundles[0].task_id, "task-1");
        assert_eq!(bundles[0].relevant_paths, vec!["src", "tests"]);
        assert_eq!(bundles[0].notes, vec!["Must pass tests."]);
    }
}
