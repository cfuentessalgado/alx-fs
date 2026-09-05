use std::{path::PathBuf, str::FromStr, time::Duration};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::error::{invalid, not_found};
use crate::model::{SearchDocument, artifact_fallback_name};
use crate::{Annotation, AnnotationKind, Artifact, NewAnnotation, Task, new_uuid, now};

const SCHEMA: &str = include_str!("schema.sql");

pub(crate) trait Store: Send + Sync {
    fn create_task(&self, id: &str, body: &str) -> Result<Task>;
    fn read_task(&self, key: &str) -> Result<Task>;
    fn list_tasks(&self) -> Result<Vec<Task>>;
    fn list_completed_tasks(&self) -> Result<Vec<Task>>;
    fn list_archived_tasks(&self) -> Result<Vec<Task>>;
    fn search_tasks(&self, query: &str) -> Result<Vec<Task>>;
    fn search_documents(&self) -> Result<Vec<SearchDocument>>;
    fn edit_task(&self, key: &str, id: Option<&str>, body: Option<&str>) -> Result<()>;
    fn complete_task(&self, key: &str) -> Result<()>;
    fn reopen_task(&self, key: &str) -> Result<()>;
    fn archive_task(&self, key: &str) -> Result<()>;
    fn delete_task(&self, key: &str) -> Result<()>;
    fn create_artifact(&self, task_key: &str, artifact_type: &str, body: &str) -> Result<Artifact>;
    fn create_artifact_with_name(
        &self,
        task_key: &str,
        artifact_type: &str,
        name: Option<&str>,
        body: &str,
    ) -> Result<Artifact>;
    fn read_artifact(&self, uuid: &str) -> Result<Artifact>;
    fn update_artifact(&self, uuid: &str, body: &str) -> Result<()>;
    fn rename_artifact(&self, uuid: &str, name: &str) -> Result<()>;
    fn list_artifacts(&self, task_key: &str, artifact_type: Option<&str>) -> Result<Vec<Artifact>>;
    fn create_annotation(&self, artifact_uuid: &str, input: NewAnnotation) -> Result<Annotation>;
    fn list_annotations(
        &self,
        artifact_uuid: &str,
        include_resolved: bool,
    ) -> Result<Vec<Annotation>>;
    fn resolve_annotation(&self, uuid: &str) -> Result<()>;
}

pub(crate) struct SqliteStore {
    database_path: PathBuf,
}

impl SqliteStore {
    pub(crate) fn new(database_path: impl Into<PathBuf>) -> Result<Self> {
        let database_path = database_path.into();
        if let Some(parent) = database_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create database directory {}", parent.display())
            })?;
        }
        let store = Self { database_path };
        store.migrate()?;
        Ok(store)
    }

    fn connect(&self) -> Result<Connection> {
        let connection = Connection::open(&self.database_path).with_context(|| {
            format!(
                "failed to open SQLite database {}",
                self.database_path.display()
            )
        })?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        Ok(connection)
    }

    fn migrate(&self) -> Result<()> {
        let mut connection = self.connect()?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(SCHEMA)?;
        transaction.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (1, ?1)",
            [now()],
        )?;
        let has_artifact_name: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM pragma_table_info('artifacts') WHERE name = 'name'
             )",
            [],
            |row| row.get(0),
        )?;
        if !has_artifact_name {
            transaction.execute("ALTER TABLE artifacts ADD COLUMN name TEXT", [])?;
        }
        transaction.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (2, ?1)",
            [now()],
        )?;
        let has_task_archived_at: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM pragma_table_info('tasks') WHERE name = 'archived_at'
             )",
            [],
            |row| row.get(0),
        )?;
        if !has_task_archived_at {
            transaction.execute("ALTER TABLE tasks ADD COLUMN archived_at TEXT", [])?;
        }
        transaction.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (3, ?1)",
            [now()],
        )?;
        let has_task_completed_at: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM pragma_table_info('tasks') WHERE name = 'completed_at'
             )",
            [],
            |row| row.get(0),
        )?;
        if !has_task_completed_at {
            transaction.execute("ALTER TABLE tasks ADD COLUMN completed_at TEXT", [])?;
        }
        transaction.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (4, ?1)",
            [now()],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

