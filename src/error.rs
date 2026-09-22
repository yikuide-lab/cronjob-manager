use std::io;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Invalid cron expression '{0}': {1}")]
    InvalidSchedule(String, String),
    #[error("Command failed: {0}")]
    CommandFailed(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, AppError>;
