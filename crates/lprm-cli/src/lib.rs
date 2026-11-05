use std::ffi::OsString;
use std::io::{self, Write};

use clap::{Args, Parser, Subcommand, ValueEnum, error::ErrorKind};
use lprm_core::{Envelope, LprmError};
use serde::Serialize;
use serde_json::Value;
use tracing::error;
use tracing_subscriber::EnvFilter;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Parser)]
#[command(name = "lprm", version, about = "Local Pull Request Manager")]
pub struct Cli {
    #[arg(long, global = true)]
    pub json: bool,

    #[arg(long, global = true, value_enum)]
    pub output: Option<OutputFormat>,

    #[arg(long, global = true)]
    pub yes: bool,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    pub fn machine_output(&self) -> bool {
        self.json || self.output == Some(OutputFormat::Json)
    }

    fn command_name(&self) -> &'static str {
        match self.command {
            Command::Init(_) => "init",
            Command::Doctor => "doctor",
            Command::Config(_) => "config",
            Command::Worktree(_) => "worktree",
            Command::Pr(_) => "pr",
            Command::Queue(_) => "queue",
            Command::Audit(_) => "audit",
            Command::Recover(_) => "recover",
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Init(InitArgs),
    Doctor,
    Config(ConfigArgs),
    Worktree(WorktreeArgs),
    Pr(PrArgs),
    Queue(QueueArgs),
    Audit(IdArgs),
    Recover(RecoverArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[arg(long, default_value = "dev")]
    pub target: String,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Show,
}

#[derive(Debug, Args)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    pub command: WorktreeCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorktreeCommand {
    Create(WorktreeCreateArgs),
    List,
    Status(IdArgs),
    Cleanup(WorktreeCleanupArgs),
}

#[derive(Debug, Args)]
pub struct WorktreeCreateArgs {
    #[arg(long)]
    pub task: String,

    #[arg(long)]
    pub agent: Option<String>,
}

#[derive(Debug, Args)]
pub struct WorktreeCleanupArgs {
    pub id: String,

    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct PrArgs {
    #[command(subcommand)]
    pub command: PrCommand,
}

#[derive(Debug, Subcommand)]
pub enum PrCommand {
    Create(PrCreateArgs),
    List(PrListArgs),
    Show(IdArgs),
    Diff(PrDiffArgs),
    Ready(IdArgs),
    Check(IdArgs),
    Review(IdArgs),
    Approve(ReasonArgs),
    Enqueue(IdArgs),
    Close(ReasonArgs),
}

#[derive(Debug, Args)]
pub struct PrCreateArgs {
    #[arg(long = "from")]
    pub source_branch: String,

    #[arg(long)]
    pub title: String,

    #[arg(long)]
    pub body_file: Option<String>,
}

#[derive(Debug, Args)]
pub struct PrListArgs {
    #[arg(long)]
    pub status: Option<String>,
}

#[derive(Debug, Args)]
pub struct PrDiffArgs {
    pub id: String,

