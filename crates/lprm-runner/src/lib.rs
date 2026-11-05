//! Validation command and reviewer adapters for LPRM.

mod adapter;

pub use adapter::{AdapterLimits, run_json_adapter};

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use lprm_core::{ChecksConfig, LprmError};
use serde::Serialize;

const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
    TimedOut,
    MissingExecutable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckResult {
    pub status: CheckStatus,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NamedCheckResult {
    pub name: String,
    pub result: CheckResult,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckSuiteResult {
    pub passed: bool,
    pub checks: Vec<NamedCheckResult>,
}

#[derive(Clone, Debug)]
pub struct CheckRunner {
    max_output_bytes: usize,
}

#[derive(Debug)]
struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

impl CheckRunner {
    /// Creates a runner with a per-stream byte limit.
    ///
    /// # Errors
    ///
    /// Returns an invalid-argument error when the byte limit is zero.
    pub fn new(max_output_bytes: usize) -> Result<Self, LprmError> {
        if max_output_bytes == 0 {
            return Err(LprmError::invalid_argument(
                "Check log byte limit must be greater than zero",
            ));
        }
        Ok(Self { max_output_bytes })
    }

    /// Runs an argument-array command directly and captures bounded private logs.
    ///
    /// # Errors
    ///
    /// Returns an invalid-argument error for an empty command or timeout, and an internal error
    /// when process control or log persistence fails. A missing executable is a stable result.
    pub fn run(
        &self,
        name: &str,
        command: &[String],
        working_directory: &Path,
        log_directory: &Path,
        timeout: Duration,
    ) -> Result<CheckResult, LprmError> {
        let (program, arguments) = validate_request(command, working_directory, timeout)?;

        prepare_log_directory(log_directory)?;
        let safe_name = safe_log_name(name);
        let stdout_path = log_directory.join(format!("{safe_name}.stdout.log"));
        let stderr_path = log_directory.join(format!("{safe_name}.stderr.log"));
        let started = Instant::now();
        let mut process = Command::new(program);
        process
            .args(arguments)
            .current_dir(working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_process_group(&mut process);
        let mut child = match process.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                write_private_file(&stdout_path, &[])?;
                write_private_file(&stderr_path, &[])?;
                return Ok(CheckResult {
                    status: CheckStatus::MissingExecutable,
                    exit_code: None,
                    duration_ms: elapsed_millis(started),
                    stdout_path,
                    stderr_path,
                    stdout_truncated: false,
                    stderr_truncated: false,
                });
            }
            Err(error) => {
                return Err(LprmError::internal(format!(
                    "Unable to start configured check: {error}"
                )));
            }
        };

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LprmError::internal("Check stdout pipe is unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| LprmError::internal("Check stderr pipe is unavailable"))?;
        let stdout_limit = self.max_output_bytes;
        let stderr_limit = self.max_output_bytes;
        let stdout_reader = thread::spawn(move || read_bounded(stdout, stdout_limit));
        let stderr_reader = thread::spawn(move || read_bounded(stderr, stderr_limit));

        let mut timed_out = false;
        let exit_status = loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|error| LprmError::internal(format!("Unable to poll check: {error}")))?
            {
                break status;
            }
            if started.elapsed() >= timeout {
                timed_out = true;
                terminate_process_group(&mut child);
                break child.wait().map_err(|error| {
                    LprmError::internal(format!("Unable to reap timed out check: {error}"))
                })?;
            }
            thread::sleep(POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed())));
        };

        let stdout = join_reader(stdout_reader, "stdout")?;
        let stderr = join_reader(stderr_reader, "stderr")?;
        write_private_file(&stdout_path, &stdout.bytes)?;
        write_private_file(&stderr_path, &stderr.bytes)?;
        let status = if timed_out {
            CheckStatus::TimedOut
        } else if exit_status.success() {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        };

        Ok(CheckResult {
            status,
            exit_code: exit_status.code(),
            duration_ms: elapsed_millis(started),
            stdout_path,
            stderr_path,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
        })
    }

    /// Runs every configured required check in declared order against one working directory.
    ///
    /// # Errors
    ///
    /// Returns an invalid-argument error if a required check has no command, or forwards runner
    /// errors when a command cannot be controlled or its logs cannot be persisted.
    pub fn run_required(
        &self,
        checks: &ChecksConfig,
        working_directory: &Path,
        log_directory: &Path,
    ) -> Result<CheckSuiteResult, LprmError> {
        let timeout = Duration::from_secs(checks.timeout_seconds);
        let mut results = Vec::with_capacity(checks.required.len());
        for name in &checks.required {
            let command = checks.commands.get(name).ok_or_else(|| {
                LprmError::invalid_argument(format!(
                    "Required check {name:?} has no configured command"
                ))
            })?;
            results.push(NamedCheckResult {
                name: name.clone(),
                result: self.run(name, command, working_directory, log_directory, timeout)?,
            });
        }
        let passed = results
            .iter()
            .all(|check| check.result.status == CheckStatus::Passed);
        Ok(CheckSuiteResult {
            passed,
            checks: results,
        })
    }
}

