use std::{
    borrow::Cow,
    collections::{BTreeMap, HashSet},
    error::Error as StdError,
    fmt, fs,
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{
    Json, Router,
    extract::{Path as AxumPath, Request, State},
    http::{Method, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use chrono::Utc;
use clap::ValueEnum;
use directories::BaseDirs;
use pulldown_cmark::{Options, Parser, html};
use rand::{RngCore, rngs::OsRng};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const SCHEMA: &str = include_str!("schema.sql");
const INDEX_HTML: &str = include_str!("web/index.html");
const LOGIN_HTML: &str = include_str!("web/login.html");
pub const AGENT_SKILL: &str = include_str!("skill.md");

pub mod service;

#[derive(Clone, Debug)]
pub struct App {
    database_path: Arc<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Task {
    pub uuid: String,
    pub id: String,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Artifact {
    pub uuid: String,
    pub task_uuid: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub name: Option<String>,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Artifact {
    pub fn display_name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| artifact_fallback_name(&self.artifact_type, &self.uuid))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Annotation {
    pub uuid: String,
    pub artifact_uuid: String,
    pub kind: AnnotationKind,
    pub start_offset: Option<u64>,
    pub end_offset: Option<u64>,
    pub selected_text: Option<String>,
    pub body: Option<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
#[serde(rename_all = "lowercase")]
#[value(rename_all = "lower")]
pub enum AnnotationKind {
    Comment,
    Question,
    Scratch,
    Good,
}

impl AnnotationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Question => "question",
            Self::Scratch => "scratch",
            Self::Good => "good",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Comment => "Comment",
            Self::Question => "Question",
            Self::Scratch => "Scratch",
            Self::Good => "Good",
        }
    }
}

impl FromStr for AnnotationKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "comment" => Ok(Self::Comment),
            "question" => Ok(Self::Question),
            "scratch" => Ok(Self::Scratch),
            "good" => Ok(Self::Good),
            _ => bail!("unsupported annotation kind: {value}"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewAnnotation {
    pub kind: AnnotationKind,
    pub start_offset: Option<u64>,
    pub end_offset: Option<u64>,
    pub selected_text: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Serialize)]
struct TaskView {
    task: Task,
    artifacts: Vec<Artifact>,
    rendered_task_html: String,
}

#[derive(Debug, Serialize)]
struct ArtifactView {
    artifact: Artifact,
    rendered_html: String,
    annotations: Vec<Annotation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DomainErrorKind {
    Invalid,
    NotFound,
}

#[derive(Debug)]
struct DomainError {
    kind: DomainErrorKind,
    message: String,
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl StdError for DomainError {}

fn invalid(message: impl Into<String>) -> anyhow::Error {
    DomainError {
        kind: DomainErrorKind::Invalid,
        message: message.into(),
    }
    .into()
}

fn not_found(message: impl Into<String>) -> anyhow::Error {
    DomainError {
        kind: DomainErrorKind::NotFound,
        message: message.into(),
    }
    .into()
}

impl App {
    pub fn from_env() -> Result<Self> {
        let path = match std::env::var_os("ALX_DB") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => default_database_path()?,
        };
        Self::new(path)
    }

    pub fn new(database_path: impl Into<PathBuf>) -> Result<Self> {
        let database_path = database_path.into();
        if let Some(parent) = database_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create database directory {}", parent.display())
            })?;
        }
        let app = Self {
            database_path: Arc::new(database_path),
        };
        app.migrate()?;
        Ok(app)
    }

    pub fn database_path(&self) -> &Path {
        self.database_path.as_path()
    }

    /// The password hash is kept outside SQLite so the existing storage schema stays unchanged.
    pub fn password_path(&self) -> PathBuf {
        match std::env::var_os("ALX_AUTH_FILE") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => self
                .database_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("password.hash"),
        }
    }

    pub fn has_password(&self) -> Result<bool> {
        Ok(self.read_password_hash()?.is_some())
    }

    pub fn set_password(&self, password: &str) -> Result<()> {
        if password.is_empty() {
            bail!("password must not be empty");
        }
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|error| anyhow!("failed to hash password: {error}"))?
            .to_string();
        let path = self.password_path();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create password directory {}", parent.display())
            })?;
        }
        let temporary = path.with_file_name(format!(".password-{}.tmp", new_uuid()));
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .with_context(|| format!("failed to create {}", temporary.display()))?;
            set_private_file_mode(&file)?;
            file.write_all(hash.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temporary, &path)
                .with_context(|| format!("failed to replace password hash {}", path.display()))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub fn clear_password(&self) -> Result<()> {
        match fs::remove_file(self.password_path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn read_password_hash(&self) -> Result<Option<String>> {
        match fs::read_to_string(self.password_path()) {
            Ok(contents) => {
                let hash = contents.trim();
                if hash.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(hash.to_owned()))
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn connect(&self) -> Result<Connection> {
        let connection = Connection::open(self.database_path.as_path()).with_context(|| {
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

    pub fn create_task(&self, id: &str, body: &str) -> Result<Task> {
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

    pub fn read_task(&self, key: &str) -> Result<Task> {
        validate_non_empty("task identifier", key)?;
        let connection = self.connect()?;
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

    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        self.list_tasks_with_condition("archived_at IS NULL AND completed_at IS NULL")
    }

    pub fn list_completed_tasks(&self) -> Result<Vec<Task>> {
        self.list_tasks_with_condition("archived_at IS NULL AND completed_at IS NOT NULL")
    }

    pub fn list_archived_tasks(&self) -> Result<Vec<Task>> {
        self.list_tasks_with_condition("archived_at IS NOT NULL")
    }

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

    pub fn search_tasks(&self, query: &str) -> Result<Vec<Task>> {
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

    pub fn update_task(&self, key: &str, body: &str) -> Result<()> {
        let task = self.read_task(key)?;
        ensure_task_writable(&task)?;
        let changed = self.connect()?.execute(
            "UPDATE tasks SET body = ?2, updated_at = ?3
             WHERE uuid = ?1 AND archived_at IS NULL AND completed_at IS NULL",
            params![task.uuid, body, now()],
        )?;
        if changed == 0 {
            ensure_task_writable(&self.read_task(&task.uuid)?)?;
            return Err(not_found(format!("task not found: {key}")));
        }
        Ok(())
    }

    pub fn complete_task(&self, key: &str) -> Result<()> {
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

    pub fn reopen_task(&self, key: &str) -> Result<()> {
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

    pub fn archive_task(&self, key: &str) -> Result<()> {
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

    pub fn delete_task(&self, key: &str) -> Result<()> {
        let task = self.read_task(key)?;
        let changed = self
            .connect()?
            .execute("DELETE FROM tasks WHERE uuid = ?1", [task.uuid])?;
        if changed == 0 {
            return Err(not_found(format!("task not found: {key}")));
        }
        Ok(())
    }

    pub fn create_artifact(
        &self,
        task_key: &str,
        artifact_type: &str,
        body: &str,
    ) -> Result<Artifact> {
        self.create_artifact_with_name(task_key, artifact_type, None, body)
    }

    pub fn create_artifact_with_name(
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

    pub fn read_artifact(&self, uuid: &str) -> Result<Artifact> {
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

    pub fn update_artifact(&self, uuid: &str, body: &str) -> Result<()> {
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

    pub fn rename_artifact(&self, uuid: &str, name: &str) -> Result<()> {
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

    pub fn list_artifacts(
        &self,
        task_key: &str,
        artifact_type: Option<&str>,
    ) -> Result<Vec<Artifact>> {
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

    pub fn create_annotation(
        &self,
        artifact_uuid: &str,
        input: NewAnnotation,
    ) -> Result<Annotation> {
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

    pub fn list_annotations(
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

    pub fn resolve_annotation(&self, uuid: &str) -> Result<()> {
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

    pub fn feedback_markdown(&self, artifact_uuid: &str) -> Result<String> {
        self.feedback_markdown_at_level(artifact_uuid, 1)
    }

    pub fn review_markdown(&self, artifact_uuid: &str) -> Result<String> {
        let artifact = self.read_artifact(artifact_uuid)?;
        let feedback = self.feedback_markdown_at_level(&artifact.uuid, 2)?;
        Ok(format!(
            "# Artifact Review\n\nArtifact UUID: {}\n\nYou must review and address all unresolved feedback below.\n\nRules:\n- Read the current artifact before making changes.\n- Address every unresolved `comment`, `question`, and `scratch`.\n- Treat `good` annotations as guidance to preserve that part unless conflicting feedback requires otherwise.\n- Do not resolve annotations until their feedback has been addressed.\n- Update the existing artifact instead of creating a replacement unless explicitly requested.\n\n{}",
            artifact.uuid, feedback
        ))
    }

    fn feedback_markdown_at_level(
        &self,
        artifact_uuid: &str,
        heading_level: usize,
    ) -> Result<String> {
        let annotations = self.list_annotations(artifact_uuid, false)?;
        let mut grouped: BTreeMap<AnnotationKind, Vec<Annotation>> = BTreeMap::new();
        for annotation in annotations {
            grouped.entry(annotation.kind).or_default().push(annotation);
        }
        let mut output = format!("{} Feedback\n", "#".repeat(heading_level));
        for (kind, entries) in grouped {
            output.push('\n');
            output.push_str(&"#".repeat(heading_level + 1));
            output.push(' ');
            output.push_str(kind.title());
            output.push('\n');
            for entry in entries {
                if let Some(selected_text) = entry.selected_text {
                    output.push('\n');
                    for line in selected_text.lines() {
                        output.push_str("> ");
                        output.push_str(line);
                        output.push('\n');
                    }
                }
                if let Some(body) = entry.body {
                    output.push('\n');
                    output.push_str(&body);
                    output.push('\n');
                }
            }
        }
        Ok(output)
    }

    pub fn context_markdown(&self, task_key: &str) -> Result<String> {
        let task = self.read_task(task_key)?;
        let artifacts = self.list_artifacts(&task.uuid, None)?;
        let mut output = format!(
            "# Task: {}\n\n{}\n\n## Agent instructions\n\nWhen creating an artifact, provide a short descriptive filename with `--name` when there is an obvious one. Use the artifact UUID for all subsequent operations.\n",
            markdown_inline(&task.id),
            task.body
        );
        if !artifacts.is_empty() {
            output.push_str("\n## Artifacts\n");
            for artifact in artifacts {
                output.push_str(&format!(
                    "\n### {} ({})\n\n{}\n",
                    markdown_inline(&artifact.artifact_type),
                    artifact.uuid,
                    artifact.body
                ));
            }
        }
        Ok(output)
    }

    pub fn dump(&self, target: &Path, task_key: Option<&str>, zip: bool) -> Result<()> {
        let tasks = match task_key {
            Some(key) => vec![self.read_task(key)?],
            None => {
                let mut tasks = self.list_tasks()?;
                tasks.extend(self.list_completed_tasks()?);
                tasks.extend(self.list_archived_tasks()?);
                tasks
            }
        };
        let mut files: Vec<(String, String)> = Vec::new();
        let mut used_task_dirs: Vec<String> = Vec::new();
        for task in tasks {
            let task_dir = unique_component(&task.id, &task.uuid, &mut used_task_dirs);
            files.push((format!("{task_dir}/task.md"), task.body.clone()));
            let artifacts = self.list_artifacts(&task.uuid, None)?;
            let mut artifacts_by_type: BTreeMap<&str, Vec<&Artifact>> = BTreeMap::new();
            for artifact in &artifacts {
                artifacts_by_type
                    .entry(artifact.artifact_type.as_str())
                    .or_default()
                    .push(artifact);
            }
            for (artifact_type, artifacts) in artifacts_by_type {
                let type_dir = format!("{task_dir}/{}", safe_component(artifact_type));
                let mut used_names: Vec<String> = Vec::new();
                for artifact in artifacts {
                    let name =
                        unique_component(&artifact.display_name(), &artifact.uuid, &mut used_names);
                    files.push((format!("{type_dir}/{name}"), artifact.body.clone()));
                }
            }
        }
        if zip {
            write_zip(target, &files)?;
        } else {
            write_directory(target, &files)?;
        }
        Ok(())
    }

    pub fn migration_versions(&self) -> Result<Vec<i64>> {
        let connection = self.connect()?;
        let mut statement =
            connection.prepare("SELECT version FROM schema_migrations ORDER BY version")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        collect_rows(rows)
    }
}

pub fn default_database_path() -> Result<PathBuf> {
    let dirs =
        BaseDirs::new().ok_or_else(|| anyhow!("could not determine the user data directory"))?;
    Ok(dirs.data_dir().join("alx").join("alx.db"))
}

pub fn default_skill_path() -> Result<PathBuf> {
    let dirs = BaseDirs::new().ok_or_else(|| anyhow!("could not determine the home directory"))?;
    Ok(dirs
        .home_dir()
        .join(".agents")
        .join("skills")
        .join("alx")
        .join("SKILL.md"))
}

pub fn install_skill() -> Result<PathBuf> {
    let path = default_skill_path()?;
    let directory = path
        .parent()
        .ok_or_else(|| anyhow!("agent skill path has no parent directory"))?;
    fs::create_dir_all(directory).with_context(|| {
        format!(
            "failed to create agent skill directory {}",
            directory.display()
        )
    })?;
    fs::write(&path, AGENT_SKILL)
        .with_context(|| format!("failed to install agent skill at {}", path.display()))?;
    Ok(path)
}

pub fn parse_bind_address(bind: Option<&str>, tailscale: bool) -> Result<SocketAddr> {
    if bind.is_some() && tailscale {
        bail!("--bind cannot be used with --tailscale");
    }
    if tailscale {
        return tailscale_address();
    }
    bind.unwrap_or("127.0.0.1:3000")
        .parse()
        .context("invalid bind address; expected IP:PORT")
}

fn tailscale_address() -> Result<SocketAddr> {
    tailscale_address_with(Path::new("tailscale"))
}

fn tailscale_address_with(program: &Path) -> Result<SocketAddr> {
    let output = Command::new(program)
        .args(["ip", "-4"])
        .output()
        .context("failed to run 'tailscale ip -4'")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("'tailscale ip -4' failed: {}", stderr.trim());
    }
    let stdout = String::from_utf8(output.stdout).context("tailscale returned non-UTF-8 output")?;
    let ip = stdout
        .lines()
        .map(str::trim)
        .find_map(|line| line.parse::<Ipv4Addr>().ok())
        .ok_or_else(|| anyhow!("'tailscale ip -4' returned no valid IPv4 address"))?;
    Ok(SocketAddr::new(IpAddr::V4(ip), 3000))
}

#[derive(Clone)]
struct WebState {
    app: App,
    auth: Option<Arc<AuthState>>,
}

impl std::ops::Deref for WebState {
    type Target = App;

    fn deref(&self) -> &Self::Target {
        &self.app
    }
}

struct AuthState {
    app: App,
    sessions: Mutex<SessionState>,
}

struct SessionState {
    /// The hash active when this session set was last synchronized.
    password_hash: Option<String>,
    tokens: HashSet<String>,
}

pub fn router(app: App) -> Router {
    build_router(WebState { app, auth: None })
}

/// Build a web router with authentication enabled. Loopback servers use `router` instead.
pub fn router_with_auth(app: App) -> Result<Router> {
    Ok(build_router(WebState {
        app: app.clone(),
        auth: Some(Arc::new(AuthState {
            app,
            sessions: Mutex::new(SessionState {
                password_hash: None,
                tokens: HashSet::new(),
            }),
        })),
    }))
}

fn build_router(state: WebState) -> Router {
    let authentication = state.auth.is_some();
    let router = Router::new()
        .route("/", get(index))
        .route("/login", get(login_page))
        .route("/api/login", post(api_login))
        .route("/api/tasks", get(api_tasks).post(api_create_task))
        .route("/api/completed-tasks", get(api_completed_tasks))
        .route("/api/archived-tasks", get(api_archived_tasks))
        .route(
            "/api/tasks/{key}",
            get(api_task).put(api_update_task).delete(api_delete_task),
        )
        .route("/api/tasks/{key}/complete", post(api_complete_task))
        .route("/api/tasks/{key}/reopen", post(api_reopen_task))
        .route("/api/tasks/{key}/archive", post(api_archive_task))
        .route("/api/tasks/{key}/artifacts", post(api_create_artifact))
        .route("/api/artifacts/{uuid}", get(api_artifact))
        .route(
            "/api/artifacts/{uuid}/annotations",
            post(api_create_annotation),
        )
        .route(
            "/api/annotations/{uuid}/resolve",
            post(api_resolve_annotation),
        );
    let router = router.layer(middleware::from_fn(validate_browser_request));
    let router = if authentication {
        router.layer(middleware::from_fn_with_state(state.clone(), authenticate))
    } else {
        router
    };
    router.with_state(state)
}

pub async fn serve(app: App, address: SocketAddr) -> Result<()> {
    if !address.ip().is_loopback() && !app.has_password()? {
        bail!("non-loopback serving requires a password; set one with `alx serve password set`");
    }
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind {address}"))?;
    let local_address = listener.local_addr()?;
    eprintln!("listening on http://{local_address}");
    let router = if address.ip().is_loopback() {
        router(app)
    } else {
        router_with_auth(app)?
    };
    axum::serve(listener, router).await?;
    Ok(())
}

async fn index() -> impl IntoResponse {
    (
        [(
            header::CONTENT_SECURITY_POLICY,
            "default-src 'self'; base-uri 'none'; connect-src 'self'; frame-ancestors 'none'; img-src 'none'; object-src 'none'; script-src 'self' 'unsafe-inline' https://cdn.jsdelivr.net; style-src 'unsafe-inline' https://cdn.jsdelivr.net",
        )],
        Html(INDEX_HTML),
    )
}

async fn login_page() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; base-uri 'none'; connect-src 'self'; frame-ancestors 'none'; img-src 'none'; object-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'unsafe-inline'",
            ),
        ],
        Html(LOGIN_HTML),
    )
}

#[derive(Debug, Deserialize)]
struct LoginInput {
    password: String,
}

async fn api_login(State(state): State<WebState>, Json(input): Json<LoginInput>) -> Response {
    let Some(auth) = state.auth else {
        return (StatusCode::NOT_FOUND, "authentication is not enabled").into_response();
    };
    let hash = match auth.app.read_password_hash() {
        Ok(Some(hash)) => hash,
        Ok(None) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "password authentication is not configured",
            )
                .into_response();
        }
        Err(error) => {
            eprintln!("failed to read password hash: {error:#}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal server error").into_response();
        }
    };
    let valid = PasswordHash::new(&hash).ok().is_some_and(|parsed| {
        Argon2::default()
            .verify_password(input.password.as_bytes(), &parsed)
            .is_ok()
    });
    if !valid {
        return (StatusCode::UNAUTHORIZED, "invalid password").into_response();
    }
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let token = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut sessions = auth.sessions.lock().expect("session mutex poisoned");
    synchronize_sessions(&mut sessions, Some(&hash));
    sessions.tokens.insert(token.clone());
    let cookie = format!("alx_session={token}; Path=/; HttpOnly; SameSite=Strict");
    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response()
}

fn synchronize_sessions(state: &mut SessionState, current_hash: Option<&str>) {
    if state.password_hash.as_deref() != current_hash {
        state.password_hash = current_hash.map(str::to_owned);
        state.tokens.clear();
    }
}

async fn authenticate(State(state): State<WebState>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if path == "/login" || path == "/api/login" {
        return next.run(request).await;
    }
    let current_hash = state
        .auth
        .as_ref()
        .and_then(|auth| auth.app.read_password_hash().ok().flatten());
    let configured = current_hash.is_some();
    let authorized = state.auth.as_ref().is_some_and(|auth| {
        let mut sessions = auth.sessions.lock().expect("session mutex poisoned");
        // Password changes happen in a separate CLI process. Compare the current
        // hash on every request so rotation and clearing revoke old cookies.
        synchronize_sessions(&mut sessions, current_hash.as_deref());
        if !configured {
            return false;
        }
        let Some(cookie) = request
            .headers()
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
        else {
            return false;
        };
        let Some(token) = cookie.split(';').find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            (name == "alx_session").then_some(value)
        }) else {
            return false;
        };
        sessions.tokens.contains(token)
    });
    if authorized {
        next.run(request).await
    } else if configured && request.method() == Method::GET && path == "/" {
        Redirect::to("/login").into_response()
    } else if !configured {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "password authentication is not configured",
        )
            .into_response()
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Cookie realm=alx")],
            "authentication required",
        )
            .into_response()
    }
}

async fn validate_browser_request(request: Request, next: Next) -> Response {
    if let Some(host) = request.headers().get(header::HOST) {
        let Ok(host) = host.to_str() else {
            return forbidden("invalid Host header");
        };
        if !valid_host(host) {
            return forbidden("Host must be an IP address or localhost");
        }
        if let Some(origin) = request.headers().get(header::ORIGIN) {
            let Ok(origin) = origin.to_str() else {
                return forbidden("invalid Origin header");
            };
            if !same_origin(origin, host) {
                return forbidden("cross-origin requests are not allowed");
            }
        }
    }
    next.run(request).await
}

fn forbidden(message: &'static str) -> Response {
    (
        StatusCode::FORBIDDEN,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        message,
    )
        .into_response()
}

fn valid_host(authority: &str) -> bool {
    authority
        .parse::<axum::http::uri::Authority>()
        .is_ok_and(|authority| {
            authority.host().eq_ignore_ascii_case("localhost")
                || authority.host().parse::<IpAddr>().is_ok()
        })
}

fn same_origin(origin: &str, host: &str) -> bool {
    let Ok(uri) = origin.parse::<Uri>() else {
        return false;
    };
    matches!(uri.scheme_str(), Some("http") | Some("https"))
        && uri
            .authority()
            .is_some_and(|authority| authority.as_str().eq_ignore_ascii_case(host))
}

async fn api_tasks(State(app): State<WebState>) -> ApiResult<Json<Vec<Task>>> {
    Ok(Json(app.list_tasks()?))
}

#[derive(Debug, Deserialize)]
struct NewTaskInput {
    id: String,
    body: String,
}

async fn api_create_task(
    State(app): State<WebState>,
    Json(input): Json<NewTaskInput>,
) -> ApiResult<(StatusCode, Json<Task>)> {
    let task = app.create_task(&input.id, &input.body)?;
    Ok((StatusCode::CREATED, Json(task)))
}

async fn api_completed_tasks(State(app): State<WebState>) -> ApiResult<Json<Vec<Task>>> {
    Ok(Json(app.list_completed_tasks()?))
}

async fn api_archived_tasks(State(app): State<WebState>) -> ApiResult<Json<Vec<Task>>> {
    Ok(Json(app.list_archived_tasks()?))
}

async fn api_task(
    State(app): State<WebState>,
    AxumPath(key): AxumPath<String>,
) -> ApiResult<Json<TaskView>> {
    let task = app.read_task(&key)?;
    let artifacts = app.list_artifacts(&task.uuid, None)?;
    let rendered_task_html = render_markdown(&task.body);
    Ok(Json(TaskView {
        task,
        artifacts,
        rendered_task_html,
    }))
}

#[derive(Debug, Deserialize)]
struct DeleteConfirmation {
    confirm: bool,
}

#[derive(Debug, Deserialize)]
struct TaskUpdateInput {
    body: String,
}

async fn api_update_task(
    State(app): State<WebState>,
    AxumPath(key): AxumPath<String>,
    Json(input): Json<TaskUpdateInput>,
) -> ApiResult<StatusCode> {
    app.update_task(&key, &input.body)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn api_complete_task(
    State(app): State<WebState>,
    AxumPath(key): AxumPath<String>,
    Json(_body): Json<serde_json::Value>,
) -> ApiResult<StatusCode> {
    app.complete_task(&key)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn api_reopen_task(
    State(app): State<WebState>,
    AxumPath(key): AxumPath<String>,
    Json(_body): Json<serde_json::Value>,
) -> ApiResult<StatusCode> {
    app.reopen_task(&key)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn api_archive_task(
    State(app): State<WebState>,
    AxumPath(key): AxumPath<String>,
    Json(_body): Json<serde_json::Value>,
) -> ApiResult<StatusCode> {
    app.archive_task(&key)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn api_delete_task(
    State(app): State<WebState>,
    AxumPath(key): AxumPath<String>,
    Json(input): Json<DeleteConfirmation>,
) -> ApiResult<StatusCode> {
    if !input.confirm {
        return Err(invalid("task deletion requires explicit confirmation").into());
    }
    app.delete_task(&key)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct NewArtifactInput {
    #[serde(rename = "type")]
    artifact_type: String,
    name: Option<String>,
    body: String,
}

async fn api_create_artifact(
    State(app): State<WebState>,
    AxumPath(key): AxumPath<String>,
    Json(input): Json<NewArtifactInput>,
) -> ApiResult<(StatusCode, Json<Artifact>)> {
    let artifact = app.create_artifact_with_name(
        &key,
        &input.artifact_type,
        input.name.as_deref(),
        &input.body,
    )?;
    Ok((StatusCode::CREATED, Json(artifact)))
}

async fn api_artifact(
    State(app): State<WebState>,
    AxumPath(uuid): AxumPath<String>,
) -> ApiResult<Json<ArtifactView>> {
    let artifact = app.read_artifact(&uuid)?;
    let annotations = app.list_annotations(&uuid, false)?;
    let rendered_html = render_markdown(&artifact.body);
    Ok(Json(ArtifactView {
        artifact,
        rendered_html,
        annotations,
    }))
}

async fn api_create_annotation(
    State(app): State<WebState>,
    AxumPath(uuid): AxumPath<String>,
    Json(input): Json<NewAnnotation>,
) -> ApiResult<(StatusCode, Json<Annotation>)> {
    let annotation = app.create_annotation(&uuid, input)?;
    Ok((StatusCode::CREATED, Json(annotation)))
}

async fn api_resolve_annotation(
    State(app): State<WebState>,
    AxumPath(uuid): AxumPath<String>,
    Json(_body): Json<serde_json::Value>,
) -> ApiResult<StatusCode> {
    app.resolve_annotation(&uuid)?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn render_markdown(markdown: &str) -> String {
    let parser = Parser::new_ext(markdown, Options::all());
    let mut unsafe_html = String::new();
    html::push_html(&mut unsafe_html, parser);
    ammonia::Builder::default()
        .rm_tags(&["img"])
        .add_tag_attributes("code", &["class"])
        .attribute_filter(|element, attribute, value| {
            if element == "code" && attribute == "class" {
                let languages: Vec<&str> = value
                    .split_ascii_whitespace()
                    .filter(|class| class.starts_with("language-"))
                    .collect();
                return if languages.is_empty() {
                    None
                } else {
                    Some(Cow::Owned(languages.join(" ")))
                };
            }
            Some(Cow::Borrowed(value))
        })
        .clean(&unsafe_html)
        .to_string()
}

struct ApiError(anyhow::Error);
type ApiResult<T> = std::result::Result<T, ApiError>;

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(error: E) -> Self {
        Self(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let domain_error = self
            .0
            .chain()
            .find_map(|cause| cause.downcast_ref::<DomainError>());
        let (status, message) = match domain_error {
            Some(error) if error.kind == DomainErrorKind::NotFound => {
                (StatusCode::NOT_FOUND, error.to_string())
            }
            Some(error) => (StatusCode::BAD_REQUEST, error.to_string()),
            None => {
                eprintln!("HTTP request failed: {:#}", self.0);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_owned(),
                )
            }
        };
        (
            status,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            message,
        )
            .into_response()
    }
}

fn write_directory(target: &Path, files: &[(String, String)]) -> Result<()> {
    if target.exists() && !target.is_dir() {
        bail!(
            "dump target exists and is not a directory: {}",
            target.display()
        );
    }
    fs::create_dir_all(target)
        .with_context(|| format!("failed to create dump directory {}", target.display()))?;
    for (path, contents) in files {
        let file_path = target.join(path);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&file_path, contents)
            .with_context(|| format!("failed to write {}", file_path.display()))?;
    }
    Ok(())
}

fn write_zip(target: &Path, files: &[(String, String)]) -> Result<()> {
    if let Some(parent) = target.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(target)
        .with_context(|| format!("failed to create {}", target.display()))?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (path, contents) in files {
        writer
            .start_file(path.as_str(), options)
            .with_context(|| format!("failed to add {path} to archive"))?;
        writer
            .write_all(contents.as_bytes())
            .with_context(|| format!("failed to write {path} to archive"))?;
    }
    writer
        .finish()
        .with_context(|| format!("failed to finish archive {}", target.display()))?;
    Ok(())
}

fn safe_component(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control()
            || matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            )
        {
            output.push('_');
        } else {
            output.push(character);
        }
    }
    let output = output.trim().trim_end_matches(['.', ' ']).to_owned();
    if output.is_empty() {
        "unnamed".to_owned()
    } else {
        output
    }
}

fn unique_component(name: &str, uuid: &str, used: &mut Vec<String>) -> String {
    let candidate = safe_component(name);
    if is_unused(&candidate, used) {
        used.push(candidate.clone());
        return candidate;
    }
    let (stem, extension) = match candidate.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
            (stem.to_owned(), format!(".{extension}"))
        }
        _ => (candidate.clone(), String::new()),
    };
    let prefix = format!("{stem}--{}", &uuid[..8]);
    let mut attempt = format!("{prefix}{extension}");
    let mut counter = 2;
    while !is_unused(&attempt, used) {
        attempt = format!("{prefix}-{counter}{extension}");
        counter += 1;
    }
    used.push(attempt.clone());
    attempt
}

fn is_unused(candidate: &str, used: &[String]) -> bool {
    !used
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(candidate))
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn new_uuid() -> String {
    Uuid::now_v7().to_string()
}

fn normalized_uuid(value: &str) -> Option<String> {
    Uuid::parse_str(value).ok().map(|uuid| uuid.to_string())
}

fn require_uuid(value: &str) -> Result<String> {
    normalized_uuid(value).ok_or_else(|| invalid(format!("invalid UUID: {value}")))
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

fn set_private_file_mode(file: &fs::File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn markdown_inline(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\n' | '\r' => output.push(' '),
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' => {
                output.push('\\');
                output.push(character);
            }
            _ => output.push(character),
        }
    }
    output
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

fn artifact_fallback_name(artifact_type: &str, uuid: &str) -> String {
    format!("{artifact_type}--{}.md", &uuid[..8])
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

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn fake_command(script: &str) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tailscale");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        (directory, path)
    }

    #[test]
    fn render_markdown_keeps_language_classes_on_code_blocks() {
        let html = render_markdown(
            "```rust\nfn main() {}\n```\n\n```mermaid\ngraph TD\n```\n\n```\nplain\n```",
        );
        assert!(html.contains("<code class=\"language-rust\">"));
        assert!(html.contains("<code class=\"language-mermaid\">"));
        assert!(html.contains("<code>plain\n</code>"));
    }

    #[test]
    fn render_markdown_strips_foreign_classes_on_code_blocks() {
        let html = render_markdown(
            "<code class=\"language-rust evil\">x</code><span class=\"evil\">y</span>",
        );
        assert!(html.contains("<code class=\"language-rust\">x</code>"));
        assert!(!html.contains("evil"));
    }

    #[test]
    fn tailscale_selects_first_ipv4_and_passes_expected_arguments() {
        let (_directory, command) = fake_command(
            "test \"$1 $2\" = \"ip -4\" || exit 9\nprintf 'not-an-ip\\n100.64.0.8\\n100.64.0.9\\n'",
        );
        assert_eq!(
            tailscale_address_with(&command).unwrap(),
            "100.64.0.8:3000".parse().unwrap()
        );
    }

    #[test]
    fn tailscale_reports_command_and_output_failures() {
        let (_directory, command) = fake_command("echo unavailable >&2\nexit 3");
        assert!(
            tailscale_address_with(&command)
                .unwrap_err()
                .to_string()
                .contains("unavailable")
        );

        let (_directory, command) = fake_command("printf 'not-an-ip\\n'");
        assert!(
            tailscale_address_with(&command)
                .unwrap_err()
                .to_string()
                .contains("no valid IPv4")
        );
    }
}
