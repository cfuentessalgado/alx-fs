use std::fmt;

use std::error::Error as StdError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DomainErrorKind {
    Invalid,
    NotFound,
}

#[derive(Debug)]
pub(crate) struct DomainError {
    pub(crate) kind: DomainErrorKind,
    message: String,
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl StdError for DomainError {}

pub(crate) fn invalid(message: impl Into<String>) -> anyhow::Error {
    DomainError {
        kind: DomainErrorKind::Invalid,
        message: message.into(),
    }
    .into()
}

pub(crate) fn not_found(message: impl Into<String>) -> anyhow::Error {
    DomainError {
        kind: DomainErrorKind::NotFound,
        message: message.into(),
    }
    .into()
}
