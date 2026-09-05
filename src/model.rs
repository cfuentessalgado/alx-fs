use std::str::FromStr;

use anyhow::{Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactInfo {
    pub uuid: String,
    pub task_uuid: String,
    pub task_id: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SearchDocument {
    pub path: String,
    pub artifact_uuid: Option<String>,
    pub body: String,
}

impl SearchDocument {
    pub fn display_path(&self) -> String {
        match &self.artifact_uuid {
            Some(uuid) => format!("{} [{uuid}]", self.path),
            None => self.path.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrepMatch {
    pub path: String,
    pub artifact_uuid: Option<String>,
    pub line_number: u64,
    pub line: String,
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

pub(crate) fn artifact_fallback_name(artifact_type: &str, uuid: &str) -> String {
    format!("{artifact_type}--{}.md", &uuid[..8])
}