    #[arg(long)]
    pub stat: bool,
}

#[derive(Debug, Args)]
pub struct ReasonArgs {
    pub id: String,

    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct QueueArgs {
    #[command(subcommand)]
    pub command: QueueCommand,
}

#[derive(Debug, Subcommand)]
pub enum QueueCommand {
    List,
    Run(QueueRunArgs),
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct QueueRunArgs {
    #[arg(long)]
    pub once: bool,

    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct IdArgs {
    pub id: String,
}

#[derive(Debug, Args)]
pub struct RecoverArgs {
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
struct InitReport {
    repository_id: String,
    target_branch: String,
    target_sha: String,
    state_directory: std::path::PathBuf,
    database_path: std::path::PathBuf,
}

pub fn run() -> i32 {
    run_from(std::env::args_os())
}

pub fn run_from<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let arguments: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let machine_requested = machine_output_requested(&arguments);

    let cli = match Cli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(parse_error) => {
            if matches!(
                parse_error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                let _ = parse_error.print();
                return 0;
            }

            let problem = LprmError::invalid_argument(parse_error.to_string());
            emit_error(&problem, machine_requested);
            return problem.exit_code();
        }
    };

    let machine_output = cli.machine_output();
    initialize_tracing(machine_output);
    match execute(&cli) {
        Ok(data) => {
            emit_success(&data, machine_output);
            0
        }
        Err(problem) => {
            emit_error(&problem, machine_output);
            problem.exit_code()
        }
    }
}

fn execute(cli: &Cli) -> Result<Value, LprmError> {
    match &cli.command {
        Command::Init(arguments) => initialize_repository(arguments),
        Command::Doctor => {
            let current_directory = std::env::current_dir()
                .map_err(|_| LprmError::internal("Current directory is unavailable"))?;
            let report = lprm_git::doctor(&current_directory)?;
            serde_json::to_value(report)
                .map_err(|_| LprmError::internal("Doctor report serialization failed"))
        }
        Command::Config(arguments) => match arguments.command {
            ConfigCommand::Show => {
                let current_directory = std::env::current_dir()
                    .map_err(|_| LprmError::internal("Current directory is unavailable"))?;
                let repository = lprm_git::Git::default().discover(&current_directory)?;
                let config = lprm_core::Config::load_for_repository(&repository.worktree_root)?;
                serde_json::to_value(config)
                    .map_err(|_| LprmError::internal("Config serialization failed"))
            }
        },
        _ => Err(LprmError::internal(format!(
            "{} is not implemented yet",
            cli.command_name()
        ))),
    }
}

fn initialize_repository(arguments: &InitArgs) -> Result<Value, LprmError> {
    let current_directory = std::env::current_dir()
        .map_err(|_| LprmError::internal("Current directory is unavailable"))?;
    let doctor = lprm_git::doctor(&current_directory)?;
    let mut config = lprm_core::Config::load_for_repository(&doctor.repository.worktree_root)?;
    config
        .repository
        .target_branch
        .clone_from(&arguments.target);
    config.validate()?;

    let git = lprm_git::Git::default();
    let target_sha = git.resolve_local_branch(
        &doctor.repository.worktree_root,
        &config.repository.target_branch,
    )?;
    let layout = lprm_store::StateLayout::for_repository(&doctor.repository.common_git_dir)?;
    layout.prepare()?;
    let store = lprm_store::Store::open(layout.database_path())?;
    store.register_repository(
        layout.repository_id(),
        &doctor.repository.common_git_dir,
        &config.repository.target_branch,
        &config.fingerprint()?,
    )?;

    serde_json::to_value(InitReport {
        repository_id: layout.repository_id().to_owned(),
        target_branch: config.repository.target_branch,
        target_sha,
        state_directory: layout.repository_dir().to_owned(),
        database_path: layout.database_path().to_owned(),
    })
    .map_err(|_| LprmError::internal("Init report serialization failed"))
}

fn machine_output_requested(args: &[OsString]) -> bool {
    args.iter().enumerate().any(|(index, argument)| {
        let value = argument.to_string_lossy();
        value == "--json"
            || value == "--output=json"
            || (value == "--output"
                && args
                    .get(index + 1)
                    .is_some_and(|next| next.to_string_lossy() == "json"))
    })
}

fn initialize_tracing(json: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_ansi(false);

    if json {
        let _ = subscriber.json().try_init();
    } else {
        let _ = subscriber.compact().try_init();
    }
}

fn emit_success<T: Serialize>(data: &T, json: bool) {
    let mut stdout = io::stdout().lock();
    if json {
        let envelope = Envelope::success(data);
        if serde_json::to_writer(&mut stdout, &envelope).is_ok() {
            let _ = writeln!(stdout);
        }
    } else if serde_json::to_writer_pretty(&mut stdout, data).is_ok() {
        let _ = writeln!(stdout);
    }
}

fn emit_error(problem: &LprmError, json: bool) {
    if json {
        let envelope = Envelope::<Value>::failure(problem);
        let mut stdout = io::stdout().lock();
        if serde_json::to_writer(&mut stdout, &envelope).is_ok() {
            let _ = writeln!(stdout);
        }
        error!(code = ?problem.code, message = %problem.message);
    } else {
        eprintln!("error: {problem}");
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use clap::{CommandFactory, Parser};

    use super::{Cli, Command, OutputFormat, machine_output_requested};

    #[test]
    fn parses_the_documented_nested_command_shape() {
        let cli = Cli::try_parse_from([
            "lprm",
            "pr",
            "create",
            "--from",
            "feature/a",
            "--title",
            "Add A",
            "--json",
        ])
        .expect("documented command should parse");

        assert!(cli.machine_output());
        assert!(matches!(cli.command, Command::Pr(_)));
    }

    #[test]
    fn output_json_alias_enables_machine_mode() {
        let cli = Cli::try_parse_from(["lprm", "--output", "json", "doctor"])
            .expect("output alias should parse");

        assert_eq!(cli.output, Some(OutputFormat::Json));
        assert!(cli.machine_output());
    }

    #[test]
    fn detects_machine_mode_before_clap_reports_invalid_arguments() {
        let args = [OsString::from("lprm"), OsString::from("--output=json")];

        assert!(machine_output_requested(&args));
    }

    #[test]
    fn clap_model_has_a_required_subcommand() {
        Cli::command().debug_assert();
    }
}
