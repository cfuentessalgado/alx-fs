use std::io::{self, IsTerminal, Read};

use alx::{AnnotationKind, App, NewAnnotation, parse_bind_address};
use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "alx", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Task(TaskArgs),
    Artifact(ArtifactArgs),
    Annotation(AnnotationArgs),
    Skill(SkillArgs),
    Serve(ServeArgs),
    Dump(DumpArgs),
}

#[derive(Debug, Args)]
struct TaskArgs {
    #[command(subcommand)]
    command: TaskCommand,
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    Create {
        id: String,
    },
    Read {
        id_or_uuid: String,
    },
    Update {
        id_or_uuid: String,
    },
    Search {
        query: String,
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        json: bool,
        #[arg(long, conflicts_with = "completed")]
        archived: bool,
        #[arg(long, conflicts_with = "archived")]
        completed: bool,
    },
    Complete {
        id_or_uuid: String,
    },
    Reopen {
        id_or_uuid: String,
    },
    Archive {
        id_or_uuid: String,
    },
    Delete {
        id_or_uuid: String,
        #[arg(long, alias = "yes")]
        confirm: bool,
    },
    Context {
        id_or_uuid: String,
    },
}

#[derive(Debug, Args)]
struct ArtifactArgs {
    #[command(subcommand)]
    command: ArtifactCommand,
}

