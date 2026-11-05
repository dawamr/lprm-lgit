//! Opt-in, bounded agent events delivered to a configured subprocess.
//!
//! Delivery is strictly best effort and one-way: the callback's output is discarded, failures
//! are logged, and nothing here can alter PR state, queue order, or any admission gate.

use std::time::Duration;

use lprm_core::{AgentIdentity, Config, LprmError};
use lprm_runner::{AdapterLimits, run_json_adapter};
use serde_json::{Value, json};
use tracing::warn;

use super::{Command, PrCommand, QueueCommand};

const EVENT_SCHEMA_VERSION: u32 = 1;

struct Event {
    name: &'static str,
    pr_id: Option<String>,
    status: Option<String>,
    detail: Option<String>,
}

fn event(name: &'static str, pr_id: Option<&str>, data: &Value) -> Event {
    Event {
        name,
        pr_id: pr_id.map(str::to_owned),
        status: data
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned),
        detail: None,
    }
}

fn pr_id_of(data: &Value) -> Option<&str> {
    data.get("pr_id").and_then(Value::as_str)
}

/// Maps a successful command to the events agents may subscribe to.
fn success_events(command: &Command, data: &Value) -> Vec<Event> {
    match command {
        Command::Pr(arguments) => {
            let name = match &arguments.command {
                PrCommand::Create(_) => "pr.created",
                PrCommand::Ready(_) => "pr.ready",
                PrCommand::Sync(_) => "pr.revised",
                PrCommand::Review(_) => "pr.reviewed",
                PrCommand::Approve(_) => "pr.approved",
                PrCommand::Enqueue(_) => "pr.queued",
                PrCommand::Close(_) => "pr.closed",
                PrCommand::Check(_) => "pr.checked",
                PrCommand::List(_) | PrCommand::Show(_) | PrCommand::Diff(_) => return Vec::new(),
            };
            let id = pr_id_of(data).or_else(|| data.get("pull_request").and_then(pr_id_of));
            let mut events = vec![event(name, id, data.get("pull_request").unwrap_or(data))];
            if name == "pr.checked" && data["auto_enqueue"]["queued"].as_bool() == Some(true) {
                events.push(event("pr.queued", id, &Value::Null));
            }
            events
        }
        Command::Queue(arguments) => match &arguments.command {
            QueueCommand::Run(_) => queue_run_events(data),
            QueueCommand::List => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn queue_run_events(data: &Value) -> Vec<Event> {
    let mut events = Vec::new();
    for attempt in data["attempts"].as_array().into_iter().flatten() {
        let record = &attempt["attempt"];
        if record["state"] == "succeeded" {
            events.push(event("pr.merged", record["pr_id"].as_str(), &Value::Null));
        }
    }
    for id in data["invalidated"].as_array().into_iter().flatten() {
        events.push(event("pr.invalidated", id.as_str(), &Value::Null));
    }
    events
}

fn failure_events(command: &Command, error: &LprmError) -> Vec<Event> {
    let Command::Pr(arguments) = command else {
        return Vec::new();
    };
    let (name, id) = match &arguments.command {
        PrCommand::Check(check) => ("pr.check_failed", &check.id),
        _ => return Vec::new(),
    };
    vec![Event {
        name,
        pr_id: Some(id.clone()),
        status: None,
        detail: Some(error.message.clone()),
    }]
}

fn deliver(config: &Config, root: &std::path::Path, actor: &AgentIdentity, event: &Event) {
    let payload = json!({
        "schema_version": EVENT_SCHEMA_VERSION,
        "event": event.name,
        "actor": actor.as_str(),
        "pr_id": event.pr_id,
        "status": event.status,
        "detail": event.detail,
    });
    let Ok(bytes) = serde_json::to_vec(&payload) else {
        return;
    };
    let limits = AdapterLimits {
        timeout: Duration::from_secs(config.events.timeout_seconds),
        max_input_bytes: config.events.max_payload_bytes,
        max_output_bytes: config.events.max_payload_bytes,
    };
    if let Err(problem) = run_json_adapter(&config.events.command, root, &bytes, limits) {
        warn!(
            event = event.name,
            "event delivery failed: {}", problem.message
        );
    }
}

/// Delivers events for a finished command. Never fails and never alters the command's result.
pub fn notify(command: &Command, actor: &AgentIdentity, outcome: Result<&Value, &LprmError>) {
    let Ok(directory) = std::env::current_dir() else {
        return;
    };
    let Ok(repository) = lprm_git::Git::default().discover(&directory) else {
        return;
    };
    let Ok(config) = Config::load_for_repository(&repository.worktree_root) else {
        return;
    };
    if config.events.command.is_empty() {
        return;
    }
    let events = match outcome {
        Ok(data) => success_events(command, data),
        Err(error) => failure_events(command, error),
    };
    for event in &events {
        deliver(&config, &repository.worktree_root, actor, event);
    }
}