impl Store for SqliteStore {
    fn create_task(&self, id: &str, body: &str) -> Result<Task> {
        validate_non_empty("task id", id)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let duplicate = transaction
            .query_row("SELECT 1 FROM tasks WHERE id = ?1", [id], |_| Ok(()))
            .optional()?;
        if duplicate.is_some() {
            return Err(invalid(format!("task id already exists: {id}")));
        }

        let normalized_id = normalized_uuid(id);
        if let Some(normalized_id) = normalized_id.as_deref() {
            let collision = transaction
                .query_row(
                    "SELECT 1 FROM tasks WHERE uuid = ?1",
                    [normalized_id],
                    |_| Ok(()),
                )
                .optional()?;
            if collision.is_some() {
                return Err(invalid(format!(
                    "task id conflicts with an existing task UUID: {id}"
                )));
            }
        }

        let uuid = loop {
            let candidate = new_uuid();
            let mut statement = transaction.prepare("SELECT id FROM tasks")?;
            let ids = statement.query_map([], |row| row.get::<_, String>(0))?;
            let mut collision = normalized_id.as_deref() == Some(candidate.as_str());
            for existing_id in ids {
                if normalized_uuid(&existing_id?).as_deref() == Some(candidate.as_str()) {
                    collision = true;
                    break;
                }
            }
            drop(statement);
            if !collision {
                break candidate;
            }
        };
        let timestamp = now();
        let task = Task {
            uuid,
            id: id.to_owned(),
            body: body.to_owned(),
            created_at: timestamp.clone(),
            updated_at: timestamp,
            archived_at: None,
            completed_at: None,
        };
        transaction
            .execute(
                "INSERT INTO tasks(uuid, id, body, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![task.uuid, task.id, task.body, task.created_at, task.updated_at],
            )
            .with_context(|| format!("failed to create task {id}"))?;
        transaction.commit()?;
        Ok(task)
    }

    fn read_task(&self, key: &str) -> Result<Task> {
        validate_non_empty("task identifier", key)?;
        read_task_from_connection(&self.connect()?, key)
    }

    fn list_tasks(&self) -> Result<Vec<Task>> {
        self.list_tasks_with_condition("archived_at IS NULL AND completed_at IS NULL")
    }

    fn list_completed_tasks(&self) -> Result<Vec<Task>> {
        self.list_tasks_with_condition("archived_at IS NULL AND completed_at IS NOT NULL")
    }

    fn list_archived_tasks(&self) -> Result<Vec<Task>> {
        self.list_tasks_with_condition("archived_at IS NOT NULL")
    }

