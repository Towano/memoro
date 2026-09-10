use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use clap::{Args, Parser, Subcommand};
use memoro::{
    config::{self, SpaceSettings},
    git::GitRepository,
    server::{self, ServeOverrides, Transport},
    service::Service,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_REMOTE: &str = "origin";
const DEFAULT_BRANCH: &str = "main";
const DEFAULT_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Parser)]
#[command(name = "memoro", about = "Git-backed MCP memory server", version = VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run Memoro as an MCP server.
    Serve(ServeArguments),
    /// Configure a Git synchronization remote.
    Sync(SyncArguments),
    /// Manage memory spaces.
    Spaces(SpacesArguments),
}

#[derive(Debug, Args)]
struct ServeArguments {
    /// MCP transport: stdio or http.
    #[arg(long)]
    transport: Option<String>,

    /// Memoro data directory.
    #[arg(long)]
    home: Option<String>,

    /// HTTP listen host.
    #[arg(long)]
    host: Option<String>,

    /// HTTP listen port.
    #[arg(long)]
    port: Option<String>,

    /// Optional HTTP Bearer token.
    #[arg(long)]
    token: Option<String>,
}

#[derive(Debug, Args)]
struct SyncArguments {
    #[command(subcommand)]
    command: SyncCommand,
}

#[derive(Debug, Subcommand)]
enum SyncCommand {
    /// Configure the synchronization remote for a memory space.
    Setup(SyncSetupArguments),
}

#[derive(Debug, Args)]
struct SyncSetupArguments {
    /// Git repository URL or local repository path.
    repository_url: String,

    /// Memory space to configure.
    #[arg(long, default_value = config::DEFAULT_SPACE)]
    space: String,

    /// Remote branch to synchronize.
    #[arg(long, default_value = DEFAULT_BRANCH)]
    branch: String,

    /// Network operation timeout in seconds.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout: u64,

    /// Private deploy key path. Must be paired with --known-hosts.
    #[arg(long)]
    deploy_key: Option<PathBuf>,

    /// SSH known-hosts path. Must be paired with --deploy-key.
    #[arg(long)]
    known_hosts: Option<PathBuf>,

    /// Replace an existing origin remote URL.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct SpacesArguments {
    #[command(subcommand)]
    command: SpacesCommand,
}

#[derive(Debug, Subcommand)]
enum SpacesCommand {
    /// Add a memory space.
    Add(SpaceAddArguments),
    /// Remove a memory space.
    Remove(SpaceNameArguments),
    /// List registered memory spaces.
    List,
}

#[derive(Debug, Args)]
struct SpaceAddArguments {
    name: String,
    #[arg(long)]
    readonly: bool,
}

#[derive(Debug, Args)]
struct SpaceNameArguments {
    name: String,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return exit_with_clap_error(error),
    };

    match cli.command {
        Command::Serve(arguments) => run_serve(arguments).await,
        Command::Sync(arguments) => match arguments.command {
            SyncCommand::Setup(arguments) => run_sync_setup(arguments),
        },
        Command::Spaces(arguments) => run_spaces(arguments.command),
    }
}

