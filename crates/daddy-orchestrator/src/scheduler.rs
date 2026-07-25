use crate::models::{ExecutionPlan, ExecutionStage, TaskGraph, TaskKind, WorkerAssignment};
use daddy_core::ModelTier;

pub trait Scheduler: Send + Sync {
    fn name(&self) -> &'static str;
    fn schedule(&self, graph: &TaskGraph, providers: &[String]) -> anyhow::Result<ExecutionPlan>;
}

pub struct BasicScheduler;

impl Scheduler for BasicScheduler {
    // Return the scheduler name used in CLI output and telemetry.
    fn name(&self) -> &'static str {
        "basic"
    }

    // Build a staged execution plan with deterministic provider assignment.
    fn schedule(&self, graph: &TaskGraph, providers: &[String]) -> anyhow::Result<ExecutionPlan> {
        graph.validate()?;
        let stages = build_stages(graph)?;
        let assignments = graph
            .tasks
            .iter()
            .map(|task| WorkerAssignment {
                task_id: task.id.clone(),
                provider: pick_provider(&task.kind, providers),
                model_tier: Some(pick_model_tier(&task.kind)),
            })
            .collect();
        Ok(ExecutionPlan {
            assignments,
            stages,
        })
    }
}

// Group tasks into execution stages where each stage contains only dependency-ready tasks.
fn build_stages(graph: &TaskGraph) -> anyhow::Result<Vec<ExecutionStage>> {
    let mut completed = std::collections::BTreeSet::new();
    let mut remaining: Vec<_> = graph.tasks.iter().collect();
    let mut stages = Vec::new();
    while !remaining.is_empty() {
        let ready: Vec<_> = remaining
            .iter()
            .filter(|task| task.depends_on.iter().all(|dep| completed.contains(dep)))
            .copied()
            .collect();
        if ready.is_empty() {
            anyhow::bail!("task graph contains a cycle or unresolved dependency chain");
        }
        for task in &ready {
            completed.insert(task.id.clone());
        }
        remaining.retain(|task| !completed.contains(&task.id));
        stages.push(ExecutionStage {
            index: stages.len(),
            task_ids: ready.into_iter().map(|task| task.id.clone()).collect(),
        });
    }
    Ok(stages)
}

// Pick a provider from the available ordered list using task-kind heuristics.
fn pick_provider(kind: &TaskKind, providers: &[String]) -> String {
    if providers.is_empty() {
        return "auto".to_string();
    }
    let index = match kind {
        TaskKind::Backend | TaskKind::Bugfix | TaskKind::Refactor | TaskKind::Review => 0,
        TaskKind::Frontend => 1.min(providers.len().saturating_sub(1)),
        TaskKind::Tests | TaskKind::Docs => providers.len().saturating_sub(1),
        TaskKind::Research | TaskKind::General => 0,
    };
    providers[index].clone()
}

// Pick a model tier based on the complexity and risk of the task category.
fn pick_model_tier(kind: &TaskKind) -> ModelTier {
    match kind {
        TaskKind::Tests | TaskKind::Docs => ModelTier::Fast,
        TaskKind::Frontend => ModelTier::Fast,
        TaskKind::Backend
        | TaskKind::Bugfix
        | TaskKind::Refactor
        | TaskKind::Review
        | TaskKind::Research
        | TaskKind::General => ModelTier::Strongest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Job, Task};
    use std::path::PathBuf;

    #[test]
    // Confirm independent tasks share a stage and dependent tasks are delayed.
    fn scheduler_groups_independent_tasks_by_stage() {
        let graph = TaskGraph {
            job: Job {
                id: "job-1".to_string(),
                goal: "x".to_string(),
                cwd: PathBuf::from("."),
            },
            tasks: vec![
                Task {
                    id: "a".to_string(),
                    title: "A".to_string(),
                    description: "a".to_string(),
                    kind: TaskKind::Backend,
                    depends_on: Vec::new(),
                    acceptance_criteria: Vec::new(),
                    relevant_paths: Vec::new(),
                },
                Task {
                    id: "b".to_string(),
                    title: "B".to_string(),
                    description: "b".to_string(),
                    kind: TaskKind::Docs,
                    depends_on: Vec::new(),
                    acceptance_criteria: Vec::new(),
                    relevant_paths: Vec::new(),
                },
                Task {
                    id: "c".to_string(),
                    title: "C".to_string(),
                    description: "c".to_string(),
                    kind: TaskKind::Tests,
                    depends_on: vec!["a".to_string()],
                    acceptance_criteria: Vec::new(),
                    relevant_paths: Vec::new(),
                },
            ],
        };
        let plan = BasicScheduler
            .schedule(&graph, &["codex".to_string(), "claude".to_string()])
            .unwrap();
        assert_eq!(plan.stages.len(), 2);
        assert_eq!(plan.stages[0].task_ids.len(), 2);
        assert_eq!(plan.stages[1].task_ids, vec!["c".to_string()]);
    }
}
