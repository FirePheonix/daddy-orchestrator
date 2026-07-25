use crate::models::{Job, JobRequest, Task, TaskGraph, TaskKind};
use anyhow::{Result, anyhow};
use serde::Deserialize;
use std::process::Command;

pub trait Planner: Send + Sync {
    fn name(&self) -> &'static str;
    fn plan(&self, request: &JobRequest) -> Result<TaskGraph>;
}

pub struct CavemanPlanner;

pub enum PlannerBackend {
    Caveman(CavemanPlanner),
    Endpoint(EndpointPlanner),
}

pub struct EndpointPlanner {
    name: &'static str,
    endpoint: String,
    model: String,
    api_key: Option<String>,
}

impl Planner for CavemanPlanner {
    // Return the planner name used in CLI output and telemetry.
    fn name(&self) -> &'static str {
        "caveman"
    }

    // Decompose a high-level goal into a deterministic task graph using heuristics.
    fn plan(&self, request: &JobRequest) -> Result<TaskGraph> {
        let job = Job {
            id: uuid::Uuid::new_v4().to_string(),
            goal: request.goal.clone(),
            cwd: request.cwd.clone(),
        };
        let tasks = build_task_list(&request.goal);
        let graph = TaskGraph { job, tasks };
        graph.validate()?;
        Ok(graph)
    }
}

impl PlannerBackend {
    // Create the deterministic heuristic planner backend.
    pub fn caveman() -> Self {
        Self::Caveman(CavemanPlanner)
    }

    // Create an OpenAI-compatible planner backend using one HTTP endpoint and model id.
    pub fn endpoint(
        name: &'static str,
        endpoint: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self::Endpoint(EndpointPlanner {
            name,
            endpoint: endpoint.into(),
            model: model.into(),
            api_key,
        })
    }
}

impl Planner for PlannerBackend {
    // Return the active planner backend name.
    fn name(&self) -> &'static str {
        match self {
            PlannerBackend::Caveman(planner) => planner.name(),
            PlannerBackend::Endpoint(planner) => planner.name(),
        }
    }

    // Delegate planning to the configured backend implementation.
    fn plan(&self, request: &JobRequest) -> Result<TaskGraph> {
        match self {
            PlannerBackend::Caveman(planner) => planner.plan(request),
            PlannerBackend::Endpoint(planner) => planner.plan(request),
        }
    }
}

impl Planner for EndpointPlanner {
    // Return the configured endpoint planner family name.
    fn name(&self) -> &'static str {
        self.name
    }

    // Ask an OpenAI-compatible endpoint for a JSON task graph and convert it into the shared shape.
    fn plan(&self, request: &JobRequest) -> Result<TaskGraph> {
        let body = serde_json::json!({
            "model": self.model,
            "temperature": 0,
            "messages": [
                {
                    "role": "system",
                    "content": "You are a planning model. Decompose the user goal into task objects. Return only JSON shaped like {\"tasks\":[{\"id\":\"...\",\"title\":\"...\",\"description\":\"...\",\"kind\":\"general\",\"depends_on\":[],\"acceptance_criteria\":[],\"relevant_paths\":[]}]}."
                },
                {
                    "role": "user",
                    "content": request.goal
                }
            ]
        });
        let output = run_curl_request(&self.endpoint, self.api_key.as_deref(), &body)?;
        let spec = parse_endpoint_task_graph(&output)?;
        build_graph_from_spec(request, spec)
    }
}

