use alx::{AnnotationKind, App, NewAnnotation};
use tempfile::TempDir;

fn test_app() -> (TempDir, App) {
    let directory = tempfile::tempdir().unwrap();
    let app = App::new(directory.path().join("nested/alx.db")).unwrap();
    (directory, app)
}

#[test]
fn migrates_and_supports_task_crud_and_lookup() {
    let (_directory, app) = test_app();
    assert_eq!(app.migration_versions().unwrap(), vec![1]);

    let task = app.create_task("ARE-1175", "Investigate search").unwrap();
    assert_eq!(app.read_task("ARE-1175").unwrap(), task);
    assert_eq!(app.read_task(&task.uuid).unwrap(), task);
    assert_eq!(app.list_tasks().unwrap(), vec![task.clone()]);
    assert_eq!(app.search_tasks("search").unwrap(), vec![task]);
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
fn updates_artifacts_and_reports_missing_records() {
    let (_directory, app) = test_app();
    app.create_task("T-1", "body").unwrap();
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