#[derive(Debug, Subcommand)]
enum ArtifactCommand {
    Create {
        task_id_or_uuid: String,
        artifact_type: String,
        #[arg(long)]
        name: Option<String>,
    },
    Read {
        uuid: String,
    },
    Update {
        uuid: String,
    },
    Rename {
        uuid: String,
        name: String,
    },
    List {
        task_id_or_uuid: String,
        #[arg(long = "type")]
        artifact_type: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Feedback {
        uuid: String,
        #[arg(long)]
        json: bool,
    },
    Review {
        uuid: String,
    },
}

#[derive(Debug, Args)]
struct AnnotationArgs {
    #[command(subcommand)]
    command: AnnotationCommand,
}

#[derive(Debug, Subcommand)]
enum AnnotationCommand {
    Create {
        artifact_uuid: String,
        kind: AnnotationKind,
        #[arg(long, requires = "end_offset")]
        start_offset: Option<u64>,
        #[arg(long, requires = "start_offset")]
        end_offset: Option<u64>,
        #[arg(long)]
        selected_text: Option<String>,
    },
    List {
        artifact_uuid: String,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    Resolve {
        uuid: String,
    },
}

#[derive(Debug, Args)]
struct SkillArgs {
    #[command(subcommand)]
    command: SkillCommand,
}

#[derive(Debug, Subcommand)]
enum SkillCommand {
    Read,
    Install,
}

#[derive(Debug, Args)]
struct DumpArgs {
    /// Optional task key followed by the target path; the last value is always the target
    #[arg(num_args = 1..=2, required = true, value_name = "[TASK] TARGET")]
    args: Vec<String>,
    /// Write a zip archive at the target path instead of a directory tree
    #[arg(long)]
    zip: bool,
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[command(subcommand)]
    command: Option<ServeCommand>,
    #[arg(long, value_name = "IP:PORT", conflicts_with = "tailscale")]
    bind: Option<String>,
    #[arg(long, conflicts_with = "bind")]
    tailscale: bool,
}

#[derive(Debug, Subcommand)]
enum ServeCommand {
    Password(PasswordArgs),
    Install {
        #[arg(long, value_name = "IP:PORT", conflicts_with = "tailscale")]
        bind: Option<String>,
        #[arg(long, conflicts_with = "bind")]
        tailscale: bool,
    },
    Status,
    Restart,
    Uninstall,
}

#[derive(Debug, Args)]
struct PasswordArgs {
    #[command(subcommand)]
    command: PasswordCommand,
}

#[derive(Debug, Subcommand)]
enum PasswordCommand {
    Set,
    Clear,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Task(args) => run_task(&App::from_env()?, args.command)?,
        Command::Artifact(args) => run_artifact(&App::from_env()?, args.command)?,
        Command::Annotation(args) => run_annotation(&App::from_env()?, args.command)?,
        Command::Skill(args) => run_skill(args.command)?,
        Command::Serve(args) => run_serve(args).await?,
        Command::Dump(args) => run_dump(&App::from_env()?, args)?,
    }
    Ok(())
}

async fn run_serve(args: ServeArgs) -> Result<()> {
    let ServeArgs {
        command,
        bind: foreground_bind,
        tailscale: foreground_tailscale,
    } = args;
    match command {
        None => {
            let address = parse_bind_address(foreground_bind.as_deref(), foreground_tailscale)?;
            alx::serve(App::from_env()?, address).await
        }
        Some(ServeCommand::Password(password_args)) => {
            let app = App::from_env()?;
            match password_args.command {
                PasswordCommand::Set => {
                    let first = read_password("Password: ")?;
                    let second = read_password("Confirm password: ")?;
                    if first != second {
                        bail!("passwords do not match");
                    }
                    app.set_password(&first)?;
                }
                PasswordCommand::Clear => app.clear_password()?,
            }
            Ok(())
        }
        Some(ServeCommand::Install {
            bind: install_bind,
            tailscale: install_tailscale,
        }) => {
            let bind = install_bind.or(foreground_bind);
            let tailscale = install_tailscale || foreground_tailscale;
            if bind.is_some() && tailscale {
                bail!("--bind cannot be used with --tailscale");
            }
            if let Some(bind) = bind.as_deref() {
                // Validate now, but resolve --tailscale only when the service starts.
                parse_bind_address(Some(bind), false)?;
            }
            alx::service::install(&alx::service::InstallOptions { bind, tailscale })
        }
        Some(ServeCommand::Status) => alx::service::status(),
        Some(ServeCommand::Restart) => alx::service::restart(),
        Some(ServeCommand::Uninstall) => alx::service::uninstall(),
    }
}

fn read_password(prompt: &str) -> Result<String> {
    if io::stdin().is_terminal() {
        return Ok(rpassword::prompt_password(prompt)?);
    }
    let config = rpassword::ConfigBuilder::new()
        .input_reader(io::stdin())
        .output_writer(io::stderr())
        .build();
    Ok(rpassword::prompt_password_with_config(prompt, config)?)
}

fn run_task(app: &App, command: TaskCommand) -> Result<()> {
    match command {
        TaskCommand::Create { id } => {
            let body = stdin()?;
            println!("{}", app.create_task(&id, &body)?.uuid);
        }
        TaskCommand::Read { id_or_uuid } => print!("{}", app.read_task(&id_or_uuid)?.body),
        TaskCommand::Update { id_or_uuid } => app.update_task(&id_or_uuid, &stdin()?)?,
        TaskCommand::Search { query, json } => {
            let tasks = app.search_tasks(&query)?;
            if json {
                println!("{}", serde_json::to_string(&tasks)?);
            } else {
                for task in tasks {
                    println!(
                        "{}\t{}\t{}",
                        task.uuid,
                        tsv_field(&task.id),
                        task.updated_at
                    );
                }
            }
        }
        TaskCommand::List {
            json,
            archived,
            completed,
        } => {
            let tasks = if archived {
                app.list_archived_tasks()?
            } else if completed {
                app.list_completed_tasks()?
            } else {
                app.list_tasks()?
            };
            if json {
                println!("{}", serde_json::to_string(&tasks)?);
            } else {
                for task in tasks {
                    println!(
                        "{}\t{}\t{}",
                        task.uuid,
                        tsv_field(&task.id),
                        task.updated_at
                    );
                }
            }
        }
        TaskCommand::Complete { id_or_uuid } => app.complete_task(&id_or_uuid)?,
        TaskCommand::Reopen { id_or_uuid } => app.reopen_task(&id_or_uuid)?,
        TaskCommand::Archive { id_or_uuid } => app.archive_task(&id_or_uuid)?,
        TaskCommand::Delete {
            id_or_uuid,
            confirm,
        } => {
            if !confirm {
                bail!("task deletion requires --confirm");
            }
            app.delete_task(&id_or_uuid)?;
        }
        TaskCommand::Context { id_or_uuid } => print!("{}", app.context_markdown(&id_or_uuid)?),
    }
    Ok(())
}

fn run_artifact(app: &App, command: ArtifactCommand) -> Result<()> {
    match command {
        ArtifactCommand::Create {
            task_id_or_uuid,
            artifact_type,
            name,
        } => {
            let body = stdin()?;
            println!(
                "{}",
                app.create_artifact_with_name(
                    &task_id_or_uuid,
                    &artifact_type,
                    name.as_deref(),
                    &body,
                )?
                .uuid
            );
        }
        ArtifactCommand::Read { uuid } => print!("{}", app.read_artifact(&uuid)?.body),
        ArtifactCommand::Update { uuid } => app.update_artifact(&uuid, &stdin()?)?,
        ArtifactCommand::Rename { uuid, name } => app.rename_artifact(&uuid, &name)?,
        ArtifactCommand::List {
            task_id_or_uuid,
            artifact_type,
            json,
        } => {
            let artifacts = app.list_artifacts(&task_id_or_uuid, artifact_type.as_deref())?;
            if json {
                println!("{}", serde_json::to_string(&artifacts)?);
            } else {
                for artifact in artifacts {
                    println!(
                        "{}\t{}\t{}\t{}",
                        artifact.uuid,
                        tsv_field(&artifact.artifact_type),
                        tsv_field(&artifact.display_name()),
                        artifact.updated_at
                    );
                }
            }
        }
        ArtifactCommand::Feedback { uuid, json } => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&app.list_annotations(&uuid, false)?)?
                );
            } else {
                print!("{}", app.feedback_markdown(&uuid)?);
            }
        }
        ArtifactCommand::Review { uuid } => print!("{}", app.review_markdown(&uuid)?),
    }
    Ok(())
}