// Run one OpenAI-compatible planning request through the system curl binary.
fn run_curl_request(
    endpoint: &str,
    api_key: Option<&str>,
    body: &serde_json::Value,
) -> Result<String> {
    let mut cmd = Command::new("curl");
    cmd.arg("--silent")
        .arg("--show-error")
        .arg("-X")
        .arg("POST")
        .arg(endpoint)
        .arg("-H")
        .arg("Content-Type: application/json");
    if let Some(api_key) = api_key {
        cmd.arg("-H")
            .arg(format!("Authorization: Bearer {api_key}"));
    }
    cmd.arg("-d").arg(body.to_string());
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "planner request failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// Convert one endpoint response into the internal task-graph specification shape.
fn parse_endpoint_task_graph(raw: &str) -> Result<TaskGraphSpec> {
    if let Ok(spec) = serde_json::from_str::<TaskGraphSpec>(raw) {
        return Ok(spec);
    }
    let envelope: ChatCompletionEnvelope = serde_json::from_str(raw)?;
    let content = envelope
        .choices
        .first()
        .and_then(|choice| choice.message.content.clone())
        .ok_or_else(|| anyhow!("planner response did not contain a message content field"))?;
    if let Ok(spec) = serde_json::from_str::<TaskGraphSpec>(&content) {
        return Ok(spec);
    }
    let extracted = extract_json_object(&content)
        .ok_or_else(|| anyhow!("planner message did not contain a parseable JSON object"))?;
    Ok(serde_json::from_str(&extracted)?)
}

// Build a validated shared task graph from the endpoint planner's task specification.
fn build_graph_from_spec(request: &JobRequest, spec: TaskGraphSpec) -> Result<TaskGraph> {
    let graph = TaskGraph {
        job: Job {
            id: uuid::Uuid::new_v4().to_string(),
            goal: request.goal.clone(),
            cwd: request.cwd.clone(),
        },
        tasks: spec.tasks.into_iter().map(task_from_spec).collect(),
    };
    graph.validate()?;
    Ok(graph)
}

// Convert one planner task specification into the shared task model.
fn task_from_spec(spec: TaskSpec) -> Task {
    Task {
        id: spec.id,
        title: spec.title,
        description: spec.description,
        kind: parse_task_kind(spec.kind.as_deref()),
        depends_on: spec.depends_on,
        acceptance_criteria: spec.acceptance_criteria,
        relevant_paths: spec.relevant_paths,
    }
}

// Parse the planner's task-kind string into the shared task-kind enum.
fn parse_task_kind(value: Option<&str>) -> TaskKind {
    match value.unwrap_or("general") {
        "backend" => TaskKind::Backend,
        "frontend" => TaskKind::Frontend,
        "tests" => TaskKind::Tests,
        "docs" => TaskKind::Docs,
        "refactor" => TaskKind::Refactor,
        "bugfix" => TaskKind::Bugfix,
        "review" => TaskKind::Review,
        "research" => TaskKind::Research,
        _ => TaskKind::General,
    }
}

// Extract the first top-level JSON object from a planner message that may contain prose wrappers.
fn extract_json_object(value: &str) -> Option<String> {
    let start = value.find('{')?;
    let end = value.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(value[start..=end].to_string())
}

#[derive(Debug, Deserialize)]
struct TaskGraphSpec {
    tasks: Vec<TaskSpec>,
}

#[derive(Debug, Deserialize)]
struct TaskSpec {
    id: String,
    title: String,
    description: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    acceptance_criteria: Vec<String>,
    #[serde(default)]
    relevant_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionEnvelope {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

// Build a deterministic task list from simple goal classification heuristics.
fn build_task_list(goal: &str) -> Vec<Task> {
    let lower = goal.to_lowercase();
    if contains_any(&lower, &["oauth", "auth", "login", "signin", "signup"]) {
        return auth_task_list();
    }
    if contains_any(&lower, &["test", "failing", "broken", "bug", "fix"]) {
        return bugfix_task_list();
    }
    if contains_any(&lower, &["refactor", "cleanup", "restructure", "simplify"]) {
        return refactor_task_list();
    }
    general_task_list(goal)
}

// Check whether the goal contains any of the given marker words.
fn contains_any(goal: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| goal.contains(marker))
}

// Return the default task breakdown for authentication-oriented goals.
fn auth_task_list() -> Vec<Task> {
    vec![
        task(
            "backend-auth",
            "Implement auth backend flow",
            "Add or update the backend authentication flow and supporting server-side logic.",
            TaskKind::Backend,
            Vec::new(),
            vec![
                "Server-side authentication flow is implemented.".to_string(),
                "Required environment or config changes are documented in code comments or task notes."
                    .to_string(),
            ],
            vec!["src".to_string(), "server".to_string(), "api".to_string()],
        ),
        task(
            "frontend-auth",
            "Implement auth frontend flow",
            "Add or update the frontend login and session user experience.",
            TaskKind::Frontend,
            vec!["backend-auth".to_string()],
            vec![
                "Login or auth UI is wired to the backend flow.".to_string(),
                "User-facing errors and success states are handled.".to_string(),
            ],
            vec!["src".to_string(), "app".to_string(), "components".to_string()],
        ),
        task(
            "auth-tests",
            "Add auth coverage",
            "Create or update tests that validate the authentication flow.",
            TaskKind::Tests,
            vec!["backend-auth".to_string(), "frontend-auth".to_string()],
            vec!["Automated coverage exists for the new auth flow.".to_string()],
            vec!["tests".to_string(), "src".to_string()],
        ),
        task(
            "auth-docs",
            "Document auth flow changes",
            "Summarize the auth changes and any setup requirements.",
            TaskKind::Docs,
            vec!["backend-auth".to_string()],
            vec!["Operational setup notes are documented.".to_string()],
            vec!["README.md".to_string(), "docs".to_string()],
        ),
    ]
}

// Return the default task breakdown for bug-fix or failing-test goals.
fn bugfix_task_list() -> Vec<Task> {
    vec![
        task(
            "diagnose",
            "Diagnose the failing behavior",
            "Identify the failing path, likely root cause, and the files involved.",
            TaskKind::Research,
            Vec::new(),
            vec!["The likely failure cause is identified.".to_string()],
            vec!["src".to_string(), "tests".to_string()],
        ),
        task(
            "patch",
            "Implement the bug fix",
            "Change the affected code path to resolve the diagnosed issue.",
            TaskKind::Bugfix,
            vec!["diagnose".to_string()],
            vec!["The broken behavior is corrected.".to_string()],
            vec!["src".to_string()],
        ),
        task(
            "regression-tests",
            "Add regression coverage",
            "Add or update tests that fail before the fix and pass after it.",
            TaskKind::Tests,
            vec!["patch".to_string()],
            vec!["Regression coverage is present.".to_string()],
            vec!["tests".to_string(), "src".to_string()],
        ),
    ]
}

// Return the default task breakdown for refactor-oriented goals.
fn refactor_task_list() -> Vec<Task> {
    vec![
        task(
            "refactor-core",
            "Refactor the targeted code path",
            "Restructure the implementation without changing the intended behavior.",
            TaskKind::Refactor,
            Vec::new(),
            vec!["The target implementation is simplified or reorganized.".to_string()],
            vec!["src".to_string()],
        ),
        task(
            "refactor-tests",
            "Validate refactor behavior",
            "Add or update tests that confirm the refactor preserved behavior.",
            TaskKind::Tests,
            vec!["refactor-core".to_string()],
            vec!["Behavior-preserving coverage exists.".to_string()],
            vec!["tests".to_string(), "src".to_string()],
        ),
    ]
}

// Return a small default task breakdown for general implementation goals.
fn general_task_list(goal: &str) -> Vec<Task> {
    vec![
        task(
            "implement",
            "Implement the requested change",
            goal,
            TaskKind::General,
            Vec::new(),
            vec!["The requested change is implemented.".to_string()],
            vec!["src".to_string()],
        ),
        task(
            "validate",
            "Validate the change with tests",
            "Add or update tests that confirm the requested change works.",
            TaskKind::Tests,
            vec!["implement".to_string()],
            vec!["The change is validated by tests.".to_string()],
            vec!["tests".to_string(), "src".to_string()],
        ),
    ]
}

// Create one task value with a stable shape used by the heuristic planner.
fn task(
    id: &str,
    title: &str,
    description: &str,
    kind: TaskKind,
    depends_on: Vec<String>,
    acceptance_criteria: Vec<String>,
    relevant_paths: Vec<String>,
) -> Task {
    Task {
        id: id.to_string(),
        title: title.to_string(),
        description: description.to_string(),
        kind,
        depends_on,
        acceptance_criteria,
        relevant_paths,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    // Confirm auth-oriented goals split into backend, frontend, tests, and docs tasks.
    fn caveman_planner_splits_auth_goal() {
        let planner = CavemanPlanner;
        let graph = planner
            .plan(&JobRequest {
                goal: "Build OAuth login".to_string(),
                cwd: PathBuf::from("."),
                provider_order: vec!["codex".to_string(), "claude".to_string()],
            })
            .unwrap();
        assert_eq!(graph.tasks.len(), 4);
        assert_eq!(graph.tasks[0].kind, TaskKind::Backend);
        assert_eq!(graph.tasks[1].kind, TaskKind::Frontend);
        assert_eq!(graph.tasks[2].kind, TaskKind::Tests);
    }

    #[test]
    // Confirm unknown dependencies are rejected by task-graph validation.
    fn task_graph_validation_rejects_missing_dependency() {
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
                depends_on: vec!["missing".to_string()],
                acceptance_criteria: Vec::new(),
                relevant_paths: Vec::new(),
            }],
        };
        assert!(graph.validate().is_err());
    }

    #[test]
    // Accept a bare JSON task graph from an endpoint planner response.
    fn endpoint_parser_accepts_direct_json() {
        let spec = parse_endpoint_task_graph(
            r#"{"tasks":[{"id":"task-1","title":"Task","description":"Do work","kind":"backend","depends_on":[],"acceptance_criteria":[],"relevant_paths":["src"]}]}"#,
        )
        .unwrap();
        assert_eq!(spec.tasks.len(), 1);
        assert_eq!(spec.tasks[0].id, "task-1");
    }

    #[test]
    // Accept an OpenAI-compatible envelope whose message content contains the JSON task graph.
    fn endpoint_parser_accepts_chat_completion_envelope() {
        let spec = parse_endpoint_task_graph(
            r#"{"choices":[{"message":{"content":"{\"tasks\":[{\"id\":\"task-1\",\"title\":\"Task\",\"description\":\"Do work\",\"kind\":\"frontend\",\"depends_on\":[],\"acceptance_criteria\":[],\"relevant_paths\":[\"app\"]}]}"}}]}"#,
        )
        .unwrap();
        assert_eq!(spec.tasks.len(), 1);
        assert_eq!(spec.tasks[0].title, "Task");
    }
}