    fn search_tasks(&self, query: &str) -> Result<Vec<Task>> {
        validate_non_empty("search query", query)?;
        let pattern = format!("%{query}%");
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT uuid, id, body, created_at, updated_at, archived_at, completed_at FROM tasks
             WHERE archived_at IS NULL AND completed_at IS NULL AND (id LIKE ?1 OR body LIKE ?1)
             ORDER BY updated_at DESC, uuid",
        )?;
        let rows = statement.query_map([pattern], task_from_row)?;
        collect_rows(rows)
    }

    fn search_documents(&self) -> Result<Vec<SearchDocument>> {
        let connection = self.connect()?;
        let mut documents = Vec::new();

        {
            let mut statement = connection.prepare("SELECT id, body FROM tasks ORDER BY id")?;
            let rows = statement.query_map([], |row| {
                Ok(SearchDocument {
                    path: format!("{}/task.md", row.get::<_, String>(0)?),
                    artifact_uuid: None,
                    body: row.get(1)?,
                })
            })?;
            documents.extend(collect_rows(rows)?);
        }
        {
            let mut statement = connection.prepare(
                "SELECT tasks.id, artifacts.type, artifacts.name, artifacts.uuid, artifacts.body
                 FROM artifacts JOIN tasks ON tasks.uuid = artifacts.task_uuid
                 ORDER BY tasks.id, artifacts.created_at, artifacts.uuid",
            )?;
            let rows = statement.query_map([], |row| {
                let artifact_type: String = row.get(1)?;
                let uuid: String = row.get(3)?;
                let name: Option<String> = row.get(2)?;
                Ok(SearchDocument {
                    path: format!(
                        "{}/{}/{}",
                        row.get::<_, String>(0)?,
                        artifact_type,
                        name.unwrap_or_else(|| artifact_fallback_name(&artifact_type, &uuid))
                    ),
                    artifact_uuid: Some(uuid),
                    body: row.get(4)?,
                })
            })?;
            documents.extend(collect_rows(rows)?);
        }
        {
            let mut statement = connection.prepare(
                "SELECT tasks.id, artifacts.type, artifacts.name, artifacts.uuid,
                        annotations.uuid, annotations.body
                 FROM annotations
                 JOIN artifacts ON artifacts.uuid = annotations.artifact_uuid
                 JOIN tasks ON tasks.uuid = artifacts.task_uuid
                 WHERE annotations.body IS NOT NULL
                 ORDER BY tasks.id, artifacts.created_at, annotations.created_at, annotations.uuid",
            )?;
            let rows = statement.query_map([], |row| {
                let artifact_type: String = row.get(1)?;
                let artifact_uuid: String = row.get(3)?;
                let name: Option<String> = row.get(2)?;
                Ok(SearchDocument {
                    path: format!(
                        "{}/{}/{}/annotations/{}.md",
                        row.get::<_, String>(0)?,
                        artifact_type,
                        name.unwrap_or_else(|| artifact_fallback_name(
                            &artifact_type,
                            &artifact_uuid
                        )),
                        row.get::<_, String>(4)?
                    ),
                    artifact_uuid: Some(artifact_uuid),
                    body: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                })
            })?;
            documents.extend(collect_rows(rows)?);
        }
        Ok(documents)
    }

    fn edit_task(&self, key: &str, id: Option<&str>, body: Option<&str>) -> Result<()> {
        validate_non_empty("task identifier", key)?;
        if let Some(id) = id {
            validate_non_empty("task id", id)?;
        }
        if id.is_none() && body.is_none() {
            return Err(invalid("task edit requires an id or body"));
        }

        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let task = read_task_from_connection(&transaction, key)?;
        ensure_task_writable(&task)?;
        let id = id.unwrap_or(&task.id);
        let body = body.unwrap_or(&task.body);

        if id != task.id {
            let duplicate = transaction
                .query_row(
                    "SELECT 1 FROM tasks WHERE id = ?1 AND uuid != ?2",
                    params![id, task.uuid],
                    |_| Ok(()),
                )
                .optional()?;
            if duplicate.is_some() {
                return Err(invalid(format!("task id already exists: {id}")));
            }
            if let Some(normalized_id) = normalized_uuid(id) {
                let collision = transaction
                    .query_row(
                        "SELECT 1 FROM tasks WHERE uuid = ?1",
                        [normalized_id],
                        |_| Ok(()),
                    )
                    .optional()?;
                if collision.is_some() {
                    return Err(invalid(format!(
                        "task id conflicts with an existing task UUID: {id}"
                    )));
                }
            }
        }

        transaction.execute(
            "UPDATE tasks SET id = ?2, body = ?3, updated_at = ?4
             WHERE uuid = ?1 AND archived_at IS NULL AND completed_at IS NULL",
            params![task.uuid, id, body, now()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn complete_task(&self, key: &str) -> Result<()> {
        let task = self.read_task(key)?;
        if task.archived_at.is_some() {
            return Err(invalid(format!(
                "archived task cannot be completed: {}",
                task.id
            )));
        }
        if task.completed_at.is_some() {
            return Ok(());
        }
        let timestamp = now();
        let changed = self.connect()?.execute(
            "UPDATE tasks SET completed_at = ?2, updated_at = ?2
             WHERE uuid = ?1 AND archived_at IS NULL AND completed_at IS NULL",
            params![task.uuid, timestamp],
        )?;
        if changed == 0 {
            let current = self.read_task(&task.uuid)?;
            if current.archived_at.is_some() {
                return Err(invalid(format!(
                    "archived task cannot be completed: {}",
                    current.id
                )));
            }
        }
        Ok(())
    }

    fn reopen_task(&self, key: &str) -> Result<()> {
        let task = self.read_task(key)?;
        if task.archived_at.is_some() {
            return Err(invalid(format!(
                "archived task cannot be reopened: {}",
                task.id
            )));
        }
        if task.completed_at.is_none() {
            return Ok(());
        }
        let timestamp = now();
        let changed = self.connect()?.execute(
            "UPDATE tasks SET completed_at = NULL, updated_at = ?2
             WHERE uuid = ?1 AND archived_at IS NULL AND completed_at IS NOT NULL",
            params![task.uuid, timestamp],
        )?;
        if changed == 0 {
            let current = self.read_task(&task.uuid)?;
            if current.archived_at.is_some() {
                return Err(invalid(format!(
                    "archived task cannot be reopened: {}",
                    current.id
                )));
            }
        }
        Ok(())
    }

    fn archive_task(&self, key: &str) -> Result<()> {
        let task = self.read_task(key)?;
        if task.archived_at.is_some() {
            return Ok(());
        }
        let timestamp = now();
        let changed = self.connect()?.execute(
            "UPDATE tasks SET archived_at = ?2, updated_at = ?2 WHERE uuid = ?1",
            params![task.uuid, timestamp],
        )?;
        if changed == 0 {
            return Err(not_found(format!("task not found: {key}")));
        }
        Ok(())
    }

    fn delete_task(&self, key: &str) -> Result<()> {
        let task = self.read_task(key)?;
        let changed = self
            .connect()?
            .execute("DELETE FROM tasks WHERE uuid = ?1", [task.uuid])?;
        if changed == 0 {
            return Err(not_found(format!("task not found: {key}")));
        }
        Ok(())
    }

    fn create_artifact(&self, task_key: &str, artifact_type: &str, body: &str) -> Result<Artifact> {
        self.create_artifact_with_name(task_key, artifact_type, None, body)
    }

    fn create_artifact_with_name(
        &self,
        task_key: &str,
        artifact_type: &str,
        name: Option<&str>,
        body: &str,
    ) -> Result<Artifact> {
        validate_non_empty("artifact type", artifact_type)?;
        if let Some(name) = name {
            validate_non_empty("artifact name", name)?;
        }
        let task = self.read_task(task_key)?;
        ensure_task_writable(&task)?;
        let timestamp = now();
        let uuid = new_uuid();
        let name = name
            .map(str::to_owned)
            .or_else(|| Some(artifact_fallback_name(artifact_type, &uuid)));
        let artifact = Artifact {
            uuid,
            task_uuid: task.uuid,
            artifact_type: artifact_type.to_owned(),
            name,
            body: body.to_owned(),
            created_at: timestamp.clone(),
            updated_at: timestamp,
        };
        let changed = self.connect()?.execute(
            "INSERT INTO artifacts(uuid, task_uuid, type, name, body, created_at, updated_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7
             WHERE EXISTS(
                 SELECT 1 FROM tasks
                 WHERE uuid = ?2 AND archived_at IS NULL AND completed_at IS NULL
             )",
            params![
                artifact.uuid,
                artifact.task_uuid,
                artifact.artifact_type,
                artifact.name,
                artifact.body,
                artifact.created_at,
                artifact.updated_at
            ],
        )?;
        if changed == 0 {
            ensure_task_writable(&self.read_task(&artifact.task_uuid)?)?;
            return Err(invalid("task state changed while creating artifact"));
        }
        Ok(artifact)
    }

    fn read_artifact(&self, uuid: &str) -> Result<Artifact> {
        let normalized = require_uuid(uuid)?;
        self.connect()?
            .query_row(
                "SELECT uuid, task_uuid, type, name, body, created_at, updated_at
                 FROM artifacts WHERE uuid = ?1",
                [normalized],
                artifact_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found(format!("artifact not found: {uuid}")))
    }

    fn update_artifact(&self, uuid: &str, body: &str) -> Result<()> {
        let artifact = self.read_artifact(uuid)?;
        ensure_task_writable(&self.read_task(&artifact.task_uuid)?)?;
        let changed = self.connect()?.execute(
            "UPDATE artifacts SET body = ?2, updated_at = ?3
             WHERE uuid = ?1 AND EXISTS(
                 SELECT 1 FROM tasks
                 WHERE tasks.uuid = artifacts.task_uuid
                   AND archived_at IS NULL AND completed_at IS NULL
             )",
            params![artifact.uuid, body, now()],
        )?;
        if changed == 0 {
            let current = self.read_artifact(&artifact.uuid)?;
            ensure_task_writable(&self.read_task(&current.task_uuid)?)?;
            return Err(not_found(format!("artifact not found: {uuid}")));
        }
        Ok(())
    }

    fn rename_artifact(&self, uuid: &str, name: &str) -> Result<()> {
        let artifact = self.read_artifact(uuid)?;
        ensure_task_writable(&self.read_task(&artifact.task_uuid)?)?;
        validate_non_empty("artifact name", name)?;
        let changed = self.connect()?.execute(
            "UPDATE artifacts SET name = ?2, updated_at = ?3
             WHERE uuid = ?1 AND EXISTS(
                 SELECT 1 FROM tasks
                 WHERE tasks.uuid = artifacts.task_uuid
                   AND archived_at IS NULL AND completed_at IS NULL
             )",
            params![artifact.uuid, name, now()],
        )?;
        if changed == 0 {
            let current = self.read_artifact(&artifact.uuid)?;
            ensure_task_writable(&self.read_task(&current.task_uuid)?)?;
            return Err(not_found(format!("artifact not found: {uuid}")));
        }
        Ok(())
    }

    fn list_artifacts(&self, task_key: &str, artifact_type: Option<&str>) -> Result<Vec<Artifact>> {
        if let Some(value) = artifact_type {
            validate_non_empty("artifact type", value)?;
        }
        let task = self.read_task(task_key)?;
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT uuid, task_uuid, type, name, body, created_at, updated_at
             FROM artifacts
             WHERE task_uuid = ?1 AND (?2 IS NULL OR type = ?2)
             ORDER BY created_at, uuid",
        )?;
        let rows = statement.query_map(params![task.uuid, artifact_type], artifact_from_row)?;
        collect_rows(rows)
    }

    fn create_annotation(&self, artifact_uuid: &str, input: NewAnnotation) -> Result<Annotation> {
        let artifact = self.read_artifact(artifact_uuid)?;
        ensure_task_writable(&self.read_task(&artifact.task_uuid)?)?;
        validate_offsets(input.start_offset, input.end_offset)?;
        let annotation = Annotation {
            uuid: new_uuid(),
            artifact_uuid: artifact.uuid,
            kind: input.kind,
            start_offset: input.start_offset,
            end_offset: input.end_offset,
            selected_text: normalize_optional(input.selected_text),
            body: normalize_optional(input.body),
            created_at: now(),
            resolved_at: None,
        };
        let changed = self.connect()?.execute(
            "INSERT INTO annotations(
                uuid, artifact_uuid, kind, start_offset, end_offset, selected_text, body,
                created_at, resolved_at
             )
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL
             WHERE EXISTS(
                 SELECT 1 FROM artifacts
                 JOIN tasks ON tasks.uuid = artifacts.task_uuid
                 WHERE artifacts.uuid = ?2
                   AND tasks.archived_at IS NULL AND tasks.completed_at IS NULL
             )",
            params![
                annotation.uuid,
                annotation.artifact_uuid,
                annotation.kind.as_str(),
                annotation.start_offset,
                annotation.end_offset,
                annotation.selected_text,
                annotation.body,
                annotation.created_at
            ],
        )?;
        if changed == 0 {
            let current = self.read_artifact(&annotation.artifact_uuid)?;
            ensure_task_writable(&self.read_task(&current.task_uuid)?)?;
            return Err(invalid("task state changed while creating annotation"));
        }
        Ok(annotation)
    }

    fn list_annotations(
        &self,
        artifact_uuid: &str,
        include_resolved: bool,
    ) -> Result<Vec<Annotation>> {
        let artifact = self.read_artifact(artifact_uuid)?;
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT uuid, artifact_uuid, kind, start_offset, end_offset, selected_text,
                    body, created_at, resolved_at
             FROM annotations
             WHERE artifact_uuid = ?1 AND (?2 OR resolved_at IS NULL)
             ORDER BY created_at, uuid",
        )?;
        let rows = statement.query_map(
            params![artifact.uuid, include_resolved],
            annotation_from_row,
        )?;
        collect_rows(rows)
    }

    fn resolve_annotation(&self, uuid: &str) -> Result<()> {
        let normalized = require_uuid(uuid)?;
        let connection = self.connect()?;
        let task_uuid = connection
            .query_row(
                "SELECT artifacts.task_uuid FROM annotations
                 JOIN artifacts ON artifacts.uuid = annotations.artifact_uuid
                 WHERE annotations.uuid = ?1",
                [&normalized],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| not_found(format!("annotation not found: {uuid}")))?;
        ensure_task_writable(&self.read_task(&task_uuid)?)?;
        let changed = connection.execute(
            "UPDATE annotations SET resolved_at = COALESCE(resolved_at, ?2)
             WHERE uuid = ?1 AND EXISTS(
                 SELECT 1 FROM artifacts
                 JOIN tasks ON tasks.uuid = artifacts.task_uuid
                 WHERE artifacts.uuid = annotations.artifact_uuid
                   AND tasks.archived_at IS NULL AND tasks.completed_at IS NULL
             )",
            params![normalized, now()],
        )?;
        if changed == 0 {
            let current_task_uuid = connection
                .query_row(
                    "SELECT artifacts.task_uuid FROM annotations
                     JOIN artifacts ON artifacts.uuid = annotations.artifact_uuid
                     WHERE annotations.uuid = ?1",
                    [&normalized],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| not_found(format!("annotation not found: {uuid}")))?;
            ensure_task_writable(&self.read_task(&current_task_uuid)?)?;
            return Err(not_found(format!("annotation not found: {uuid}")));
        }
        Ok(())
    }
}

