use std::path::Path;

use alx::{AnnotationKind, App, NewAnnotation};
use tempfile::TempDir;

fn test_app() -> (TempDir, App) {
    let directory = tempfile::tempdir().unwrap();
    let app = App::new(directory.path().join("nested/alx.db")).unwrap();
    (directory, app)
}

fn migration_versions(database_path: impl AsRef<Path>) -> Vec<i64> {
    let connection = rusqlite::Connection::open(database_path).unwrap();
    let mut statement = connection
        .prepare("SELECT version FROM schema_migrations ORDER BY version")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<i64>>>()
        .unwrap()
}

#[test]
fn migrates_and_supports_task_crud_and_lookup() {
    let (directory, app) = test_app();
    assert_eq!(
        migration_versions(directory.path().join("nested/alx.db")),
        vec![1, 2, 3, 4]
    );

    let task = app.create_task("ARE-1175", "Investigate search").unwrap();
    assert_eq!(app.read_task("ARE-1175").unwrap(), task);
    assert_eq!(app.read_task(&task.uuid).unwrap(), task);
    assert_eq!(app.list_tasks().unwrap(), vec![task.clone()]);
    assert_eq!(app.search_tasks("search").unwrap(), vec![task]);
}

#[test]
fn completed_tasks_are_read_only_and_reopenable() {
    let (_directory, app) = test_app();
    let task = app.create_task("COMPLETE-1", "body").unwrap();
    let artifact = app.create_artifact(&task.uuid, "notes", "text").unwrap();
    let annotation = app
        .create_annotation(
            &artifact.uuid,
            NewAnnotation {
                kind: AnnotationKind::Comment,
                start_offset: None,
                end_offset: None,
                selected_text: None,
                body: Some("feedback".into()),
            },
        )
        .unwrap();

    app.complete_task("COMPLETE-1").unwrap();
    app.complete_task(&task.uuid).unwrap();
    assert!(app.list_tasks().unwrap().is_empty());
    assert!(app.search_tasks("body").unwrap().is_empty());
    let completed = app.list_completed_tasks().unwrap();
    assert_eq!(completed.len(), 1);
    assert!(completed[0].completed_at.is_some());
    assert!(completed[0].archived_at.is_none());

    assert!(app.update_task(&task.uuid, "changed").is_err());
    assert!(app.create_artifact(&task.uuid, "more", "blocked").is_err());
    assert!(app.update_artifact(&artifact.uuid, "changed").is_err());
    assert!(app.rename_artifact(&artifact.uuid, "changed.md").is_err());
    assert!(
        app.create_annotation(
            &artifact.uuid,
            NewAnnotation {
                kind: AnnotationKind::Good,
                start_offset: None,
                end_offset: None,
                selected_text: None,
                body: None,
            },
        )
        .is_err()
    );
    assert!(app.resolve_annotation(&annotation.uuid).is_err());

    app.reopen_task("COMPLETE-1").unwrap();
    app.reopen_task(&task.uuid).unwrap();
    assert!(app.list_completed_tasks().unwrap().is_empty());
    assert_eq!(app.list_tasks().unwrap().len(), 1);
    app.update_task(&task.uuid, "changed").unwrap();
    app.update_artifact(&artifact.uuid, "changed").unwrap();
    app.resolve_annotation(&annotation.uuid).unwrap();

    app.complete_task(&task.uuid).unwrap();
    app.archive_task(&task.uuid).unwrap();
    assert!(app.list_completed_tasks().unwrap().is_empty());
    assert_eq!(app.list_archived_tasks().unwrap().len(), 1);
    assert!(app.reopen_task(&task.uuid).is_err());
}

#[test]
fn completion_is_atomic_with_content_writes() {
    use std::sync::{Arc, Barrier};

    let (_directory, app) = test_app();
    for index in 0..64 {
        let task = app.create_task(&format!("RACE-{index}"), "before").unwrap();
        let artifact = app.create_artifact(&task.uuid, "notes", "before").unwrap();
        let barrier = Arc::new(Barrier::new(3));

        let task_writer = {
            let app = app.clone();
            let task_uuid = task.uuid.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let _ = app.update_task(&task_uuid, "after");
            })
        };
        let artifact_writer = {
            let app = app.clone();
            let artifact_uuid = artifact.uuid.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let _ = app.update_artifact(&artifact_uuid, "after");
            })
        };

        barrier.wait();
        app.complete_task(&task.uuid).unwrap();
        let task_body_at_completion = app.read_task(&task.uuid).unwrap().body;
        let artifact_body_at_completion = app.read_artifact(&artifact.uuid).unwrap().body;
        task_writer.join().unwrap();
        artifact_writer.join().unwrap();

        assert_eq!(
            app.read_task(&task.uuid).unwrap().body,
            task_body_at_completion
        );
        assert_eq!(
            app.read_artifact(&artifact.uuid).unwrap().body,
            artifact_body_at_completion
        );
    }
}

