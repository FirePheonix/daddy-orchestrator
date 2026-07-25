use anyhow::{Result, anyhow};
use clap::{Args, Parser, Subcommand};
use daddy_core::{Agent, AgentOptions, MCPServer, ModelTier, RunOptions};
use daddy_orchestrator::{
    BasicScheduler, CavemanPlanner, JobRequest, Orchestrator, StaticContextRouter,
};
use daddy_providers::default_catalog;
use daddy_storage::{inspect_trajectory, load_trajectory};
use daddy_workspace::GitWorktreeManager;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "daddy", about = "Rust wrapper for coding-agent CLIs")]
struct Cli {
    #[command(subcommand)]
    command: Option<CommandKind>,

    #[arg(global = true, long)]
    provider: Option<String>,

    #[arg(global = true, long)]
    model: Option<String>,

    #[arg(global = true, long)]
    reasoning: Option<String>,

    #[arg(global = true, long)]
    system_prompt: Option<String>,

    #[arg(global = true, long)]
    data_dir: Option<PathBuf>,

    #[arg(global = true, long)]
    cwd: Option<PathBuf>,

    #[arg(global = true, long)]
    traj_path: Option<PathBuf>,

    #[arg(global = true, long)]
    mcp_config: Option<PathBuf>,

    #[arg(global = true, long)]
    config: Option<PathBuf>,

    #[arg()]
    prompt: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CliConfig {
    #[serde(default)]
    provider: Option<ProviderConfigValue>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    model_tier: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    data_dir: Option<PathBuf>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    mcp_servers: Vec<MCPServer>,
    #[serde(default)]
    mcp_config: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ProviderConfigValue {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Subcommand, Clone)]
enum CommandKind {
    Doctor(DoctorArgs),
    Traj(TrajArgs),
    Viewer(ViewerArgs),
    Chat(ChatArgs),
    Resume(ResumeArgs),
    Run(RunArgs),
}

#[derive(Args, Clone)]
struct DoctorArgs {
    #[arg(long)]
    live: bool,
}

#[derive(Args, Clone)]
struct TrajArgs {
    path: PathBuf,
}

#[derive(Args, Clone)]
struct ViewerArgs {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 7878)]
    port: u16,
}

#[derive(Args, Clone)]
struct ChatArgs {
    #[arg(long)]
    save: Option<PathBuf>,
}

#[derive(Args, Clone)]
struct ResumeArgs {
    source: String,
    prompt: Vec<String>,
}

#[derive(Args, Clone)]
struct RunArgs {
    goal: Vec<String>,

    #[arg(long, default_value = "caveman")]
    planner: String,

    #[arg(long)]
    json: bool,

    #[arg(long)]
    prepare_worktrees: bool,

    #[arg(long)]
    execute: bool,
}

#[derive(Debug, serde::Serialize)]
struct RunCommandOutput {
    planned: daddy_orchestrator::PlannedJob,
    #[serde(skip_serializing_if = "Option::is_none")]
    prepared_workspaces: Option<daddy_workspace::PreparedWorkspaceSet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    executed_tasks: Option<Vec<ExecutedTask>>,
}

#[derive(Debug, serde::Serialize)]
struct ExecutedTask {
    task_id: String,
    provider: String,
    workspace: PathBuf,
    trajectory_path: PathBuf,
    result: String,
}

struct TaskAgentSpec {
    catalog: Arc<dyn daddy_core::ProviderCatalog>,
    provider: String,
    model_tier: Option<ModelTier>,
    cwd: PathBuf,
    model: Option<String>,
    reasoning: Option<String>,
    system_prompt: Option<String>,
    mcp_servers: Vec<MCPServer>,
}