async fn run_serve(arguments: ServeArguments) -> ExitCode {
    let overrides = ServeOverrides {
        transport: arguments.transport,
        home: arguments.home,
        host: arguments.host,
        port: arguments.port,
        token: arguments.token,
    };
    let settings = match server::resolve_serve_config(&overrides) {
        Ok(settings) => settings,
        Err(error) => return exit_with_error(&error),
    };
    let service = match Service::open(settings.home) {
        Ok(service) => Arc::new(service),
        Err(error) => return exit_with_error(&error.to_string()),
    };

    let result = match settings.transport {
        Transport::Stdio => server::run_stdio(server::MemoroServer::new(service)).await,
        Transport::Http => {
            server::run_http(service, &settings.host, settings.port, settings.token).await
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => exit_with_error(&error),
    }
}

fn run_spaces(command: SpacesCommand) -> ExitCode {
    let home = match config::resolve_home(None, None) {
        Ok(home) => home,
        Err(error) => return exit_with_error(&error.to_string()),
    };
    run_spaces_at(&home, command)
}

fn run_spaces_at(home: &Path, command: SpacesCommand) -> ExitCode {
    let mut spaces = match config::load_spaces(home) {
        Ok(spaces) => spaces,
        Err(error) => return exit_with_error(&error.to_string()),
    };
    match command {
        SpacesCommand::Add(arguments) => {
            let name = match config::normalize_space_name(&arguments.name) {
                Ok(name) => name,
                Err(error) => return exit_with_error(&error.to_string()),
            };
            if spaces
                .insert(
                    name.clone(),
                    SpaceSettings {
                        readonly: arguments.readonly,
                    },
                )
                .is_some()
            {
                return exit_with_error(&format!("Space '{name}' is already registered."));
            }
            match config::save_spaces(home, &spaces) {
                Ok(_) => print_json(&serde_json::json!({
                    "space": name,
                    "readonly": arguments.readonly,
                    "status": "added"
                })),
                Err(error) => exit_with_error(&error.to_string()),
            }
        }
        SpacesCommand::Remove(arguments) => {
            let name = match config::normalize_space_name(&arguments.name) {
                Ok(name) => name,
                Err(error) => return exit_with_error(&error.to_string()),
            };
            if name == config::DEFAULT_SPACE {
                return exit_with_error("The default space 'personal' cannot be removed.");
            }
            if spaces.remove(&name).is_none() {
                return exit_with_error(&format!("Space '{name}' is not registered."));
            }
            match config::save_spaces(home, &spaces) {
                Ok(_) => print_json(&serde_json::json!({
                    "space": name,
                    "status": "removed"
                })),
                Err(error) => exit_with_error(&error.to_string()),
            }
        }
        SpacesCommand::List => {
            let entries: Vec<_> = spaces
                .iter()
                .map(|(name, settings)| {
                    serde_json::json!({
                        "space": name,
                        "readonly": settings.readonly,
                        "path": config::RuntimePaths::new(home.to_path_buf()).space(name).ok(),
                    })
                })
                .collect();
            print_json(&serde_json::json!({ "spaces": entries }))
        }
    }
}

fn run_sync_setup(arguments: SyncSetupArguments) -> ExitCode {
    let home = match config::resolve_home(None, None) {
        Ok(home) => home,
        Err(error) => return exit_with_error(&error.to_string()),
    };
    run_sync_setup_at(&home, arguments)
}

fn run_sync_setup_at(home: &Path, arguments: SyncSetupArguments) -> ExitCode {
    let space = match config::normalize_space_name(&arguments.space) {
        Ok(space) => space,
        Err(error) => return exit_with_error(&error.to_string()),
    };
    if arguments.timeout == 0 {
        return exit_with_error("--timeout must be greater than zero seconds.");
    }
    if arguments.deploy_key.is_some() != arguments.known_hosts.is_some() {
        return exit_with_error("--deploy-key and --known-hosts must be provided together.");
    }
    if let (Some(deploy_key), Some(known_hosts)) = (&arguments.deploy_key, &arguments.known_hosts) {
        if !deploy_key.is_file() {
            return exit_with_error(&format!(
                "Deploy key path {} does not exist or is not a regular file.",
                deploy_key.display()
            ));
        }
        if !known_hosts.is_file() {
            return exit_with_error(&format!(
                "Known-hosts path {} does not exist or is not a regular file.",
                known_hosts.display()
            ));
        }
        if deploy_key == known_hosts {
            return exit_with_error(
                "--deploy-key and --known-hosts must refer to different files.",
            );
        }
    }
    if arguments.branch.is_empty()
        || arguments.branch.starts_with('-')
        || arguments.branch.ends_with('.')
        || arguments.branch.contains("..")
        || arguments.branch.contains(' ')
        || arguments.branch.contains('~')
        || arguments.branch.contains('^')
        || arguments.branch.contains(':')
        || arguments.branch.contains('?')
        || arguments.branch.contains('*')
        || arguments.branch.contains('[')
        || arguments.branch.contains('\\')
    {
        return exit_with_error("--branch is not a valid Git branch name.");
    }

    let spaces = match config::load_spaces(home) {
        Ok(spaces) => spaces,
        Err(error) => return exit_with_error(&error.to_string()),
    };
    if !spaces.contains_key(&space) {
        return exit_with_error(&format!(
            "Space '{space}' is not registered. Run 'memoro spaces add {space}' first."
        ));
    }
    let repository_path = match config::RuntimePaths::new(home.to_path_buf()).space(&space) {
        Ok(path) => path,
        Err(error) => return exit_with_error(&error.to_string()),
    };
    let repository = match GitRepository::initialize(&repository_path) {
        Ok(repository) => repository,
        Err(error) => return exit_with_error(&error.to_string()),
    };
    let remotes = match repository.remote_names() {
        Ok(remotes) => remotes,
        Err(error) => return exit_with_error(&error.to_string()),
    };
    if remotes.contains(DEFAULT_REMOTE) && !arguments.force {
        return exit_with_error(
            "Remote 'origin' is already configured. Use --force to replace its URL.",
        );
    }
    if let Some(deploy_key) = &arguments.deploy_key {
        if let Some(known_hosts) = &arguments.known_hosts {
            if let Err(error) = repository.configure_deploy_key(deploy_key, known_hosts) {
                return exit_with_error(&error.to_string());
            }
        }
    }
    let heads = match repository.remote_heads(&arguments.repository_url, arguments.timeout) {
        Ok(heads) => heads,
        Err(error) => return exit_with_error(&error.to_string()),
    };
    if !heads.is_empty() && !heads.contains_key(&arguments.branch) {
        return exit_with_error(&format!(
            "Remote does not contain branch '{}'. Choose an existing branch with --branch.",
            arguments.branch
        ));
    }
    if let Err(error) = repository.set_remote_url(DEFAULT_REMOTE, &arguments.repository_url) {
        return exit_with_error(&error.to_string());
    }
    print_json(&serde_json::json!({
        "space": space,
        "remote": DEFAULT_REMOTE,
        "branch": arguments.branch,
        "repository": arguments.repository_url,
        "status": "configured"
    }))
}

fn print_json(value: &serde_json::Value) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => exit_with_error(&format!("Could not format command result: {error}")),
    }
}

fn exit_with_clap_error(error: clap::Error) -> ExitCode {
    let _ = error.print();
    ExitCode::from(error.exit_code() as u8)
}

fn exit_with_error(error: &str) -> ExitCode {
    eprintln!("{error}");
    ExitCode::FAILURE
}