#[test]
fn archives_tasks_and_permanently_deletes_their_content() {
    let (_directory, app) = test_app();
    let task = app.create_task("ARCHIVE-1", "searchable body").unwrap();
    let artifact = app
        .create_artifact(&task.uuid, "notes", "artifact body")
        .unwrap();
    app.create_annotation(
        &artifact.uuid,
        NewAnnotation {
            kind: AnnotationKind::Comment,
            start_offset: None,
            end_offset: None,
            selected_text: None,
            body: Some("feedback".into()),
        },
    )
    .unwrap();

    app.archive_task("ARCHIVE-1").unwrap();
    app.archive_task(&task.uuid).unwrap();
    assert!(app.list_tasks().unwrap().is_empty());
    assert!(app.search_tasks("searchable").unwrap().is_empty());
    let archived = app.list_archived_tasks().unwrap();
    assert_eq!(archived.len(), 1);
    assert!(archived[0].archived_at.is_some());
    assert_eq!(app.read_task("ARCHIVE-1").unwrap().uuid, task.uuid);

    app.delete_task("ARCHIVE-1").unwrap();
    assert!(app.list_archived_tasks().unwrap().is_empty());
    assert!(app.read_task(&task.uuid).is_err());
    assert!(app.read_artifact(&artifact.uuid).is_err());
}

#[test]
fn normalizes_uuid_lookups_and_rejects_cross_namespace_collisions() {
    let (_directory, app) = test_app();
    let task = app.create_task("T-UUID", "body").unwrap();
    assert_eq!(
        app.read_task(&task.uuid.to_uppercase()).unwrap().uuid,
        task.uuid
    );

    let error = app.create_task(&task.uuid, "collision").unwrap_err();
    assert!(error.to_string().contains("conflicts"));

    let artifact = app.create_artifact("T-UUID", "notes", "old").unwrap();
    let uppercase_artifact = artifact.uuid.to_uppercase();
    assert_eq!(
        app.read_artifact(&uppercase_artifact).unwrap().uuid,
        artifact.uuid
    );
    app.update_artifact(&uppercase_artifact, "new").unwrap();
    let annotation = app
        .create_annotation(
            &uppercase_artifact,
            NewAnnotation {
                kind: AnnotationKind::Comment,
                start_offset: None,
                end_offset: None,
                selected_text: None,
                body: Some("note".into()),
            },
        )
        .unwrap();
    assert_eq!(annotation.artifact_uuid, artifact.uuid);
    assert_eq!(
        app.list_annotations(&uppercase_artifact, false)
            .unwrap()
            .len(),
        1
    );
    app.resolve_annotation(&annotation.uuid.to_uppercase())
        .unwrap();
    assert!(
        app.list_annotations(&uppercase_artifact, false)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn migrates_existing_tasks_and_artifacts_with_nullable_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("legacy.db");
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
             );
             INSERT INTO schema_migrations VALUES (1, '2025-01-01T00:00:00Z');
             CREATE TABLE tasks (
                uuid TEXT PRIMARY KEY,
                id TEXT NOT NULL UNIQUE,
                body TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
             );
             CREATE TABLE artifacts (
                uuid TEXT PRIMARY KEY,
                task_uuid TEXT NOT NULL REFERENCES tasks(uuid) ON DELETE CASCADE,
                type TEXT NOT NULL,
                body TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
             );
             INSERT INTO tasks VALUES (
                '01900000-0000-7000-8000-000000000001', 'OLD-1', 'task',
                '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z'
             );
             INSERT INTO artifacts VALUES (
                '01900000-0000-7000-8000-000000000002',
                '01900000-0000-7000-8000-000000000001', 'research', 'body',
                '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z'
             );",
        )
        .unwrap();
    drop(connection);

    let app = App::new(&database_path).unwrap();
    assert_eq!(migration_versions(&database_path), vec![1, 2, 3, 4]);
    assert_eq!(app.read_task("OLD-1").unwrap().archived_at, None);
    assert_eq!(app.read_task("OLD-1").unwrap().completed_at, None);
    let artifact = app
        .read_artifact("01900000-0000-7000-8000-000000000002")
        .unwrap();
    assert_eq!(artifact.name, None);
    assert_eq!(
        serde_json::to_value(&artifact).unwrap()["name"],
        serde_json::Value::Null
    );
    assert_eq!(artifact.display_name(), "research--01900000.md");
}

#[test]
fn permits_duplicate_artifact_types_and_keeps_them_in_context() {
    let (_directory, app) = test_app();
    let task = app.create_task("ARE-1", "Task body").unwrap();
    let first = app.create_artifact(&task.uuid, "notes", "First").unwrap();
    let second = app.create_artifact("ARE-1", "notes", "Second").unwrap();

    let artifacts = app.list_artifacts("ARE-1", Some("notes")).unwrap();
    assert_eq!(artifacts.len(), 2);
    assert_eq!(artifacts[0].uuid, first.uuid);
    assert_eq!(artifacts[1].uuid, second.uuid);

    let context = app.context_markdown("ARE-1").unwrap();
    assert!(context.contains("# Task: ARE-1"));
    assert!(context.contains(&format!("### notes ({})", first.uuid)));
    assert!(context.contains(&format!("### notes ({})", second.uuid)));
    assert!(context.contains("First"));
    assert!(context.contains("Second"));
}