fn run_annotation(app: &App, command: AnnotationCommand) -> Result<()> {
    match command {
        AnnotationCommand::Create {
            artifact_uuid,
            kind,
            start_offset,
            end_offset,
            selected_text,
        } => {
            let body = stdin()?;
            let annotation = app.create_annotation(
                &artifact_uuid,
                NewAnnotation {
                    kind,
                    start_offset,
                    end_offset,
                    selected_text,
                    body: Some(body),
                },
            )?;
            println!("{}", annotation.uuid);
        }
        AnnotationCommand::List {
            artifact_uuid,
            all,
            json,
        } => {
            let annotations = app.list_annotations(&artifact_uuid, all)?;
            if json {
                println!("{}", serde_json::to_string(&annotations)?);
            } else {
                for annotation in annotations {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        annotation.uuid,
                        annotation.kind.as_str(),
                        optional_number(annotation.start_offset),
                        optional_number(annotation.end_offset),
                        annotation.resolved_at.as_deref().unwrap_or("\\N")
                    );
                }
            }
        }
        AnnotationCommand::Resolve { uuid } => app.resolve_annotation(&uuid)?,
    }
    Ok(())
}

fn run_dump(app: &App, args: DumpArgs) -> Result<()> {
    let (task_key, target) = match args.args.as_slice() {
        [target] => (None, target.as_str()),
        [task_key, target] => (Some(task_key.as_str()), target.as_str()),
        _ => bail!("dump accepts an optional task key followed by one target path"),
    };
    app.dump(std::path::Path::new(target), task_key, args.zip)
}

fn run_skill(command: SkillCommand) -> Result<()> {
    match command {
        SkillCommand::Read => print!("{}", alx::AGENT_SKILL),
        SkillCommand::Install => println!("{}", alx::install_skill()?.display()),
    }
    Ok(())
}

fn stdin() -> Result<String> {
    let mut body = String::new();
    io::stdin().read_to_string(&mut body)?;
    Ok(body)
}

fn optional_number(value: Option<u64>) -> String {
    value.map_or_else(|| "\\N".to_owned(), |number| number.to_string())
}

fn tsv_field(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}
