//! Opt-in agent events: bounded delivery that can never corrupt state or bypass gates.

mod common;

use std::fs;
use std::path::Path;

use common::{Fixture, assert_success};
use serde_json::Value;
use tempfile::tempdir;

fn events_config(script: &str, timeout: u32) -> String {
    format!(
        "[events]\ntimeout_seconds = {timeout}\ncommand = [\"/bin/sh\", \"-c\", '''{script}''']\n"
    )
}

fn logged(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("each event is one JSON line"))
        .collect()
}

#[test]
fn events_are_off_unless_configured() {
    let fixture = Fixture::new("/usr/bin/true");
    let pr = fixture.submit("src/a.txt");
    assert_success(&fixture.lprm(&["--json", "pr", "ready", &pr.pr_id]));
    // Nothing to read: the default config has no command, so no process was ever started.
    let shown = fixture.json_success(&["--json", "pr", "show", &pr.pr_id]);
    assert_eq!(shown["data"]["status"], "ready");
}

#[test]
fn lifecycle_events_are_versioned_and_ordered() {
    let log_dir = tempdir().unwrap();
    let log = log_dir.path().join("events.log");
    let script = format!("cat >> {}; echo >> {0}", log.display());
    let fixture = Fixture::with_options("[\"/usr/bin/true\"]", &events_config(&script, 5));
    let pr = fixture.submit("src/a.txt");
    let id = &pr.pr_id;

    for arguments in [
        vec!["--json", "--actor", "v", "pr", "ready", id],
        vec!["--json", "--actor", "v", "pr", "check", id],
        vec!["--json", "--actor", "c", "pr", "enqueue", id],
        vec!["--json", "--actor", "m", "queue", "run", "--once"],
    ] {
        assert_success(&fixture.lprm(&arguments));
    }

    let events: Vec<Value> = logged(&log)
        .into_iter()
        .filter(|event| !event.is_null())
        .collect();
    let names: Vec<&str> = events
        .iter()
        .map(|event| event["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "pr.created",
            "pr.ready",
            "pr.checked",
            "pr.queued",
            "pr.merged"
        ]
    );
    for event in &events {
        assert_eq!(event["schema_version"], 1);
        assert_eq!(event["pr_id"], id.as_str());
    }
    assert_eq!(events[4]["actor"], "m");
}

#[test]
fn failing_check_emits_a_failure_event() {
    let log_dir = tempdir().unwrap();
    let log = log_dir.path().join("events.log");
    let script = format!("cat >> {}; echo >> {0}", log.display());
    let fixture = Fixture::with_options("[\"/usr/bin/false\"]", &events_config(&script, 5));
    let pr = fixture.submit("src/a.txt");
    assert_success(&fixture.lprm(&["--json", "pr", "ready", &pr.pr_id]));
    let check = fixture.lprm(&["--json", "pr", "check", &pr.pr_id]);
    assert_eq!(check.status.code(), Some(5));

    let events = logged(&log);
    assert!(
        events
            .iter()
            .any(|event| event["event"] == "pr.check_failed")
    );
}

#[test]
fn broken_callbacks_never_change_results_or_state() {
    for (script, timeout) in [
        ("cat >/dev/null; exit 1", 5),
        ("cat >/dev/null; echo 'garbage {'", 5),
        ("cat >/dev/null; echo '{\"approve\":true}'", 5),
        ("sleep 30", 1),
        ("exit 0", 5),
    ] {
        let fixture = Fixture::with_options("[\"/usr/bin/true\"]", &events_config(script, timeout));
        let pr = fixture.submit("auth/login.txt");
        let id = &pr.pr_id;
        assert_success(&fixture.lprm(&["--json", "pr", "ready", id]));
        assert_success(&fixture.lprm(&["--json", "pr", "check", id]));

        // The gates still apply no matter what the callback says or does.
        let blocked = fixture.lprm(&["--json", "--actor", "c", "pr", "enqueue", id]);
        assert_eq!(blocked.status.code(), Some(5), "{script}");
        let stdout: Value = serde_json::from_slice(&blocked.stdout).unwrap();
        assert_eq!(stdout["error"]["code"], "VALIDATION_FAILED", "{script}");
        let shown = fixture.json_success(&["--json", "pr", "show", id]);
        assert_eq!(shown["data"]["status"], "approved", "{script}");
        let queue = fixture.json_success(&["--json", "queue", "list"]);
        assert_eq!(queue["data"].as_array().unwrap().len(), 0, "{script}");
    }
}

#[test]
fn oversized_events_are_dropped_not_fatal() {
    let log_dir = tempdir().unwrap();
    let log = log_dir.path().join("events.log");
    let config = format!(
        "[events]\nmax_payload_bytes = 10\ncommand = [\"/bin/sh\", \"-c\", 'cat >> {}']\n",
        log.display()
    );
    let fixture = Fixture::with_options("[\"/usr/bin/true\"]", &config);
    let pr = fixture.submit("src/a.txt");

    assert_success(&fixture.lprm(&["--json", "pr", "ready", &pr.pr_id]));

    assert_eq!(logged(&log), Vec::<Value>::new());
}
