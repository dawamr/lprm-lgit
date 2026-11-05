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

#[derive(Debug, Serialize)]
struct WorktreeCreateReport {
    id: String,
    branch: String,
    path: std::path::PathBuf,
    target_sha: String,
    owner_agent: Option<String>,
}

#[derive(Debug, Serialize)]
struct WorktreeStatusReport {
    id: String,
    branch: String,
    path: std::path::PathBuf,
    owner_agent: Option<String>,
    created_at: String,
    present: bool,
    actual_branch: Option<String>,
    head_sha: Option<String>,
    dirty: Option<bool>,
}

#[derive(Debug, Serialize)]
struct WorktreeCleanupReport {
    id: String,
    branch: String,
    path: std::path::PathBuf,
    dry_run: bool,
    eligible: bool,
    removed: bool,
}

#[derive(Debug, Serialize)]
struct WorktreeOrphanReport {
    kind: &'static str,
    worktree_id: Option<String>,
    path: std::path::PathBuf,
    branch: Option<String>,
    suggested_action: &'static str,
}

#[derive(Debug, Serialize)]
struct RecoveryReport {
    dry_run: bool,
    worktree_orphans: Vec<WorktreeOrphanReport>,
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
        Command::Worktree(arguments) => match &arguments.command {
            WorktreeCommand::Create(create) => create_worktree(create),
            WorktreeCommand::List => list_worktrees(),
            WorktreeCommand::Status(status) => show_worktree_status(&status.id),
            WorktreeCommand::Cleanup(cleanup) => cleanup_worktree(cleanup, cli.yes),
        },
        Command::Pr(arguments) => match &arguments.command {
            PrCommand::Create(create) => create_pull_request(create),
            PrCommand::List(list) => list_pull_requests(list),
            PrCommand::Show(show) => show_pull_request(&show.id),
            PrCommand::Diff(diff) => diff_pull_request(diff),
            PrCommand::Close(close) => close_pull_request(close),
            _ => Err(LprmError::internal("PR operation is not implemented yet")),
        },
        Command::Recover(_) => recover_worktree_orphans(),
        _ => Err(LprmError::internal(format!(
            "{} is not implemented yet",
            cli.command_name()
        ))),
    }
}

fn create_pull_request(arguments: &PrCreateArgs) -> Result<Value, LprmError> {
    if arguments.title.trim().is_empty() {
        return Err(LprmError::invalid_argument(
            "Pull request title must not be empty",
        ));
    }
    let current_directory = std::env::current_dir()
        .map_err(|_| LprmError::internal("Current directory is unavailable"))?;
    let git = lprm_git::Git::default();
    let repository = git.discover(&current_directory)?;
    let config = lprm_core::Config::load_for_repository(&repository.worktree_root)?;
    let (mut store, layout) = open_current_store()?;
    let target_branch = store
        .repository_target(layout.repository_id())?
        .ok_or_else(|| LprmError::git_precondition("Repository initialization is missing"))?;
    let source_sha =
        git.resolve_local_branch(&repository.worktree_root, &arguments.source_branch)?;
    let source_worktree = store
        .list_worktrees(layout.repository_id())?
        .into_iter()
        .find(|record| record.branch == arguments.source_branch)
        .ok_or_else(|| {
            LprmError::git_precondition(
                "Pull request source must be an active LPRM-managed worktree",
            )
        })?;
    let observation = git.observe_worktree(&source_worktree.absolute_path)?;
    if !observation.present
        || observation.actual_branch.as_deref() != Some(arguments.source_branch.as_str())
    {
        return Err(LprmError::git_precondition(
            "Pull request source worktree ownership does not match Git state",
        ));
    }
    if observation.dirty != Some(false) {
        return Err(LprmError::validation_failed(
            "Commit or discard source worktree changes before creating a pull request",
        ));
    }
    if observation.head_sha.as_deref() != Some(source_sha.as_str()) {
        return Err(LprmError::git_precondition(
            "Pull request source branch and worktree HEAD do not match",
        ));
    }
    let target_sha = git.resolve_local_branch(&repository.worktree_root, &target_branch)?;
    let diff = git.collect_diff(&repository.worktree_root, &target_sha, &source_sha)?;
    if diff.commits.is_empty() || diff.files.is_empty() {
        return Err(LprmError::validation_failed(
            "Pull request source has no committed content changes relative to the target",
        ));
    }
    let body = arguments.body_file.as_deref().map_or_else(
        || Ok(String::new()),
        |path| {
            std::fs::read_to_string(path).map_err(|error| {
                LprmError::invalid_argument(format!(
                    "Unable to read pull request body file {path:?}: {error}"
                ))
            })
        },
    )?;
    let fingerprint = config.fingerprint()?;
    let request = lprm_store::NewPullRequest {
        id: format!("pr_{}", uuid::Uuid::now_v7().simple()),
        repository_id: layout.repository_id().to_owned(),
        worktree_id: Some(source_worktree.id),
        title: arguments.title.clone(),
        body,
        source_branch: arguments.source_branch.clone(),
        target_branch,
        revision_id: format!("revision_{}", uuid::Uuid::now_v7().simple()),
        source_sha,
        target_sha,
        policy_hash: fingerprint.clone(),
        check_config_hash: fingerprint,
    };
    let record = store.create_pull_request(&request)?;
    serde_json::to_value(record)
        .map_err(|_| LprmError::internal("Pull request serialization failed"))
}