impl SqliteStore {
    fn list_tasks_with_condition(&self, condition: &str) -> Result<Vec<Task>> {
        let connection = self.connect()?;
        let sql = format!(
            "SELECT uuid, id, body, created_at, updated_at, archived_at, completed_at
             FROM tasks WHERE {condition} ORDER BY created_at, uuid"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([], task_from_row)?;
        collect_rows(rows)
    }
}

fn read_task_from_connection(connection: &Connection, key: &str) -> Result<Task> {
    let by_id = connection
        .query_row(
            "SELECT uuid, id, body, created_at, updated_at, archived_at, completed_at FROM tasks WHERE id = ?1",
            [key],
            task_from_row,
        )
        .optional()?;
    let by_uuid = match normalized_uuid(key) {
        Some(uuid) => connection
            .query_row(
                "SELECT uuid, id, body, created_at, updated_at, archived_at, completed_at FROM tasks WHERE uuid = ?1",
                [uuid],
                task_from_row,
            )
            .optional()?,
        None => None,
    };
    match (by_id, by_uuid) {
        (Some(by_id), Some(by_uuid)) if by_id.uuid != by_uuid.uuid => Err(invalid(format!(
            "ambiguous task identifier matches an id and a UUID: {key}"
        ))),
        (Some(task), _) | (None, Some(task)) => Ok(task),
        (None, None) => Err(not_found(format!("task not found: {key}"))),
    }
}

fn validate_non_empty(name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(invalid(format!("{name} must not be empty")));
    }
    Ok(())
}

