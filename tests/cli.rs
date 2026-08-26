use std::path::PathBuf;

use alx::{AGENT_SKILL, Annotation, Artifact, Task};
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
    assert_eq!(fields.len(), 4);
    assert_eq!(fields[0], artifact_uuid);
    assert_eq!(fields[1], "research");
    assert_eq!(fields[2], format!("research--{}.md", &artifact_uuid[..8]));
    assert!(fields[3].contains('T'));
    let artifacts: Vec<Artifact> = serde_json::from_str(&stdout(
        command(&directory).args(["artifact", "list", "ARE-1175", "--json"]),
    ))
    .unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].uuid, artifact_uuid);
    assert_eq!(
        artifacts[0].name.as_deref(),
        Some(format!("research--{}.md", &artifact_uuid[..8]).as_str())
    );
    assert_eq!(artifacts[0].body, "# Findings\n\nOld\n");

    let expected_context = format!(
        "# Task: ARE-1175\n\nTask body\n\n\n## Agent instructions\n\nWhen creating an artifact, provide a short descriptive filename with `--name` when there is an obvious one. Use the artifact UUID for all subsequent operations.\n\n## Artifacts\n\n### research ({artifact_uuid})\n\n# Findings\n\nOld\n\n"
    );
    assert_eq!(
        stdout(command(&directory).args(["task", "context", "ARE-1175"])),
        expected_context
    );
}

#[test]
fn task_archive_and_confirmation_gated_delete_have_no_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let task_uuid = create_task(&directory);
    create_artifact(&directory);

    command(&directory)
        .args(["task", "archive", "ARE-1175"])
        .assert()
        .success()
        .stdout("");
    command(&directory)
        .args(["task", "list"])
        .assert()
        .success()
        .stdout("");
    let archived: Vec<Task> = serde_json::from_str(&stdout(command(&directory).args([
        "task",
        "list",
        "--archived",
        "--json",
    ])))
    .unwrap();
    assert_eq!(archived[0].uuid, task_uuid);
    assert!(archived[0].archived_at.is_some());

    command(&directory)
        .args(["task", "delete", "ARE-1175"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("requires --confirm"));
    command(&directory)
        .args(["task", "delete", "ARE-1175", "--confirm"])
        .assert()
        .success()
        .stdout("");
    command(&directory)
        .args(["task", "read", "ARE-1175"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("task not found"));
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
fn artifact_names_can_be_set_and_renamed_without_changing_uuid_or_type() {
    let directory = tempfile::tempdir().unwrap();
    create_task(&directory);
    let artifact_uuid = stdout(
        command(&directory)
            .args([
                "artifact",
                "create",
                "ARE-1175",
                "research",
                "--name",
                "meilisearch-index-findings.md",
            ])
            .write_stdin("findings"),
    )
    .trim()
    .to_owned();

    command(&directory)
        .args([
            "artifact",
            "rename",
            &artifact_uuid,
            "pwa-filter-semantics.md",
        ])
        .assert()
        .success()
        .stdout("");

    let artifacts: Vec<Artifact> = serde_json::from_str(&stdout(
        command(&directory).args(["artifact", "list", "ARE-1175", "--json"]),
    ))
    .unwrap();
    assert_eq!(artifacts[0].uuid, artifact_uuid);
    assert_eq!(artifacts[0].artifact_type, "research");
    assert_eq!(
        artifacts[0].name.as_deref(),
        Some("pwa-filter-semantics.md")
    );
    assert_eq!(artifacts[0].body, "findings");
}

#[test]
fn task_update_has_no_stdout_and_replaces_body() {
    let directory = tempfile::tempdir().unwrap();
    let task_uuid = create_task(&directory);

    command(&directory)
        .args(["task", "update", "ARE-1175"])
        .write_stdin("revised body\n")
        .assert()
        .success()
        .stdout("");
    command(&directory)
        .args(["task", "read", &task_uuid])
        .assert()
        .success()
        .stdout("revised body\n");

    command(&directory)
        .args(["task", "update", "missing"])
        .write_stdin("body")
        .assert()
        .failure()
        .stderr(predicate::str::contains("task not found"));
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
    assert_eq!(
        stdout(command(&directory).args(["artifact", "review", &artifact_uuid])),
        format!(
            "# Artifact Review\n\nArtifact UUID: {artifact_uuid}\n\nYou must review and address all unresolved feedback below.\n\nRules:\n- Read the current artifact before making changes.\n- Address every unresolved `comment`, `question`, and `scratch`.\n- Treat `good` annotations as guidance to preserve that part unless conflicting feedback requires otherwise.\n- Do not resolve annotations until their feedback has been addressed.\n- Update the existing artifact instead of creating a replacement unless explicitly requested.\n\n## Feedback\n\n### Question\n\n> Findings\n\nWhy?\n"
        )
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

#[test]
fn skill_read_outputs_embedded_skill_without_creating_database() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("alx.db");
    assert!(AGENT_SKILL.contains(
        "When creating an artifact, provide a short descriptive filename with `--name` when there is an obvious one. Use the artifact UUID for all subsequent operations."
    ));

    command(&directory)
        .args(["skill", "read"])
        .assert()
        .success()
        .stdout(AGENT_SKILL);

    assert!(!database.exists());
}

#[cfg(unix)]
#[test]
fn skill_install_writes_embedded_skill_to_global_agent_skills_directory() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(".agents/skills/alx/SKILL.md");

    command(&directory)
        .env("HOME", directory.path())
        .args(["skill", "install"])
        .assert()
        .success()
        .stdout(format!("{}\n", path.display()));

    assert_eq!(std::fs::read_to_string(path).unwrap(), AGENT_SKILL);
    assert!(!directory.path().join("alx.db").exists());
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