fn list_pull_requests(arguments: &PrListArgs) -> Result<Value, LprmError> {
    let (store, layout) = open_current_store()?;
    let status = arguments
        .status
        .as_deref()
        .map(parse_pull_request_status)
        .transpose()?;
    let records = store.list_pull_requests(layout.repository_id(), status)?;
    serde_json::to_value(records)
        .map_err(|_| LprmError::internal("Pull request list serialization failed"))
}

fn show_pull_request(pull_request_id: &str) -> Result<Value, LprmError> {
    let (store, layout) = open_current_store()?;
    let record = store
        .pull_request(layout.repository_id(), pull_request_id)?
        .ok_or_else(|| {
            LprmError::invalid_argument(format!("Unknown pull request ID {pull_request_id:?}"))
        })?;
    serde_json::to_value(record)
        .map_err(|_| LprmError::internal("Pull request serialization failed"))
}

fn diff_pull_request(arguments: &PrDiffArgs) -> Result<Value, LprmError> {
    let current_directory = std::env::current_dir()
        .map_err(|_| LprmError::internal("Current directory is unavailable"))?;
    let git = lprm_git::Git::default();
    let repository = git.discover(&current_directory)?;
    let (store, layout) = open_current_store()?;
    let record = store
        .pull_request(layout.repository_id(), &arguments.id)?
        .ok_or_else(|| {
            LprmError::invalid_argument(format!("Unknown pull request ID {:?}", arguments.id))
        })?;
    let revision = record
        .revisions
        .last()
        .ok_or_else(|| LprmError::internal("Pull request has no revision history"))?;
    let diff = git.collect_diff(
        &repository.worktree_root,
        &revision.target_sha,
        &revision.source_sha,
    )?;
    let mut value = serde_json::to_value(diff)
        .map_err(|_| LprmError::internal("Pull request diff serialization failed"))?;
    if arguments.stat
        && let Some(object) = value.as_object_mut()
    {
        object.remove("patch");
    }
    Ok(value)
}

fn close_pull_request(arguments: &ReasonArgs) -> Result<Value, LprmError> {
    let (mut store, layout) = open_current_store()?;
    let record = store.close_pull_request(layout.repository_id(), &arguments.id)?;
    serde_json::to_value(record)
        .map_err(|_| LprmError::internal("Pull request serialization failed"))
}

fn parse_pull_request_status(value: &str) -> Result<lprm_core::PullRequestStatus, LprmError> {
    lprm_core::PullRequestStatus::ALL
        .into_iter()
        .find(|status| status.as_str() == value)
        .ok_or_else(|| {
            LprmError::invalid_argument(format!("Unknown pull request status {value:?}"))
        })
}

