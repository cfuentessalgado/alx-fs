use std::path::PathBuf;

use alx::{AGENT_SKILL, Annotation, Artifact, ArtifactInfo, GrepMatch, Task};
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
fn artifact_info_resolves_its_owning_task() {
    let directory = tempfile::tempdir().unwrap();
    let task_uuid = create_task(&directory);
    let artifact_uuid = create_artifact(&directory);

    let info: ArtifactInfo = serde_json::from_str(&stdout(command(&directory).args([
        "artifact",
        "info",
        &artifact_uuid,
        "--json",
    ])))
    .unwrap();
    assert_eq!(info.uuid, artifact_uuid);
    assert_eq!(info.task_uuid, task_uuid);
    assert_eq!(info.task_id, "ARE-1175");
    assert_eq!(info.artifact_type, "research");

    let plain = stdout(command(&directory).args(["artifact", "info", &artifact_uuid]));
    assert!(plain.contains(&format!("Artifact UUID: {artifact_uuid}\n")));
    assert!(plain.contains("Task ID:      ARE-1175\n"));
    assert!(plain.contains(&format!("Task UUID:    {task_uuid}\n")));
    assert!(plain.contains("Type:         research\n"));
    assert!(plain.contains(&format!(
        "Name:         research--{}.md\n",
        &artifact_uuid[..8]
    )));
}

#[test]
fn grep_searches_tasks_artifacts_and_annotations_with_locations() {
    let directory = tempfile::tempdir().unwrap();
    create_task(&directory);
    let artifact_uuid = create_artifact(&directory);
    let annotation_uuid = stdout(
        command(&directory)
            .args(["annotation", "create", &artifact_uuid, "comment"])
            .write_stdin("Old feedback\n"),
    )
    .trim()
    .to_owned();

    let output = stdout(command(&directory).args(["grep", "body|Old"]));
    assert!(output.contains("ARE-1175/task.md:1:Task body\n"));
    assert!(output.contains(&format!(
        "ARE-1175/research/research--{}.md [{artifact_uuid}]:3:Old\n",
        &artifact_uuid[..8],
    )));
    assert!(output.contains(&format!(
        "ARE-1175/research/research--{}.md/annotations/{annotation_uuid}.md [{artifact_uuid}]:1:Old feedback\n",
        &artifact_uuid[..8],
    )));

    let matches: Vec<GrepMatch> = serde_json::from_str(&stdout(command(&directory).args([
        "grep",
        "^# Findings$",
        "--json",
    ])))
    .unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(
        matches[0].artifact_uuid.as_deref(),
        Some(artifact_uuid.as_str())
    );
    assert_eq!(matches[0].line_number, 1);
    assert_eq!(matches[0].line, "# Findings");
}

#[test]
fn dump_writes_a_file_tree_with_task_and_artifact_files() {
    let directory = tempfile::tempdir().unwrap();
    create_task(&directory);
    let artifact_uuid = create_artifact(&directory);
    let target = directory.path().join("export");

    command(&directory)
        .args(["dump", target.to_str().unwrap()])
        .assert()
        .success()
        .stdout("");

    assert_eq!(
        std::fs::read_to_string(target.join("ARE-1175/task.md")).unwrap(),
        "Task body\n"
    );
    assert_eq!(
        std::fs::read_to_string(target.join(format!(
            "ARE-1175/research/research--{}.md",
            &artifact_uuid[..8]
        )))
        .unwrap(),
        "# Findings\n\nOld\n"
    );
}

