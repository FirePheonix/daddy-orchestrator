use crate::models::{HandoffArtifact, SessionEvictionDecision, Task};
use daddy_core::Trajectory;

pub struct DisposableSessionPolicy {
    pub max_total_tokens: u64,
    pub max_turns: usize,
    pub restart_after_task: bool,
}

impl DisposableSessionPolicy {
    // Decide whether a worker session should be terminated and replaced after a task completes.
    pub fn evaluate(&self, task_id: &str, trajectory: &Trajectory) -> SessionEvictionDecision {
        let total_tokens = trajectory.usage.total_tokens();
        let turns = trajectory.turns.len();
        let reason = if total_tokens >= self.max_total_tokens {
            "token_threshold".to_string()
        } else if turns >= self.max_turns {
            "turn_threshold".to_string()
        } else if self.restart_after_task {
            "task_complete".to_string()
        } else {
            "keep_alive".to_string()
        };
        SessionEvictionDecision {
            task_id: task_id.to_string(),
            should_restart: reason != "keep_alive",
            reason,
            total_tokens,
            turns,
        }
    }
}

impl Default for DisposableSessionPolicy {
    // Create the default policy that treats task-scoped workers as disposable units.
    fn default() -> Self {
        Self {
            max_total_tokens: 20_000,
            max_turns: 8,
            restart_after_task: true,
        }
    }
}

// Build a handoff artifact that summarizes one task result for the next worker or reviewer.
pub fn build_handoff_artifact(
    task: &Task,
    trajectory: &Trajectory,
    changed_files: Vec<String>,
) -> HandoffArtifact {
    HandoffArtifact {
        task_id: task.id.clone(),
        summary: trajectory.result(),
        changed_files,
        unresolved_questions: if trajectory.result().trim().is_empty() {
            vec![format!(
                "Task `{}` completed without a textual result summary.",
                task.id
            )]
        } else {
            Vec::new()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TaskKind;
    use daddy_core::{ContentBlock, Turn, UsageStats};

    #[test]
    // Restart a session after the task completes even when it stays below saturation thresholds.
    fn default_policy_restarts_after_task_completion() {
        let mut trajectory = Trajectory::new("codex", "o4-mini", "session-1");
        trajectory.append_turn(Turn {
            input: "hi".to_string(),
            output: Vec::new(),
            usage: UsageStats {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: 0.0,
            },
            duration_ms: 0,
        });
        let decision = DisposableSessionPolicy::default().evaluate("task-1", &trajectory);
        assert!(decision.should_restart);
        assert_eq!(decision.reason, "task_complete");
    }

    #[test]
    // Preserve changed-file information inside the handoff artifact for follow-up workers.
    fn handoff_artifact_keeps_changed_files() {
        let mut trajectory = Trajectory::new("codex", "o4-mini", "session-1");
        trajectory.append_turn(Turn {
            input: "hi".to_string(),
            output: vec![ContentBlock::Text {
                text: "done".to_string(),
            }],
            usage: UsageStats::default(),
            duration_ms: 0,
        });
        let artifact = build_handoff_artifact(
            &Task {
                id: "task-1".to_string(),
                title: "Task".to_string(),
                description: "Task".to_string(),
                kind: TaskKind::General,
                depends_on: Vec::new(),
                acceptance_criteria: Vec::new(),
                relevant_paths: Vec::new(),
            },
            &trajectory,
            vec!["src/lib.rs".to_string()],
        );
        assert_eq!(artifact.changed_files, vec!["src/lib.rs"]);
        assert_eq!(artifact.summary, "done");
    }
}