fn recover_worktree_orphans() -> Result<Value, LprmError> {
    let current_directory = std::env::current_dir()
        .map_err(|_| LprmError::internal("Current directory is unavailable"))?;
    let repository = lprm_git::Git::default().discover(&current_directory)?;
    let config = lprm_core::Config::load_for_repository(&repository.worktree_root)?;
    let (store, layout) = open_current_store()?;
    let managed = store.list_worktrees(layout.repository_id())?;
    let git_worktrees = lprm_git::Git::default().list_worktrees(&repository.worktree_root)?;
    let mut orphans = Vec::new();

    for record in &managed {
        if !git_worktrees
            .iter()
            .any(|git| git.path == record.absolute_path)
        {
            orphans.push(WorktreeOrphanReport {
                kind: "database_only",
                worktree_id: Some(record.id.clone()),
                path: record.absolute_path.clone(),
                branch: Some(record.branch.clone()),
                suggested_action: "inspect_then_mark_cleaned",
            });
        }
    }

    let configured_root = if config.repository.worktree_root.is_absolute() {
        config.repository.worktree_root
    } else {
        repository
            .worktree_root
            .join(config.repository.worktree_root)
    };
    let canonical_root = std::fs::canonicalize(&configured_root).ok();
    for git_worktree in git_worktrees {
        let is_managed_path = canonical_root.as_ref().is_some_and(|root| {
            std::fs::canonicalize(&git_worktree.path)
                .ok()
                .and_then(|path| path.parent().map(|parent| parent == root))
                .unwrap_or(false)
        });
        let is_registered = managed
            .iter()
            .any(|record| record.absolute_path == git_worktree.path);
        if is_managed_path && !is_registered {
            orphans.push(WorktreeOrphanReport {
                kind: "git_only",
                worktree_id: None,
                path: git_worktree.path,
                branch: git_worktree.branch,
                suggested_action: "inspect_then_register_or_remove",
            });
        }
    }
    orphans.sort_by(|left, right| left.path.cmp(&right.path));

    serde_json::to_value(RecoveryReport {
        dry_run: true,
        worktree_orphans: orphans,
    })
    .map_err(|_| LprmError::internal("Recovery report serialization failed"))
}

fn cleanup_worktree(arguments: &WorktreeCleanupArgs, confirmed: bool) -> Result<Value, LprmError> {
    let current_directory = std::env::current_dir()
        .map_err(|_| LprmError::internal("Current directory is unavailable"))?;
    let repository = lprm_git::Git::default().discover(&current_directory)?;
    let config = lprm_core::Config::load_for_repository(&repository.worktree_root)?;
    let (store, layout) = open_current_store()?;
    let record = store
        .worktree(layout.repository_id(), &arguments.id)?
        .ok_or_else(|| {
            LprmError::invalid_argument(format!("Unknown worktree ID {:?}", arguments.id))
        })?;

    let configured_root = if config.repository.worktree_root.is_absolute() {
        config.repository.worktree_root
    } else {
        repository
            .worktree_root
            .join(config.repository.worktree_root)
    };
    let canonical_path =
        lprm_core::validate_managed_worktree_path(&configured_root, &record.absolute_path)?;
    let git = lprm_git::Git::default();
    let observation = git.observe_worktree(&canonical_path)?;
    if !observation.present || observation.actual_branch.as_deref() != Some(record.branch.as_str())
    {
        return Err(LprmError::git_precondition(
            "Registered worktree ownership does not match Git state",
        ));
    }
    if observation.dirty != Some(false) {
        return Err(LprmError::validation_failed(
            "Refusing to clean up a dirty worktree",
        ));
    }
    let target_branch = store
        .repository_target(layout.repository_id())?
        .ok_or_else(|| LprmError::git_precondition("Repository initialization is missing"))?;
    let target_sha = git.resolve_local_branch(&repository.worktree_root, &target_branch)?;
    if git.has_unique_commits(&repository.worktree_root, &target_sha, &record.branch)? {
        return Err(LprmError::validation_failed(
            "Refusing to clean up a worktree with commits not reachable from the target",
        ));
    }

    let dry_run = arguments.dry_run || !confirmed;
    if !dry_run {
        git.remove_linked_worktree(&repository.worktree_root, &canonical_path)?;
        store.mark_worktree_cleaned(layout.repository_id(), &record.id)?;
    }
    serde_json::to_value(WorktreeCleanupReport {
        id: record.id,
        branch: record.branch,
        path: canonical_path,
        dry_run,
        eligible: true,
        removed: !dry_run,
    })
    .map_err(|_| LprmError::internal("Worktree cleanup report serialization failed"))
}

fn list_worktrees() -> Result<Value, LprmError> {
    let (store, layout) = open_current_store()?;
    let git = lprm_git::Git::default();
    let reports = store
        .list_worktrees(layout.repository_id())?
        .into_iter()
        .map(|record| observe_worktree(&git, record))
        .collect::<Result<Vec<_>, _>>()?;
    serde_json::to_value(reports)
        .map_err(|_| LprmError::internal("Worktree list serialization failed"))
}