#[tokio::main]
// Run the main `daddy` CLI entrypoint.
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .without_time()
        .init();
    let cli = Cli::parse();
    let catalog = default_catalog();

    match cli.command.clone() {
        Some(CommandKind::Doctor(args)) => {
            print_doctor_report(&catalog, args.live, cli.model.as_deref());
        }
        Some(CommandKind::Traj(args)) => {
            println!("{}", inspect_trajectory(args.path)?);
        }
        Some(CommandKind::Viewer(args)) => {
            daddy_viewer::serve(args.host, args.port).await?;
        }
        Some(CommandKind::Chat(args)) => {
            let agent = build_agent(&cli)?;
            let mut session = agent.start_session(RunOptions {
                traj_path: args.save.or(cli.traj_path.clone()),
            })?;
            let mut input = String::new();
            loop {
                input.clear();
                print!("you> ");
                use std::io::Write;
                std::io::stdout().flush()?;
                if std::io::stdin().read_line(&mut input)? == 0 {
                    break;
                }
                let trimmed = input.trim();
                if trimmed.eq_ignore_ascii_case("/exit") || trimmed.eq_ignore_ascii_case("/quit") {
                    break;
                }
                let turn = session.send(trimmed)?;
                println!("assistant> {}", turn.result());
            }
            let trajectory = session.end()?;
            println!("saved session {}", trajectory.session_id);
        }
        Some(CommandKind::Resume(args)) => {
            let mut session = resume_session_from_source(&catalog, &cli, &args)?;
            let prompt = args.prompt.join(" ");
            let turn = session.send(&prompt)?;
            println!("{}", turn.result());
            if let Some(save_path) = cli.traj_path {
                session.save_trajectory(save_path)?;
            }
        }
        Some(CommandKind::Run(args)) => {
            let config = config_for_cli(&cli)?;
            let request = build_job_request(&cli, config.as_ref(), &args)?;
            let orchestrator =
                Orchestrator::new(CavemanPlanner, BasicScheduler, StaticContextRouter);
            let planned = orchestrator.plan_job(&request)?;
            let prepared_workspaces = if args.prepare_worktrees || args.execute {
                Some(
                    GitWorktreeManager::default()
                        .prepare_set(&planned.graph.job, &planned.graph.tasks)?,
                )
            } else {
                None
            };
            let executed_tasks = if args.execute {
                Some(execute_planned_job(
                    &cli,
                    config.as_ref(),
                    &planned,
                    prepared_workspaces.as_ref(),
                )?)
            } else {
                None
            };
            let output = RunCommandOutput {
                planned,
                prepared_workspaces,
                executed_tasks,
            };
            if args.json {
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                print_run_summary(
                    &output,
                    orchestrator.planner_name(),
                    orchestrator.scheduler_name(),
                    orchestrator.router_name(),
                );
            }
        }
        None => {
            if cli.prompt.is_empty() {
                return Err(anyhow!("provide a prompt or a subcommand"));
            }
            let agent = build_agent(&cli)?;
            let trajectory = agent.completion(
                &cli.prompt.join(" "),
                RunOptions {
                    traj_path: cli.traj_path,
                },
            )?;
            println!("{}", trajectory.result());
        }
    }

    Ok(())
}

// Load config once for `run` mode without constructing the worker agent layer.
fn config_for_cli(cli: &Cli) -> Result<Option<CliConfig>> {
    load_cli_config(cli.config.as_ref())
}

// Build an Agent from CLI flags and environment-backed defaults.
fn build_agent(cli: &Cli) -> Result<Agent> {
    let config = load_cli_config(cli.config.as_ref())?;
    let model_tier = resolve_model_tier(cli.model.as_ref(), config.as_ref());
    let mcp_servers = resolve_mcp_servers(cli, config.as_ref())?;

    Ok(Agent::builder(default_catalog())
        .with_options(AgentOptions {
            provider: resolve_provider_order(cli, config.as_ref()),
            model: cli
                .model
                .clone()
                .or_else(|| config.as_ref().and_then(|cfg| cfg.model.clone())),
            model_tier,
            reasoning: cli
                .reasoning
                .clone()
                .or_else(|| config.as_ref().and_then(|cfg| cfg.reasoning.clone())),
            system_prompt: cli
                .system_prompt
                .clone()
                .or_else(|| config.as_ref().and_then(|cfg| cfg.system_prompt.clone())),
            data_dir: cli
                .data_dir
                .clone()
                .or_else(|| config.as_ref().and_then(|cfg| cfg.data_dir.clone())),
            cwd: cli
                .cwd
                .clone()
                .or_else(|| config.as_ref().and_then(|cfg| cfg.cwd.clone())),
            metadata: Default::default(),
            mcp_servers,
        })
        .build())
}

// Parse a comma-separated provider order from a CLI flag value.
fn parse_provider_order(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect()
}

// Load a CLI config file explicitly or from the default project path.
fn load_cli_config(path: Option<&PathBuf>) -> Result<Option<CliConfig>> {
    let Some(path) = resolve_config_path(path) else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(&raw)?))
}