#[test]
fn dump_with_task_key_limits_output_and_includes_inactive_tasks_by_default() {
    let directory = tempfile::tempdir().unwrap();
    create_task(&directory);
    create_artifact(&directory);
    stdout(
        command(&directory)
            .args(["task", "create", "ARE-2"])
            .write_stdin("Other task\n"),
    );
    command(&directory)
        .args(["task", "archive", "ARE-2"])
        .assert()
        .success();
    stdout(
        command(&directory)
            .args(["task", "create", "ARE-3"])
            .write_stdin("Completed task\n"),
    );
    command(&directory)
        .args(["task", "complete", "ARE-3"])
        .assert()
        .success();

    let single = tempfile::tempdir().unwrap();
    let single_target = single.path().join("export");
    command(&directory)
        .args(["dump", "ARE-1175", single_target.to_str().unwrap()])
        .assert()
        .success();
    assert!(single_target.join("ARE-1175/task.md").is_file());
    assert!(!single_target.join("ARE-2").exists());

    let all = tempfile::tempdir().unwrap();
    let all_target = all.path().join("export");
    command(&directory)
        .args(["dump", all_target.to_str().unwrap()])
        .assert()
        .success();
    assert!(all_target.join("ARE-1175/task.md").is_file());
    assert!(all_target.join("ARE-2/task.md").is_file());
    assert!(all_target.join("ARE-3/task.md").is_file());

    command(&directory)
        .args([
            "dump",
            "missing",
            all.path().join("export2").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("task not found"));
}

#[test]
fn dump_suffixes_duplicate_artifact_names_and_sanitizes_unsafe_components() {
    let directory = tempfile::tempdir().unwrap();
    create_task(&directory);
    let _ = stdout(
        command(&directory)
            .args([
                "artifact",
                "create",
                "ARE-1175",
                "a/b",
                "--name",
                "findings.md",
            ])
            .write_stdin("first"),
    );
    let second = stdout(
        command(&directory)
            .args([
                "artifact",
                "create",
                "ARE-1175",
                "a/b",
                "--name",
                "findings.md",
            ])
            .write_stdin("second"),
    )
    .trim()
    .to_owned();

    let target = directory.path().join("export");
    command(&directory)
        .args(["dump", target.to_str().unwrap()])
        .assert()
        .success();

    let folder = target.join("ARE-1175/a_b");
    assert_eq!(
        std::fs::read_to_string(folder.join("findings.md")).unwrap(),
        "first"
    );
    assert_eq!(
        std::fs::read_to_string(folder.join(format!("findings--{}.md", &second[..8]))).unwrap(),
        "second"
    );
}

#[test]
fn dump_zip_writes_one_archive_with_the_same_tree() {
    let directory = tempfile::tempdir().unwrap();
    create_task(&directory);
    let artifact_uuid = create_artifact(&directory);
    stdout(
        command(&directory)
            .args(["task", "create", "ARE-2"])
            .write_stdin("Other task\n"),
    );

    let target = directory.path().join("export.zip");
    command(&directory)
        .args(["dump", "--zip", "ARE-1175", target.to_str().unwrap()])
        .assert()
        .success()
        .stdout("");

    let mut archive = zip::ZipArchive::new(std::fs::File::open(&target).unwrap()).unwrap();
    let names: Vec<String> = archive.file_names().map(str::to_owned).collect();
    assert_eq!(
        names,
        vec![
            "ARE-1175/task.md".to_owned(),
            format!("ARE-1175/research/research--{}.md", &artifact_uuid[..8]),
        ]
    );
    let mut file = archive.by_name("ARE-1175/task.md").unwrap();
    let mut contents = String::new();
    std::io::Read::read_to_string(&mut file, &mut contents).unwrap();
    assert_eq!(contents, "Task body\n");
}

#[test]
fn dump_rejects_a_file_target_for_a_directory_tree() {
    let directory = tempfile::tempdir().unwrap();
    create_task(&directory);
    let target = directory.path().join("occupied");
    std::fs::write(&target, "not a directory").unwrap();

    command(&directory)
        .args(["dump", target.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not a directory"));
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
fn task_complete_list_and_reopen_manage_the_completed_state() {
    let directory = tempfile::tempdir().unwrap();
    let task_uuid = create_task(&directory);

    command(&directory)
        .args(["task", "complete", "ARE-1175"])
        .assert()
        .success()
        .stdout("");
    command(&directory)
        .args(["task", "list"])
        .assert()
        .success()
        .stdout("");
    let completed: Vec<Task> = serde_json::from_str(&stdout(command(&directory).args([
        "task",
        "list",
        "--completed",
        "--json",
    ])))
    .unwrap();
    assert_eq!(completed[0].uuid, task_uuid);
    assert!(completed[0].completed_at.is_some());

    command(&directory)
        .args(["task", "update", "ARE-1175"])
        .write_stdin("blocked")
        .assert()
        .failure()
        .stderr(predicate::str::contains("read-only"));

    command(&directory)
        .args(["task", "reopen", "ARE-1175"])
        .assert()
        .success()
        .stdout("");
    assert!(
        stdout(command(&directory).args(["task", "list"]))
            .starts_with(&format!("{task_uuid}\tARE-1175\t"))
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
fn task_rename_has_no_stdout_and_keeps_uuid_body_and_artifacts() {
    let directory = tempfile::tempdir().unwrap();
    let task_uuid = create_task(&directory);
    let artifact_uuid = create_artifact(&directory);

    command(&directory)
        .args(["task", "rename", "ARE-1175", "ARE-1176"])
        .assert()
        .success()
        .stdout("");

    command(&directory)
        .args(["task", "read", "ARE-1175"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("task not found"));
    command(&directory)
        .args(["task", "read", "ARE-1176"])
        .assert()
        .success()
        .stdout("Task body\n");
    let tasks: Vec<Task> = serde_json::from_str(&stdout(
        command(&directory).args(["task", "list", "--json"]),
    ))
    .unwrap();
    assert_eq!(tasks[0].uuid, task_uuid);
    assert_eq!(tasks[0].id, "ARE-1176");
    let artifacts: Vec<Artifact> = serde_json::from_str(&stdout(
        command(&directory).args(["artifact", "list", "ARE-1176", "--json"]),
    ))
    .unwrap();
    assert_eq!(artifacts[0].uuid, artifact_uuid);

    stdout(
        command(&directory)
            .args(["task", "create", "ARE-1177"])
            .write_stdin("other"),
    );
    command(&directory)
        .args(["task", "rename", "ARE-1176", "ARE-1177"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
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