#[test]
fn names_allow_duplicates_and_do_not_change_identity_type_or_references() {
    let (_directory, app) = test_app();
    app.create_task("T-NAMES", "body").unwrap();
    let first = app
        .create_artifact_with_name("T-NAMES", "research", Some("same.md"), "one")
        .unwrap();
    let second = app
        .create_artifact_with_name("T-NAMES", "plan", Some("other.md"), "two")
        .unwrap();
    let annotation = app
        .create_annotation(
            &first.uuid,
            NewAnnotation {
                kind: AnnotationKind::Comment,
                start_offset: None,
                end_offset: None,
                selected_text: None,
                body: Some("note".into()),
            },
        )
        .unwrap();

    app.rename_artifact(&first.uuid, "renamed.md").unwrap();
    app.rename_artifact(&second.uuid, "renamed.md").unwrap();

    let renamed = app.read_artifact(&first.uuid).unwrap();
    assert_eq!(renamed.uuid, first.uuid);
    assert_eq!(renamed.task_uuid, first.task_uuid);
    assert_eq!(renamed.artifact_type, "research");
    assert_eq!(renamed.name.as_deref(), Some("renamed.md"));
    assert_eq!(renamed.body, "one");
    assert_eq!(app.read_artifact(&second.uuid).unwrap().name, renamed.name);
    assert_eq!(
        app.list_annotations(&first.uuid, false).unwrap()[0].uuid,
        annotation.uuid
    );
}

#[test]
fn updates_tasks_and_artifacts_and_reports_missing_records() {
    let (_directory, app) = test_app();
    let task = app.create_task("T-1", "body").unwrap();
    app.update_task("T-1", "revised").unwrap();
    let updated = app.read_task(&task.uuid).unwrap();
    assert_eq!(updated.body, "revised");
    assert_eq!(updated.id, task.id);
    assert!(updated.updated_at >= task.updated_at);
    let artifact = app.create_artifact("T-1", "plan", "old").unwrap();
    app.update_artifact(&artifact.uuid, "new").unwrap();
    assert_eq!(app.read_artifact(&artifact.uuid).unwrap().body, "new");
    assert!(
        app.read_task("missing")
            .unwrap_err()
            .to_string()
            .contains("not found")
    );
}

#[test]
fn feedback_is_grouped_and_resolved_annotations_are_filtered() {
    let (_directory, app) = test_app();
    app.create_task("T-2", "body").unwrap();
    let artifact = app.create_artifact("T-2", "research", "text").unwrap();
    let question = app
        .create_annotation(
            &artifact.uuid,
            NewAnnotation {
                kind: AnnotationKind::Question,
                start_offset: Some(0),
                end_offset: Some(4),
                selected_text: Some("line one\nline two".into()),
                body: Some("Are we sure?".into()),
            },
        )
        .unwrap();
    app.create_annotation(
        &artifact.uuid,
        NewAnnotation {
            kind: AnnotationKind::Good,
            start_offset: None,
            end_offset: None,
            selected_text: Some("Keep this".into()),
            body: None,
        },
    )
    .unwrap();

    let feedback = app.feedback_markdown(&artifact.uuid).unwrap();
    assert!(feedback.starts_with("# Feedback\n"));
    assert!(feedback.contains("## Question"));
    assert!(feedback.contains("> line one\n> line two"));
    assert!(feedback.contains("Are we sure?"));
    assert!(feedback.contains("## Good"));

    let review = app.review_markdown(&artifact.uuid).unwrap();
    assert!(review.starts_with(&format!(
        "# Artifact Review\n\nArtifact UUID: {}\n",
        artifact.uuid
    )));
    assert!(review.contains("\n## Feedback\n"));
    assert!(review.contains("\n### Question\n"));
    assert!(review.contains("\n### Good\n"));
    assert!(!review.contains("\n# Feedback\n"));

    app.resolve_annotation(&question.uuid).unwrap();
    let unresolved = app.list_annotations(&artifact.uuid, false).unwrap();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].kind, AnnotationKind::Good);
    assert_eq!(app.list_annotations(&artifact.uuid, true).unwrap().len(), 2);
    assert!(
        !app.feedback_markdown(&artifact.uuid)
            .unwrap()
            .contains("Are we sure?")
    );
}

#[test]
fn validates_annotation_offsets_and_input() {
    let (_directory, app) = test_app();
    app.create_task("T-3", "body").unwrap();
    let artifact = app.create_artifact("T-3", "x", "body").unwrap();
    let error = app
        .create_annotation(
            &artifact.uuid,
            NewAnnotation {
                kind: AnnotationKind::Comment,
                start_offset: Some(4),
                end_offset: Some(2),
                selected_text: None,
                body: None,
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("end offset"));
    assert!(app.create_task(" ", "").is_err());
    assert!(app.create_artifact("T-3", "", "").is_err());
}
