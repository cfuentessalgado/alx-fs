use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet},
    fmt, fs,
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{
    Json, Router,
    extract::{Path as AxumPath, Query, Request, State},
    http::{Method, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use chrono::Utc;
use directories::BaseDirs;
use pulldown_cmark::{Options, Parser, html};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const INDEX_HTML: &str = include_str!("web/index.html");
const LOGIN_HTML: &str = include_str!("web/login.html");
pub const AGENT_SKILL: &str = include_str!("skill.md");

pub mod service;

mod error;
mod model;
mod store;

use error::{DomainError, DomainErrorKind, invalid};
pub use model::{Annotation, AnnotationKind, Artifact, NewAnnotation, Task};
use store::{SqliteStore, Store};

#[derive(Clone)]
pub struct App {
    auth_path: Arc<PathBuf>,
    store: Arc<dyn Store>,
}

const REVIEW_GATE_TTL: Duration = Duration::from_secs(30 * 60);
const REVIEW_GATE_ADDRESS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000);

#[derive(Clone)]
struct ReviewGates {
    gates: Arc<Mutex<HashMap<String, ReviewGate>>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum ReviewOutcome {
    Finished,
}

struct ReviewGate {
    artifact_uuid: String,
    expires_at: Instant,
    outcome: Option<ReviewOutcome>,
}

impl Default for ReviewGates {
    fn default() -> Self {
        Self {
            gates: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl ReviewGates {
    fn create(&self, artifact_uuid: &str) -> Result<String> {
        let mut gates = self.gates.lock().expect("review gate mutex poisoned");
        gates.retain(|_, gate| gate.expires_at > Instant::now());
        let token = loop {
            let candidate = random_token();
            if !gates.contains_key(&candidate) {
                break candidate;
            }
        };
        gates.insert(
            token.clone(),
            ReviewGate {
                artifact_uuid: artifact_uuid.to_owned(),
                expires_at: Instant::now() + REVIEW_GATE_TTL,
                outcome: None,
            },
        );
        Ok(token)
    }

    fn artifact_uuid(&self, token: &str) -> Result<String> {
        let mut gates = self.gates.lock().expect("review gate mutex poisoned");
        let Some(gate) = gates.get(token) else {
            return Err(error::not_found("review gate not found or expired"));
        };
        if gate.expires_at <= Instant::now() {
            gates.remove(token);
            return Err(error::not_found("review gate not found or expired"));
        }
        Ok(gate.artifact_uuid.clone())
    }

    fn finish(&self, token: &str) -> Result<()> {
        let mut gates = self.gates.lock().expect("review gate mutex poisoned");
        let Some(gate) = gates.get_mut(token) else {
            return Err(error::not_found("review gate not found or expired"));
        };
        if gate.expires_at <= Instant::now() {
            gates.remove(token);
            return Err(error::not_found("review gate not found or expired"));
        }
        if gate.outcome.is_some() {
            return Err(invalid("review gate has already been finished"));
        }
        gate.outcome = Some(ReviewOutcome::Finished);
        Ok(())
    }

    fn outcome(&self, token: &str) -> Result<Option<ReviewOutcome>> {
        let mut gates = self.gates.lock().expect("review gate mutex poisoned");
        let Some(gate) = gates.get(token) else {
            return Err(error::not_found("review gate not found or expired"));
        };
        if gate.expires_at <= Instant::now() {
            gates.remove(token);
            return Err(error::not_found("review gate not found or expired"));
        }
        Ok(gate.outcome)
    }
}

impl fmt::Debug for App {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("App")
            .field("auth_path", &self.auth_path)
            .finish()
    }
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
        let auth_path = match std::env::var_os("ALX_AUTH_FILE") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => database_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("password.hash"),
        };
        let store = Arc::new(SqliteStore::new(database_path)?);
        Ok(Self {
            auth_path: Arc::new(auth_path),
            store,
        })
    }

    #[cfg(test)]
    fn with_store(auth_path: PathBuf, store: Arc<dyn Store>) -> Self {
        Self {
            auth_path: Arc::new(auth_path),
            store,
        }
    }

    /// The password hash is kept outside SQLite so the existing storage schema stays unchanged.
    pub fn password_path(&self) -> PathBuf {
        self.auth_path.as_path().to_path_buf()
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

    pub fn create_task(&self, id: &str, body: &str) -> Result<Task> {
        self.store.create_task(id, body)
    }

    pub fn read_task(&self, key: &str) -> Result<Task> {
        self.store.read_task(key)
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        self.store.list_tasks()
    }

    pub fn list_completed_tasks(&self) -> Result<Vec<Task>> {
        self.store.list_completed_tasks()
    }

    pub fn list_archived_tasks(&self) -> Result<Vec<Task>> {
        self.store.list_archived_tasks()
    }

    pub fn search_tasks(&self, query: &str) -> Result<Vec<Task>> {
        self.store.search_tasks(query)
    }

    pub fn update_task(&self, key: &str, body: &str) -> Result<()> {
        self.store.update_task(key, body)
    }

    pub fn complete_task(&self, key: &str) -> Result<()> {
        self.store.complete_task(key)
    }

    pub fn reopen_task(&self, key: &str) -> Result<()> {
        self.store.reopen_task(key)
    }

    pub fn archive_task(&self, key: &str) -> Result<()> {
        self.store.archive_task(key)
    }

    pub fn delete_task(&self, key: &str) -> Result<()> {
        self.store.delete_task(key)
    }

    pub fn create_artifact(
        &self,
        task_key: &str,
        artifact_type: &str,
        body: &str,
    ) -> Result<Artifact> {
        self.store.create_artifact(task_key, artifact_type, body)
    }

    pub fn create_artifact_with_name(
        &self,
        task_key: &str,
        artifact_type: &str,
        name: Option<&str>,
        body: &str,
    ) -> Result<Artifact> {
        self.store
            .create_artifact_with_name(task_key, artifact_type, name, body)
    }

    pub fn read_artifact(&self, uuid: &str) -> Result<Artifact> {
        self.store.read_artifact(uuid)
    }

    pub fn update_artifact(&self, uuid: &str, body: &str) -> Result<()> {
        self.store.update_artifact(uuid, body)
    }

    pub fn rename_artifact(&self, uuid: &str, name: &str) -> Result<()> {
        self.store.rename_artifact(uuid, name)
    }

    pub fn list_artifacts(
        &self,
        task_key: &str,
        artifact_type: Option<&str>,
    ) -> Result<Vec<Artifact>> {
        self.store.list_artifacts(task_key, artifact_type)
    }

    pub fn create_annotation(
        &self,
        artifact_uuid: &str,
        input: NewAnnotation,
    ) -> Result<Annotation> {
        self.store.create_annotation(artifact_uuid, input)
    }

    pub fn list_annotations(
        &self,
        artifact_uuid: &str,
        include_resolved: bool,
    ) -> Result<Vec<Annotation>> {
        self.store.list_annotations(artifact_uuid, include_resolved)
    }

    pub fn resolve_annotation(&self, uuid: &str) -> Result<()> {
        self.store.resolve_annotation(uuid)
    }

    pub fn feedback_markdown(&self, artifact_uuid: &str) -> Result<String> {
        self.feedback_markdown_at_level(artifact_uuid, 1)
    }

    pub fn ensure_artifact_writable(&self, artifact_uuid: &str) -> Result<Artifact> {
        let artifact = self.read_artifact(artifact_uuid)?;
        let task = self.read_task(&artifact.task_uuid)?;
        if task.archived_at.is_some() {
            bail!("archived task is read-only: {}", task.id);
        }
        if task.completed_at.is_some() {
            bail!("completed task is read-only: {}", task.id);
        }
        Ok(artifact)
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
}

/// Run an interactive browser review and print the resulting review to stdout.
///
/// A running loopback server is reused when possible. Otherwise this process owns
/// a temporary server for the duration of the review. Gate state is kept in memory.
pub async fn run_interactive_review(app: App, artifact_uuid: &str, no_open: bool) -> Result<()> {
    let artifact = app.ensure_artifact_writable(artifact_uuid)?;

    let mut owned_server = None;
    let address = match tokio::net::TcpListener::bind(REVIEW_GATE_ADDRESS).await {
        Ok(listener) => {
            let gates = ReviewGates::default();
            let router = router_with_gates(app.clone(), gates);
            let address = listener.local_addr()?;
            owned_server = Some(tokio::spawn(async move {
                if let Err(error) = axum::serve(listener, router).await {
                    eprintln!("review server stopped: {error}");
                }
            }));
            address
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => REVIEW_GATE_ADDRESS,
        Err(error) => return Err(error).context("failed to start local review server"),
    };

    let registration = register_review_gate(address, &artifact.uuid).await?;
    let url = format!(
        "http://{address}/review/{}?gate={}",
        artifact.uuid, registration.token
    );
    eprintln!("Review URL: {url}");
    if !no_open {
        open_review_url(&url);
    }

    loop {
        if review_gate_finished(address, &registration.token).await? {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if let Some(server) = owned_server {
        server.abort();
    }
    print!("{}", app.review_markdown(artifact_uuid)?);
    Ok(())
}

#[derive(Debug, Deserialize)]
struct GateRegistration {
    token: String,
}

async fn register_review_gate(
    address: SocketAddr,
    artifact_uuid: &str,
) -> Result<GateRegistration> {
    let body = serde_json::to_string(&serde_json::json!({ "artifact_uuid": artifact_uuid }))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match review_http_request(address, "POST", "/api/review-gates", Some(&body)).await {
            Ok((201, response)) => {
                return serde_json::from_str(&response)
                    .context("local review server returned invalid gate data");
            }
            Ok((404, _response)) => {
                bail!("the local alx server does not support review gates; restart it")
            }
            Ok((status, response)) => {
                bail!(
                    "could not create review gate (HTTP {status}): {}",
                    response.trim()
                )
            }
            Err(error) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let _ = error;
            }
            Err(error) => return Err(error).context("could not connect to local alx server"),
        }
    }
}

async fn review_gate_finished(address: SocketAddr, token: &str) -> Result<bool> {
    let path = format!("/api/review-gates/{token}");
    let (status, response) = review_http_request(address, "GET", &path, None).await?;
    if status == 200 {
        #[derive(Deserialize)]
        struct GateStatus {
            outcome: Option<ReviewOutcome>,
        }
        return Ok(serde_json::from_str::<GateStatus>(&response)?
            .outcome
            .is_some());
    }
    bail!(
        "review gate is no longer available (HTTP {status}): {}",
        response.trim()
    )
}

async fn review_http_request(
    address: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<(u16, String)> {
    let mut stream = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect(address),
    )
    .await
    .context("timed out connecting to local alx server")??;
    let body = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response))
        .await
        .context("timed out reading local alx server response")??;
    let response =
        String::from_utf8(response).context("local alx server returned non-UTF-8 data")?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow!("local alx server returned an invalid HTTP response"))?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| anyhow!("local alx server returned an invalid HTTP status"))?
        .parse::<u16>()?;
    Ok((status, body.to_owned()))
}

fn open_review_url(url: &str) {
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(url).stdout(Stdio::null()).status();
    #[cfg(target_os = "linux")]
    let result = Command::new("xdg-open")
        .arg(url)
        .stdout(Stdio::null())
        .status();
    #[cfg(target_os = "windows")]
    let result = Command::new("cmd")
        .args(["/C", "start", "", url])
        .stdout(Stdio::null())
        .status();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let result: std::io::Result<std::process::ExitStatus> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "browser opening is not supported on this platform",
    ));
    if let Ok(status) = &result
        && !status.success()
    {
        eprintln!("Could not open review URL in a browser (exit status: {status})");
    }
    if let Err(error) = result {
        eprintln!("Could not open review URL in a browser: {error}");
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
    gates: ReviewGates,
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
    router_with_gates(app, ReviewGates::default())
}

