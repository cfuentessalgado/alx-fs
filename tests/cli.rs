use std::path::PathBuf;

use alx::{Annotation, Artifact, Task};
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn command(directory: &TempDir) -> Command {
    let mut command = Command::cargo_bin("alx").unwrap();
    command.env("ALX_DB", directory.path().join("alx.db"));
    command
}

fn stdout(command: &mut Command) -> String {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn create_task(directory: &TempDir) -> String {
    stdout(
        command(directory)
            .args(["task", "create", "ARE-1175"])
            .write_stdin("Task body\n"),
    )
    .trim()
    .to_owned()
}

fn create_artifact(directory: &TempDir) -> String {
    stdout(
        command(directory)
            .args(["artifact", "create", "ARE-1175", "research"])
            .write_stdin("# Findings\n\nOld\n"),
    )
    .trim()
    .to_owned()
}

#[test]
fn create_read_list_search_and_context_stdout_contracts() {
    let directory = tempfile::tempdir().unwrap();
    let task_uuid = create_task(&directory);
    uuid::Uuid::parse_str(&task_uuid).unwrap();

    command(&directory)
        .args(["task", "read", &task_uuid])
        .assert()
        .success()
        .stdout("Task body\n");

    let plain_list = stdout(command(&directory).args(["task", "list"]));
    let fields: Vec<_> = plain_list.trim_end().split('\t').collect();
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0], task_uuid);
    assert_eq!(fields[1], "ARE-1175");
    assert!(fields[2].contains('T'));
    assert_eq!(
        stdout(command(&directory).args(["task", "search", "body"])),
        plain_list
    );

    let listed: Vec<Task> = serde_json::from_str(&stdout(
        command(&directory).args(["task", "list", "--json"]),
    ))
    .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].uuid, task_uuid);
    assert_eq!(listed[0].id, "ARE-1175");
    assert_eq!(listed[0].body, "Task body\n");
    let searched: Vec<Task> = serde_json::from_str(&stdout(
        command(&directory).args(["task", "search", "body", "--json"]),
    ))
    .unwrap();
    assert_eq!(searched, listed);

    let artifact_uuid = create_artifact(&directory);
    let plain_artifacts =
        stdout(command(&directory).args(["artifact", "list", "ARE-1175", "--type", "research"]));
    let fields: Vec<_> = plain_artifacts.trim_end().split('\t').collect();
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0], artifact_uuid);
    assert_eq!(fields[1], "research");
    assert!(fields[2].contains('T'));
    let artifacts: Vec<Artifact> = serde_json::from_str(&stdout(
        command(&directory).args(["artifact", "list", "ARE-1175", "--json"]),
    ))
    .unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].uuid, artifact_uuid);
    assert_eq!(artifacts[0].body, "# Findings\n\nOld\n");

    let expected_context = format!(
        "# Task: ARE-1175\n\nTask body\n\n\n## Artifacts\n\n### research ({artifact_uuid})\n\n# Findings\n\nOld\n\n"
    );
    assert_eq!(
        stdout(command(&directory).args(["task", "context", "ARE-1175"])),
        expected_context
    );
}

#[test]
fn plain_task_output_escapes_tsv_control_characters() {
    let directory = tempfile::tempdir().unwrap();
    let id = "A\tB\\C\nD\rE";
    stdout(
        command(&directory)
            .args(["task", "create", id])
            .write_stdin("body"),
    );
    let output = stdout(command(&directory).args(["task", "list"]));
    assert_eq!(output.lines().count(), 1);
    let fields: Vec<_> = output.trim_end().split('\t').collect();
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[1], "A\\tB\\\\C\\nD\\rE");
}

#[test]
fn artifact_update_has_no_stdout_and_replaces_body() {
    let directory = tempfile::tempdir().unwrap();
    create_task(&directory);
    let artifact_uuid = create_artifact(&directory);

    command(&directory)
        .args(["artifact", "update", &artifact_uuid])
        .write_stdin("replacement")
        .assert()
        .success()
        .stdout("");
    command(&directory)
        .args(["artifact", "read", &artifact_uuid])
        .assert()
        .success()
        .stdout("replacement");
}

#[test]
fn annotation_cli_creates_lists_feedback_and_resolves() {
    let directory = tempfile::tempdir().unwrap();
    create_task(&directory);
    let artifact_uuid = create_artifact(&directory);
    let annotation_uuid = stdout(
        command(&directory)
            .args([
                "annotation",
                "create",
                &artifact_uuid,
                "question",
                "--start-offset",
                "0",
                "--end-offset",
                "8",
                "--selected-text",
                "Findings",
            ])
            .write_stdin("Why?"),
    )
    .trim()
    .to_owned();

    assert_eq!(
        stdout(command(&directory).args(["annotation", "list", &artifact_uuid])),
        format!("{annotation_uuid}\tquestion\t0\t8\t\\N\n")
    );
    assert_eq!(
        stdout(command(&directory).args(["artifact", "feedback", &artifact_uuid])),
        "# Feedback\n\n## Question\n\n> Findings\n\nWhy?\n"
    );
    let feedback: Vec<Annotation> = serde_json::from_str(&stdout(command(&directory).args([
        "artifact",
        "feedback",
        &artifact_uuid,
        "--json",
    ])))
    .unwrap();
    assert_eq!(feedback.len(), 1);
    assert_eq!(feedback[0].uuid, annotation_uuid);
    assert_eq!(feedback[0].selected_text.as_deref(), Some("Findings"));
    assert_eq!(feedback[0].body.as_deref(), Some("Why?"));
    assert!(feedback[0].resolved_at.is_none());

    command(&directory)
        .args(["annotation", "resolve", &annotation_uuid])
        .assert()
        .success()
        .stdout("");
    command(&directory)
        .args(["annotation", "list", &artifact_uuid])
        .assert()
        .success()
        .stdout("");
    let all_plain =
        stdout(command(&directory).args(["annotation", "list", &artifact_uuid, "--all"]));
    let fields: Vec<_> = all_plain.trim_end().split('\t').collect();
    assert_eq!(fields.len(), 5);
    assert_eq!(fields[0], annotation_uuid);
    assert_ne!(fields[4], "\\N");
    let all: Vec<Annotation> = serde_json::from_str(&stdout(command(&directory).args([
        "annotation",
        "list",
        &artifact_uuid,
        "--all",
        "--json",
    ])))
    .unwrap();
    assert_eq!(all.len(), 1);
    assert!(all[0].resolved_at.is_some());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn default_data_directory_is_created_without_alx_db() {
    let directory = tempfile::tempdir().unwrap();
    let mut command = Command::cargo_bin("alx").unwrap();
    command.env_remove("ALX_DB");
    command.env("HOME", directory.path());

    #[cfg(target_os = "linux")]
    let expected: PathBuf = {
        let data = directory.path().join("data");
        command.env("XDG_DATA_HOME", &data);
        data.join("alx/alx.db")
    };
    #[cfg(target_os = "macos")]
    let expected: PathBuf = directory
        .path()
        .join("Library/Application Support/alx/alx.db");

    command.args(["task", "list"]).assert().success().stdout("");
    assert!(expected.is_file(), "missing {}", expected.display());
}

#[test]
fn invalid_inputs_fail_on_stderr() {
    let directory = tempfile::tempdir().unwrap();
    command(&directory)
        .args(["task", "read", "missing"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("task not found"));
    command(&directory)
        .args(["serve", "--bind", "127.0.0.1:3000", "--tailscale"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
    command(&directory)
        .args(["serve", "--bind", "not-an-address"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid bind address"));
}
