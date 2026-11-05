//! Bounded JSON subprocess adapter used by external reviewers.

use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use lprm_core::LprmError;

use super::{
    POLL_INTERVAL, configure_process_group, join_reader, read_bounded, terminate_process_group,
    validate_request,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdapterLimits {
    pub timeout: Duration,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
}

/// Runs `command` without a shell, feeds `input` on stdin, and returns bounded stdout.
///
/// The child gets a scrubbed environment (no inherited secrets), no stderr channel, its own
/// process group, and is killed on timeout. Every failure is a validation error so callers can
/// fail closed.
///
/// # Errors
///
/// Returns a validation error for oversized input or output, spawn failure, a timeout, or a
/// non-zero exit; an invalid-argument error for an empty command.
pub fn run_json_adapter(
    command: &[String],
    working_directory: &Path,
    input: &[u8],
    limits: AdapterLimits,
) -> Result<Vec<u8>, LprmError> {
    let (program, arguments) = validate_request(command, working_directory, limits.timeout)?;
    if input.len() > limits.max_input_bytes {
        return Err(LprmError::validation_failed(
            "Reviewer request exceeds the payload limit",
        ));
    }
    let mut process = Command::new(program);
    process
        .args(arguments)
        .current_dir(working_directory)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    configure_process_group(&mut process);
    let mut child = process.spawn().map_err(|error| {
        LprmError::validation_failed(format!("Unable to start external reviewer: {error}"))
    })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| LprmError::internal("Reviewer stdin pipe is unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| LprmError::internal("Reviewer stdout pipe is unavailable"))?;
    let payload = input.to_vec();
    let writer = thread::spawn(move || match stdin.write_all(&payload) {
        Err(error) if error.kind() != io::ErrorKind::BrokenPipe => Err(error),
        _ => Ok(()),
    });
    let output_limit = limits.max_output_bytes;
    let reader = thread::spawn(move || read_bounded(stdout, output_limit));

    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| LprmError::internal(format!("Unable to poll reviewer: {error}")))?
        {
            break status;
        }
        if started.elapsed() >= limits.timeout {
            timed_out = true;
            terminate_process_group(&mut child);
            break child.wait().map_err(|error| {
                LprmError::internal(format!("Unable to reap timed out reviewer: {error}"))
            })?;
        }
        thread::sleep(POLL_INTERVAL.min(limits.timeout.saturating_sub(started.elapsed())));
    };
    let _ = writer.join();
    let output = join_reader(reader, "stdout")?;
    if timed_out {
        return Err(LprmError::validation_failed("External reviewer timed out"));
    }
    if !status.success() {
        return Err(LprmError::validation_failed(format!(
            "External reviewer exited with {status}"
        )));
    }
    if output.truncated {
        return Err(LprmError::validation_failed(
            "External reviewer output exceeds the payload limit",
        ));
    }
    Ok(output.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Vec<String> {
        ["/bin/sh", "-c", script].map(str::to_owned).to_vec()
    }

    fn limits() -> AdapterLimits {
        AdapterLimits {
            timeout: Duration::from_secs(5),
            max_input_bytes: 1024,
            max_output_bytes: 1024,
        }
    }

    fn run(script: &str, input: &[u8], limits: AdapterLimits) -> Result<Vec<u8>, LprmError> {
        run_json_adapter(&shell(script), Path::new("/"), input, limits)
    }

    #[test]
    fn echoes_stdin_to_stdout() {
        assert_eq!(
            run("cat", b"{\"ok\":true}", limits()).unwrap(),
            b"{\"ok\":true}"
        );
    }

    #[test]
    fn rejects_oversized_input_before_spawning() {
        let error = run("cat", &[b'x'; 2048], limits()).unwrap_err();
        assert!(error.message.contains("payload limit"));
    }

    #[test]
    fn rejects_oversized_output() {
        let error = run("head -c 4096 /dev/zero", b"", limits()).unwrap_err();
        assert!(error.message.contains("output exceeds"));
    }

    #[test]
    fn nonzero_exit_fails_closed() {
        let error = run("echo '{}'; exit 3", b"", limits()).unwrap_err();
        assert!(error.message.contains("exited"));
    }

    #[test]
    fn timeout_kills_the_process_tree() {
        let started = Instant::now();
        let error = run(
            "sleep 30 & wait",
            b"",
            AdapterLimits {
                timeout: Duration::from_millis(100),
                ..limits()
            },
        )
        .unwrap_err();
        assert!(error.message.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn environment_secrets_are_not_inherited() {
        // HOME is set in any test environment; the adapter must not see it.
        run("test -z \"$HOME\"", b"", limits()).expect("HOME must be scrubbed");
    }

    #[test]
    fn missing_executable_is_a_validation_failure() {
        let error = run_json_adapter(
            &["/nonexistent/reviewer".to_owned()],
            Path::new("/"),
            b"",
            limits(),
        )
        .unwrap_err();
        assert!(error.message.contains("Unable to start"));
    }
}