fn validate_request<'a>(
    command: &'a [String],
    working_directory: &Path,
    timeout: Duration,
) -> Result<(&'a str, &'a [String]), LprmError> {
    let (program, arguments) = command
        .split_first()
        .ok_or_else(|| LprmError::invalid_argument("Check command must not be empty"))?;
    if program.is_empty() || arguments.iter().any(String::is_empty) {
        return Err(LprmError::invalid_argument(
            "Check command arguments must not be empty",
        ));
    }
    if timeout.is_zero() {
        return Err(LprmError::invalid_argument(
            "Check timeout must be greater than zero",
        ));
    }
    if !working_directory.is_dir() {
        return Err(LprmError::invalid_argument(
            "Check working directory must exist",
        ));
    }
    Ok((program, arguments))
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn terminate_process_group(child: &mut Child) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;

    // The child is still unreaped and was created as its own process-group leader, so its PID is
    // the process group owned by this runner. killpg is a safe wrapper around the Unix syscall.
    if let Ok(process_group) = i32::try_from(child.id()) {
        let _ = killpg(Pid::from_raw(process_group), Signal::SIGKILL);
    }
    let _ = child.kill();
}

#[cfg(not(unix))]
fn terminate_process_group(child: &mut Child) {
    let _ = child.kill();
}

fn read_bounded(mut reader: impl Read, limit: usize) -> io::Result<CapturedOutput> {
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        let retained = remaining.min(count);
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= retained < count;
    }
    Ok(CapturedOutput { bytes, truncated })
}

fn join_reader(
    reader: thread::JoinHandle<io::Result<CapturedOutput>>,
    stream: &str,
) -> Result<CapturedOutput, LprmError> {
    reader
        .join()
        .map_err(|_| LprmError::internal(format!("Check {stream} reader panicked")))?
        .map_err(|error| LprmError::internal(format!("Unable to read check {stream}: {error}")))
}

fn safe_log_name(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        "check".to_owned()
    } else {
        name
    }
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn prepare_log_directory(path: &Path) -> Result<(), LprmError> {
    fs::create_dir_all(path).map_err(|error| {
        LprmError::internal(format!("Unable to create check log directory: {error}"))
    })?;
    set_private_directory_permissions(path)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), LprmError> {
    let mut file = private_file(path)
        .map_err(|error| LprmError::internal(format!("Unable to create check log: {error}")))?;
    file.write_all(bytes)
        .map_err(|error| LprmError::internal(format!("Unable to write check log: {error}")))
}

#[cfg(unix)]
fn private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> io::Result<File> {
    File::create(path)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), LprmError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        LprmError::internal(format!("Unable to secure check log directory: {error}"))
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), LprmError> {
    Ok(())
}

/// Returns the core version used by this adapter.
pub const fn core_version() -> &'static str {
    lprm_core::VERSION
}