// Resolve the project config path from a flag or the default local filename.
fn resolve_config_path(path: Option<&PathBuf>) -> Option<PathBuf> {
    if let Some(path) = path {
        return Some(path.clone());
    }
    let default = PathBuf::from("daddy.json");
    if default.exists() {
        Some(default)
    } else {
        None
    }
}

// Resolve the provider fallback order from flags first, then config.
fn resolve_provider_order(cli: &Cli, config: Option<&CliConfig>) -> Option<Vec<String>> {
    cli.provider
        .as_ref()
        .map(|value| parse_provider_order(value))
        .or_else(|| config.and_then(|cfg| cfg.provider.as_ref().map(parse_provider_config_value)))
}

// Convert a config provider field into the ordered provider list used by the agent.
fn parse_provider_config_value(value: &ProviderConfigValue) -> Vec<String> {
    match value {
        ProviderConfigValue::Single(value) => parse_provider_order(value),
        ProviderConfigValue::Multiple(values) => values
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .collect(),
    }
}

// Resolve the model tier from flags, then config, then the environment fallback.
fn resolve_model_tier(model: Option<&String>, config: Option<&CliConfig>) -> Option<ModelTier> {
    if model.is_some() {
        return None;
    }
    config
        .and_then(|cfg| cfg.model_tier.as_deref())
        .and_then(parse_model_tier)
        .or_else(|| {
            std::env::var("DADDY_MODEL_TIER")
                .ok()
                .as_deref()
                .and_then(parse_model_tier)
        })
}

// Parse a model tier string into the shared tier enum.
fn parse_model_tier(value: &str) -> Option<ModelTier> {
    match value {
        "strongest" => Some(ModelTier::Strongest),
        "fast" => Some(ModelTier::Fast),
        _ => None,
    }
}

// Build one high-level orchestration request from CLI flags and project config.
fn build_job_request(cli: &Cli, config: Option<&CliConfig>, args: &RunArgs) -> Result<JobRequest> {
    if args.goal.is_empty() {
        return Err(anyhow!("provide a goal for `daddy run`"));
    }
    Ok(JobRequest {
        goal: args.goal.join(" "),
        cwd: cli
            .cwd
            .clone()
            .or_else(|| config.and_then(|cfg| cfg.cwd.clone()))
            .unwrap_or(std::env::current_dir()?),
        provider_order: resolve_provider_order(cli, config).unwrap_or_else(|| {
            vec![
                "codex".to_string(),
                "claude".to_string(),
                "opencode".to_string(),
            ]
        }),
    })
}

// Resolve MCP servers from a flag-driven config file first, then inline config entries.
fn resolve_mcp_servers(cli: &Cli, config: Option<&CliConfig>) -> Result<Vec<MCPServer>> {
    if cli.mcp_config.is_some() {
        return load_mcp_servers(cli.mcp_config.as_ref());
    }
    if let Some(path) = config.and_then(|cfg| cfg.mcp_config.as_ref()) {
        return load_mcp_servers(Some(path));
    }
    Ok(config
        .map(|cfg| cfg.mcp_servers.clone())
        .unwrap_or_default())
}

// Print a human-readable health report for every registered provider.
fn print_doctor_report(
    catalog: &std::sync::Arc<dyn daddy_core::ProviderCatalog>,
    live: bool,
    model: Option<&str>,
) {
    for provider in catalog.providers() {
        let health = if live {
            provider.check_health_live(model)
        } else {
            provider.check_health()
        };
        let auth = health
            .auth
            .as_ref()
            .map(|signal| signal.detail.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let auth_path = health
            .auth
            .as_ref()
            .and_then(|signal| signal.credentials_path.clone())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{}\tinstalled={}\tresume={}\tbinary={}\tauth={}\tauth_path={}\tprobed={}\trate_limited={}\twait_minutes={}\terror={}",
            health.provider,
            health.installed,
            provider.supports_native_resume(),
            health.binary_path.unwrap_or_else(|| "-".to_string()),
            auth,
            auth_path,
            health.probed,
            health
                .rate_limited
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            health
                .wait_minutes
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            health.error.unwrap_or_else(|| "-".to_string())
        );
    }
}

