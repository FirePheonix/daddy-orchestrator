use anyhow::{Result, anyhow};
use clap::{Args, Parser, Subcommand};
use daddy_core::{Agent, AgentOptions, MCPServer, ModelTier, RunOptions};
use daddy_providers::default_catalog;
use daddy_storage::{inspect_trajectory, load_trajectory};
use std::path::PathBuf;

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

    #[arg(global = true)]
    prompt: Vec<String>,
}

#[derive(Subcommand, Clone)]
enum CommandKind {
    Doctor,
    Traj(TrajArgs),
    Viewer(ViewerArgs),
    Chat(ChatArgs),
    Resume(ResumeArgs),
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
        Some(CommandKind::Doctor) => {
            print_doctor_report(&catalog);
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

// Build an Agent from CLI flags and environment-backed defaults.
fn build_agent(cli: &Cli) -> Result<Agent> {
    let model_tier = if cli.model.is_none() {
        std::env::var("DADDY_MODEL_TIER")
            .ok()
            .and_then(|tier| match tier.as_str() {
                "strongest" => Some(ModelTier::Strongest),
                "fast" => Some(ModelTier::Fast),
                _ => None,
            })
    } else {
        None
    };

    Ok(Agent::builder(default_catalog())
        .with_options(AgentOptions {
            provider: cli
                .provider
                .as_ref()
                .map(|value| parse_provider_order(value)),
            model: cli.model.clone(),
            model_tier,
            reasoning: cli.reasoning.clone(),
            system_prompt: cli.system_prompt.clone(),
            data_dir: cli.data_dir.clone(),
            cwd: cli.cwd.clone(),
            metadata: Default::default(),
            mcp_servers: load_mcp_servers(cli.mcp_config.as_ref())?,
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

// Print a human-readable health report for every registered provider.
fn print_doctor_report(catalog: &std::sync::Arc<dyn daddy_core::ProviderCatalog>) {
    for provider in catalog.providers() {
        let health = provider.check_health();
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
            "{}\tinstalled={}\tresume={}\tbinary={}\tauth={}\tauth_path={}",
            health.provider,
            health.installed,
            provider.supports_native_resume(),
            health.binary_path.unwrap_or_else(|| "-".to_string()),
            auth,
            auth_path
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
}