fn show_worktree_status(worktree_id: &str) -> Result<Value, LprmError> {
    let (store, layout) = open_current_store()?;
    let record = store
        .worktree(layout.repository_id(), worktree_id)?
        .ok_or_else(|| {
            LprmError::invalid_argument(format!("Unknown worktree ID {worktree_id:?}"))
        })?;
    let report = observe_worktree(&lprm_git::Git::default(), record)?;
    serde_json::to_value(report)
        .map_err(|_| LprmError::internal("Worktree status serialization failed"))
}

fn open_current_store() -> Result<(lprm_store::Store, lprm_store::StateLayout), LprmError> {
    let current_directory = std::env::current_dir()
        .map_err(|_| LprmError::internal("Current directory is unavailable"))?;
    let repository = lprm_git::Git::default().discover(&current_directory)?;
    let layout = lprm_store::StateLayout::for_repository(&repository.common_git_dir)?;
    if !layout.database_path().is_file() {
        return Err(LprmError::git_precondition(
            "Repository is not initialized; run lprm init first",
        ));
    }
    let store = lprm_store::Store::open(layout.database_path())?;
    if store.repository_target(layout.repository_id())?.is_none() {
        return Err(LprmError::git_precondition(
            "Repository is not initialized; run lprm init first",
        ));
    }
    Ok((store, layout))
}

fn observe_worktree(
    git: &lprm_git::Git,
    record: lprm_store::WorktreeRecord,
) -> Result<WorktreeStatusReport, LprmError> {
    let observation = git.observe_worktree(&record.absolute_path)?;
    Ok(WorktreeStatusReport {
        id: record.id,
        branch: record.branch,
        path: record.absolute_path,
        owner_agent: record.owner_agent,
        created_at: record.created_at,
        present: observation.present,
        actual_branch: observation.actual_branch,
        head_sha: observation.head_sha,
        dirty: observation.dirty,
    })
}

fn create_worktree(arguments: &WorktreeCreateArgs) -> Result<Value, LprmError> {
    let current_directory = std::env::current_dir()
        .map_err(|_| LprmError::internal("Current directory is unavailable"))?;
    let repository = lprm_git::Git::default().discover(&current_directory)?;
    let config = lprm_core::Config::load_for_repository(&repository.worktree_root)?;
    let layout = lprm_store::StateLayout::for_repository(&repository.common_git_dir)?;
    if !layout.database_path().is_file() {
        return Err(LprmError::git_precondition(
            "Repository is not initialized; run lprm init first",
        ));
    }
    let store = lprm_store::Store::open(layout.database_path())?;
    let target_branch = store
        .repository_target(layout.repository_id())?
        .ok_or_else(|| {
            LprmError::git_precondition("Repository is not initialized; run lprm init first")
        })?;
    let git = lprm_git::Git::default();
    let target_sha = git.resolve_local_branch(&repository.worktree_root, &target_branch)?;
    let spec = lprm_core::WorktreeSpec::new(&arguments.task)?;

    let configured_root = if config.repository.worktree_root.is_absolute() {
        config.repository.worktree_root
    } else {
        repository
            .worktree_root
            .join(config.repository.worktree_root)
    };
    std::fs::create_dir_all(&configured_root)
        .map_err(|error| LprmError::internal(format!("Unable to create worktree root: {error}")))?;
    let worktree_root = std::fs::canonicalize(&configured_root).map_err(|error| {
        LprmError::internal(format!("Unable to resolve worktree root: {error}"))
    })?;
    let worktree_path = worktree_root.join(spec.directory_name());

    git.create_linked_worktree(
        &repository.worktree_root,
        spec.branch(),
        &worktree_path,
        &target_sha,
    )?;
    store.register_worktree(
        spec.id(),
        layout.repository_id(),
        spec.branch(),
        &worktree_path,
        arguments.agent.as_deref(),
    )?;

    serde_json::to_value(WorktreeCreateReport {
        id: spec.id().to_owned(),
        branch: spec.branch().to_owned(),
        path: worktree_path,
        target_sha,
        owner_agent: arguments.agent.clone(),
    })
    .map_err(|_| LprmError::internal("Worktree report serialization failed"))
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