// Load MCP server definitions from a JSON file when one is configured.
fn load_mcp_servers(path: Option<&PathBuf>) -> Result<Vec<MCPServer>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let raw = std::fs::read_to_string(path)?;
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    if let Ok(servers) = serde_json::from_value::<Vec<MCPServer>>(value.clone()) {
        return Ok(servers);
    }
    if let Some(servers) = value.get("mcp_servers").or_else(|| value.get("servers")) {
        return Ok(serde_json::from_value(servers.clone())?);
    }
    Err(anyhow!(
        "MCP config must be either a JSON array or an object with `mcp_servers` or `servers`"
    ))
}

// Print a compact orchestration summary for a planned job.
fn print_run_summary(
    output: &RunCommandOutput,
    planner_name: &str,
    scheduler_name: &str,
    router_name: &str,
) {
    println!("job {}", output.planned.graph.job.id);
    println!("planner {planner_name}");
    println!("scheduler {scheduler_name}");
    println!("router {router_name}");
    println!("goal {}", output.planned.graph.job.goal);
    println!("tasks {}", output.planned.graph.tasks.len());
    if let Some(prepared) = &output.prepared_workspaces {
        println!("worktree_root {}", prepared.worktree_root.display());
        println!("prepared_worktrees {}", prepared.worktrees.len());
    }
    for stage in &output.planned.execution.stages {
        println!("stage {}", stage.index);
        for task_id in &stage.task_ids {
            let Some(task) = output
                .planned
                .graph
                .tasks
                .iter()
                .find(|task| &task.id == task_id)
            else {
                continue;
            };
            let assignment = output
                .planned
                .execution
                .assignments
                .iter()
                .find(|assignment| assignment.task_id == task.id);
            let provider = assignment
                .map(|assignment| assignment.provider.as_str())
                .unwrap_or("auto");
            let model_tier = assignment
                .and_then(|assignment| assignment.model_tier.as_ref())
                .map(|tier| match tier {
                    ModelTier::Strongest => "strongest",
                    ModelTier::Fast => "fast",
                })
                .unwrap_or("auto");
            let context = output
                .planned
                .contexts
                .iter()
                .find(|context| context.task_id == task.id)
                .map(|context| context.relevant_paths.join(","))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "-".to_string());
            let workspace = output
                .prepared_workspaces
                .as_ref()
                .and_then(|prepared| {
                    prepared
                        .worktrees
                        .iter()
                        .find(|worktree| worktree.task_id == task.id)
                })
                .map(|worktree| worktree.path.display().to_string())
                .unwrap_or_else(|| "-".to_string());
            println!(
                "  {} [{}] provider={} tier={} context={} workspace={}",
                task.title, task.id, provider, model_tier, context, workspace
            );
        }
    }
    if let Some(executed_tasks) = &output.executed_tasks {
        println!("executed_tasks {}", executed_tasks.len());
        for executed in executed_tasks {
            println!(
                "  executed {} provider={} workspace={} trajectory={}",
                executed.task_id,
                executed.provider,
                executed.workspace.display(),
                executed.trajectory_path.display()
            );
        }
    }
}