fn router_with_gates(app: App, gates: ReviewGates) -> Router {
    build_router(WebState {
        app,
        auth: None,
        gates,
    })
}

/// Build a web router with authentication enabled. Loopback servers use `router` instead.
pub fn router_with_auth(app: App) -> Result<Router> {
    Ok(build_router(WebState {
        app: app.clone(),
        gates: ReviewGates::default(),
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
        .route("/review/{uuid}", get(review_page))
        .route("/login", get(login_page))
        .route("/api/login", post(api_login))
        .route("/api/tasks", get(api_tasks).post(api_create_task))
        .route("/api/completed-tasks", get(api_completed_tasks))
        .route("/api/archived-tasks", get(api_archived_tasks))
        .route("/api/review-gates", post(api_create_review_gate))
        .route("/api/review-gates/{token}", get(api_review_gate_status))
        .route(
            "/api/review-gates/{token}/finish",
            post(api_finish_review_gate),
        )
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

async fn review_page(
    State(state): State<WebState>,
    AxumPath(uuid): AxumPath<String>,
    Query(query): Query<ReviewQuery>,
) -> ApiResult<Response> {
    let gate_uuid = state.gates.artifact_uuid(&query.gate)?;
    if gate_uuid != uuid {
        return Err(invalid("review gate does not belong to this artifact").into());
    }
    state.app.read_artifact(&uuid)?;
    Ok(index().await.into_response())
}

#[derive(Debug, Deserialize)]
struct ReviewQuery {
    gate: String,
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
struct NewReviewGate {
    artifact_uuid: String,
}

async fn api_create_review_gate(
    State(state): State<WebState>,
    Json(input): Json<NewReviewGate>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let artifact = state.app.ensure_artifact_writable(&input.artifact_uuid)?;
    let token = state.gates.create(&artifact.uuid)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "token": token })),
    ))
}