fn validate_offsets(start: Option<u64>, end: Option<u64>) -> Result<()> {
    match (start, end) {
        (None, None) => Ok(()),
        (Some(start), Some(end)) if end >= start => Ok(()),
        (Some(_), Some(_)) => Err(invalid("end offset must not be less than start offset")),
        _ => Err(invalid("start and end offsets must be supplied together")),
    }
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.is_empty())
}

fn ensure_task_writable(task: &Task) -> Result<()> {
    if task.archived_at.is_some() {
        return Err(invalid(format!("archived task is read-only: {}", task.id)));
    }
    if task.completed_at.is_some() {
        return Err(invalid(format!("completed task is read-only: {}", task.id)));
    }
    Ok(())
}

fn normalized_uuid(value: &str) -> Option<String> {
    uuid::Uuid::parse_str(value)
        .ok()
        .map(|uuid| uuid.to_string())
}

fn require_uuid(value: &str) -> Result<String> {
    normalized_uuid(value).ok_or_else(|| invalid(format!("invalid UUID: {value}")))
}

fn task_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        uuid: row.get(0)?,
        id: row.get(1)?,
        body: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
        archived_at: row.get(5)?,
        completed_at: row.get(6)?,
    })
}

fn artifact_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Artifact> {
    Ok(Artifact {
        uuid: row.get(0)?,
        task_uuid: row.get(1)?,
        artifact_type: row.get(2)?,
        name: row.get(3)?,
        body: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn annotation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Annotation> {
    let kind: String = row.get(2)?;
    let kind = <AnnotationKind as FromStr>::from_str(&kind).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                error.to_string(),
            )),
        )
    })?;
    Ok(Annotation {
        uuid: row.get(0)?,
        artifact_uuid: row.get(1)?,
        kind,
        start_offset: row.get(3)?,
        end_offset: row.get(4)?,
        selected_text: row.get(5)?,
        body: row.get(6)?,
        created_at: row.get(7)?,
        resolved_at: row.get(8)?,
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}