// Execute the planned job stage by stage and return one result record per completed task.
fn execute_planned_job(
    cli: &Cli,
    config: Option<&CliConfig>,
    planned: &daddy_orchestrator::PlannedJob,
    prepared_workspaces: Option<&daddy_workspace::PreparedWorkspaceSet>,
) -> Result<Vec<ExecutedTask>> {
    let catalog = default_catalog();
    let mcp_servers = resolve_mcp_servers(cli, config)?;
    let mut executed_tasks = Vec::new();
    for stage in &planned.execution.stages {
        let stage_results = std::thread::scope(|scope| {
            let mut handles: Vec<std::thread::ScopedJoinHandle<'_, Result<ExecutedTask>>> =
                Vec::new();
            for task_id in &stage.task_ids {
                let task = planned
                    .graph
                    .tasks
                    .iter()
                    .find(|task| &task.id == task_id)
                    .cloned()
                    .ok_or_else(|| anyhow!("unknown planned task: {task_id}"))?;
                let assignment = planned
                    .execution
                    .assignments
                    .iter()
                    .find(|assignment| assignment.task_id == task.id)
                    .cloned()
                    .ok_or_else(|| anyhow!("missing assignment for task {}", task.id))?;
                let context = planned
                    .contexts
                    .iter()
                    .find(|context| context.task_id == task.id)
                    .cloned()
                    .ok_or_else(|| anyhow!("missing context bundle for task {}", task.id))?;
                let workspace =
                    task_workspace_path(&planned.graph.job, prepared_workspaces, &task.id)?;
                let trajectory_path = task_trajectory_path(&planned.graph.job, &task.id);
                let catalog = catalog.clone();
                let model = cli
                    .model
                    .clone()
                    .or_else(|| config.and_then(|cfg| cfg.model.clone()));
                let reasoning = cli
                    .reasoning
                    .clone()
                    .or_else(|| config.and_then(|cfg| cfg.reasoning.clone()));
                let system_prompt = cli
                    .system_prompt
                    .clone()
                    .or_else(|| config.and_then(|cfg| cfg.system_prompt.clone()));
                let mcp_servers = mcp_servers.clone();
                handles.push(scope.spawn(move || -> Result<ExecutedTask> {
                    let agent = build_task_agent(TaskAgentSpec {
                        catalog,
                        provider: assignment.provider.clone(),
                        model_tier: assignment.model_tier.clone(),
                        cwd: workspace.clone(),
                        model,
                        reasoning,
                        system_prompt,
                        mcp_servers,
                    });
                    let trajectory = agent.completion(
                        &build_task_prompt(&planned.graph.job.goal, &task, &context),
                        RunOptions {
                            traj_path: Some(trajectory_path.clone()),
                        },
                    )?;
                    Ok(ExecutedTask {
                        task_id: task.id,
                        provider: assignment.provider,
                        workspace,
                        trajectory_path,
                        result: trajectory.result(),
                    })
                }));
            }
            let mut stage_results = Vec::new();
            for handle in handles {
                stage_results.push(handle.join().unwrap()?);
            }
            Ok::<Vec<ExecutedTask>, anyhow::Error>(stage_results)
        })?;
        executed_tasks.extend(stage_results);
    }
    Ok(executed_tasks)
}

// Build a task-scoped worker agent that targets one provider and one workspace.
fn build_task_agent(spec: TaskAgentSpec) -> Agent {
    Agent::builder(spec.catalog)
        .with_options(AgentOptions {
            provider: Some(vec![spec.provider]),
            model: spec.model,
            model_tier: spec.model_tier,
            reasoning: spec.reasoning,
            system_prompt: spec.system_prompt,
            data_dir: None,
            cwd: Some(spec.cwd),
            metadata: Default::default(),
            mcp_servers: spec.mcp_servers,
        })
        .build()
}