async fn api_review_gate_status(
    State(state): State<WebState>,
    AxumPath(token): AxumPath<String>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({
        "outcome": state.gates.outcome(&token)?,
    })))
}

async fn api_finish_review_gate(
    State(state): State<WebState>,
    AxumPath(token): AxumPath<String>,
) -> ApiResult<StatusCode> {
    state.gates.finish(&token)?;
    Ok(StatusCode::NO_CONTENT)
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

fn random_token() -> String {
    let mut bytes = [0_u8; 24];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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

#[cfg(test)]
mod store_tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    struct RecordingStore {
        create_task_calls: Mutex<Vec<(String, String)>>,
    }

    impl Store for RecordingStore {
        fn create_task(&self, id: &str, body: &str) -> Result<Task> {
            self.create_task_calls
                .lock()
                .unwrap()
                .push((id.to_owned(), body.to_owned()));
            Ok(Task {
                uuid: "injected-uuid".to_owned(),
                id: "injected-id".to_owned(),
                body: "injected-body".to_owned(),
                created_at: "created".to_owned(),
                updated_at: "updated".to_owned(),
                archived_at: None,
                completed_at: None,
            })
        }

        fn read_task(&self, _key: &str) -> Result<Task> {
            unreachable!()
        }

        fn list_tasks(&self) -> Result<Vec<Task>> {
            unreachable!()
        }

        fn list_completed_tasks(&self) -> Result<Vec<Task>> {
            unreachable!()
        }

        fn list_archived_tasks(&self) -> Result<Vec<Task>> {
            unreachable!()
        }

        fn search_tasks(&self, _query: &str) -> Result<Vec<Task>> {
            unreachable!()
        }

        fn update_task(&self, _key: &str, _body: &str) -> Result<()> {
            unreachable!()
        }

        fn complete_task(&self, _key: &str) -> Result<()> {
            unreachable!()
        }

        fn reopen_task(&self, _key: &str) -> Result<()> {
            unreachable!()
        }

        fn archive_task(&self, _key: &str) -> Result<()> {
            unreachable!()
        }

        fn delete_task(&self, _key: &str) -> Result<()> {
            unreachable!()
        }

        fn create_artifact(
            &self,
            _task_key: &str,
            _artifact_type: &str,
            _body: &str,
        ) -> Result<Artifact> {
            unreachable!()
        }

        fn create_artifact_with_name(
            &self,
            _task_key: &str,
            _artifact_type: &str,
            _name: Option<&str>,
            _body: &str,
        ) -> Result<Artifact> {
            unreachable!()
        }

        fn read_artifact(&self, _uuid: &str) -> Result<Artifact> {
            unreachable!()
        }

        fn update_artifact(&self, _uuid: &str, _body: &str) -> Result<()> {
            unreachable!()
        }

        fn rename_artifact(&self, _uuid: &str, _name: &str) -> Result<()> {
            unreachable!()
        }

        fn list_artifacts(
            &self,
            _task_key: &str,
            _artifact_type: Option<&str>,
        ) -> Result<Vec<Artifact>> {
            unreachable!()
        }

        fn create_annotation(
            &self,
            _artifact_uuid: &str,
            _input: NewAnnotation,
        ) -> Result<Annotation> {
            unreachable!()
        }

        fn list_annotations(
            &self,
            _artifact_uuid: &str,
            _include_resolved: bool,
        ) -> Result<Vec<Annotation>> {
            unreachable!()
        }

        fn resolve_annotation(&self, _uuid: &str) -> Result<()> {
            unreachable!()
        }
    }

    #[test]
    fn app_delegates_task_creation_and_forwards_arguments() {
        let store = Arc::new(RecordingStore {
            create_task_calls: Mutex::new(Vec::new()),
        });
        let app = App::with_store(PathBuf::from("password.hash"), store.clone());

        let task = app.create_task("task", "body").unwrap();

        assert_eq!(task.id, "injected-id");
        assert_eq!(
            *store.create_task_calls.lock().unwrap(),
            vec![("task".to_owned(), "body".to_owned())]
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

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
