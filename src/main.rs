use std::io::{self, Read};

use alx::{AnnotationKind, App, NewAnnotation, parse_bind_address};
use anyhow::Result;
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
    Serve(ServeArgs),
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
    Search {
        query: String,
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        json: bool,
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
    },
    Read {
        uuid: String,
    },
    Update {
        uuid: String,
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
struct ServeArgs {
    #[arg(long, value_name = "IP:PORT", conflicts_with = "tailscale")]
    bind: Option<String>,
    #[arg(long, conflicts_with = "bind")]
    tailscale: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let app = App::from_env()?;

    match cli.command {
        Command::Task(args) => run_task(&app, args.command)?,
        Command::Artifact(args) => run_artifact(&app, args.command)?,
        Command::Annotation(args) => run_annotation(&app, args.command)?,
        Command::Serve(args) => {
            let address = parse_bind_address(args.bind.as_deref(), args.tailscale)?;
            alx::serve(app, address).await?;
        }
    }
    Ok(())
}

fn run_task(app: &App, command: TaskCommand) -> Result<()> {
    match command {
        TaskCommand::Create { id } => {
            let body = stdin()?;
            println!("{}", app.create_task(&id, &body)?.uuid);
        }
        TaskCommand::Read { id_or_uuid } => print!("{}", app.read_task(&id_or_uuid)?.body),
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
        TaskCommand::List { json } => {
            let tasks = app.list_tasks()?;
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
        TaskCommand::Context { id_or_uuid } => print!("{}", app.context_markdown(&id_or_uuid)?),
    }
    Ok(())
}

fn run_artifact(app: &App, command: ArtifactCommand) -> Result<()> {
    match command {
        ArtifactCommand::Create {
            task_id_or_uuid,
            artifact_type,
        } => {
            let body = stdin()?;
            println!(
                "{}",
                app.create_artifact(&task_id_or_uuid, &artifact_type, &body)?
                    .uuid
            );
        }
        ArtifactCommand::Read { uuid } => print!("{}", app.read_artifact(&uuid)?.body),
        ArtifactCommand::Update { uuid } => app.update_artifact(&uuid, &stdin()?)?,
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
                        "{}\t{}\t{}",
                        artifact.uuid,
                        tsv_field(&artifact.artifact_type),
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