// Render a task prompt that keeps the worker focused on one scoped change set.
fn build_task_prompt(
    job_goal: &str,
    task: &daddy_orchestrator::Task,
    context: &daddy_orchestrator::ContextBundle,
) -> String {
    let acceptance = if context.notes.is_empty() {
        "- Complete the task safely.".to_string()
    } else {
        context
            .notes
            .iter()
            .map(|note| format!("- {note}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let relevant_paths = if context.relevant_paths.is_empty() {
        "- Use repository discovery carefully and keep scope narrow.".to_string()
    } else {
        context
            .relevant_paths
            .iter()
            .map(|path| format!("- {path}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "High-level goal:\n{job_goal}\n\nTask:\n{}\n\nTask description:\n{}\n\nRelevant paths:\n{}\n\nAcceptance criteria:\n{}\n\nRules:\n- Work only on this task.\n- Do not rewrite unrelated files.\n- Prefer the listed paths first.\n- Leave the workspace in a reviewable state.",
        task.title, task.description, relevant_paths, acceptance
    )
}

// Resolve the workspace path for one task, falling back to the job root when no isolated worktree was prepared.
fn task_workspace_path(
    job: &daddy_orchestrator::Job,
    prepared_workspaces: Option<&daddy_workspace::PreparedWorkspaceSet>,
    task_id: &str,
) -> Result<PathBuf> {
    if let Some(prepared) = prepared_workspaces {
        let worktree = prepared
            .worktrees
            .iter()
            .find(|worktree| worktree.task_id == task_id)
            .ok_or_else(|| anyhow!("missing prepared worktree for task {task_id}"))?;
        Ok(worktree.path.clone())
    } else {
        Ok(job.cwd.clone())
    }
}

// Compute the trajectory path used to persist one task-scoped worker run.
fn task_trajectory_path(job: &daddy_orchestrator::Job, task_id: &str) -> PathBuf {
    job.cwd
        .join(".daddy")
        .join("runs")
        .join(&job.id)
        .join(task_id)
        .join("trajectory.json")
}

// Resume a session either from a saved trajectory path or a JSON resume handle.
fn resume_session_from_source(
    catalog: &std::sync::Arc<dyn daddy_core::ProviderCatalog>,
    cli: &Cli,
    args: &ResumeArgs,
) -> Result<daddy_core::Session> {
    let candidate_path = PathBuf::from(&args.source);
    if candidate_path.exists() {
        let traj = load_trajectory(&candidate_path)?;
        let provider = catalog
            .get(&traj.agent)
            .ok_or_else(|| anyhow!("unknown provider in trajectory: {}", traj.agent))?;
        daddy_core::Session::from_trajectory(provider, &candidate_path, cli.cwd.clone())
    } else {
        let value: serde_json::Value = serde_json::from_str(&args.source)?;
        let provider_name = value
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("resume handle is missing a provider field"))?;
        let provider = catalog
            .get(provider_name)
            .ok_or_else(|| anyhow!("unknown provider in resume handle: {provider_name}"))?;
        daddy_core::Session::from_resume_handle(provider, &args.source, cli.cwd.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Accept a bare JSON array of MCP server definitions.
    fn load_mcp_servers_accepts_array_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(
            &path,
            r#"[{"name":"calc","command":"node","args":["server.js"],"env":{"A":"1"},"url":""}]"#,
        )
        .unwrap();
        let servers = load_mcp_servers(Some(&path)).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "calc");
    }

    #[test]
    // Accept an object that wraps MCP server definitions under `mcp_servers`.
    fn load_mcp_servers_accepts_object_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(
            &path,
            r#"{"mcp_servers":[{"name":"web","command":"","args":[],"env":{},"url":"http://localhost:9000"}]}"#,
        )
        .unwrap();
        let servers = load_mcp_servers(Some(&path)).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].url, "http://localhost:9000");
    }

    #[test]
    // Load a project config from an explicit path and preserve its defaults.
    fn load_cli_config_reads_explicit_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daddy.json");
        std::fs::write(
            &path,
            r#"{"provider":["codex","claude"],"model":"gpt-test","mcp_servers":[{"name":"calc","command":"node","args":[],"env":{},"url":""}]}"#,
        )
        .unwrap();
        let config = load_cli_config(Some(&path)).unwrap().unwrap();
        assert_eq!(config.model.as_deref(), Some("gpt-test"));
        assert_eq!(
            parse_provider_config_value(config.provider.as_ref().unwrap()),
            vec!["codex".to_string(), "claude".to_string()]
        );
        assert_eq!(config.mcp_servers.len(), 1);
    }

    #[test]
    // Let an explicit CLI provider override the provider order from config.
    fn resolve_provider_order_prefers_cli_over_config() {
        let cli = Cli {
            command: None,
            provider: Some("opencode,codex".to_string()),
            model: None,
            reasoning: None,
            system_prompt: None,
            data_dir: None,
            cwd: None,
            traj_path: None,
            mcp_config: None,
            config: None,
            prompt: Vec::new(),
        };
        let config = CliConfig {
            provider: Some(ProviderConfigValue::Single("claude".to_string())),
            ..Default::default()
        };
        assert_eq!(
            resolve_provider_order(&cli, Some(&config)).unwrap(),
            vec!["opencode".to_string(), "codex".to_string()]
        );
    }

    #[test]
    // Build a high-level orchestration request from CLI provider order and prompt text.
    fn build_job_request_uses_cli_provider_order() {
        let cli = Cli {
            command: Some(CommandKind::Run(RunArgs {
                goal: vec!["Build".to_string(), "OAuth".to_string()],
                planner: "caveman".to_string(),
                json: false,
                prepare_worktrees: false,
                execute: false,
            })),
            provider: Some("claude,codex".to_string()),
            model: None,
            reasoning: None,
            system_prompt: None,
            data_dir: None,
            cwd: None,
            traj_path: None,
            mcp_config: None,
            config: None,
            prompt: Vec::new(),
        };
        let request = build_job_request(
            &cli,
            None,
            match cli.command.as_ref().unwrap() {
                CommandKind::Run(args) => args,
                _ => panic!("expected run args"),
            },
        )
        .unwrap();
        assert_eq!(request.goal, "Build OAuth");
        assert_eq!(
            request.provider_order,
            vec!["claude".to_string(), "codex".to_string()]
        );
    }
}
