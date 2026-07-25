use anyhow::{Result, anyhow};
use clap::{Args, Parser, Subcommand};
use daddy_core::{Agent, AgentOptions, ModelTier, RunOptions};
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
    path: PathBuf,
    prompt: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .without_time()
        .init();
    let cli = Cli::parse();
    let catalog = default_catalog();

    match cli.command.clone() {
        Some(CommandKind::Doctor) => {
            for provider in catalog.providers() {
                let health = provider.check_health();
                println!(
                    "{}\tinstalled={}\tbinary={}\tauth={}",
                    health.provider,
                    health.installed,
                    health.binary_path.unwrap_or_else(|| "-".to_string()),
                    health
                        .auth
                        .map(|auth| auth.detail)
                        .unwrap_or_else(|| "unknown".to_string())
                );
            }
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
            let path = args.path;
            let traj = load_trajectory(&path)?;
            let provider = catalog
                .get(&traj.agent)
                .ok_or_else(|| anyhow!("unknown provider in trajectory: {}", traj.agent))?;
            let mut session =
                daddy_core::Session::from_trajectory(provider, &path, cli.cwd.clone())?;
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
            mcp_servers: Vec::new(),
        })
        .build())
}

fn parse_provider_order(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect()
}
