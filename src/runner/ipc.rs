use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::runner::report::RunnerWarnings;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ClientMessage {
    Spawn {
        argv: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Stdin {
        data: String,
    },
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KeeperStatus {
    Ready { warnings: RunnerWarnings },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum KeeperMessage {
    ChildSpawned,
    ChildStdout {
        data: Vec<u8>,
    },
    ChildStderr {
        data: Vec<u8>,
    },
    ChildExited,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum KeeperFrame {
    KeeperStatus {
        status: KeeperStatus,
    },
    KeeperMessage {
        message: KeeperMessage,
    },
}


impl KeeperStatus {
    pub fn ready(warnings: RunnerWarnings) -> Self {
        Self::Ready { warnings }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }

    pub fn warnings(&self) -> Option<&RunnerWarnings> {
        match self {
            Self::Ready { warnings } => Some(warnings),
            Self::Error { .. } => None,
        }
    }

    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::Ready { .. } => None,
            Self::Error { message } => Some(message),
        }
    }
}

impl KeeperMessage {
    pub fn child_stdout(data: Vec<u8>) -> Self {
        Self::ChildStdout { data }
    }

    pub fn child_stderr(data: Vec<u8>) -> Self {
        Self::ChildStderr { data }
    }

    pub fn output_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::ChildStdout { data } | Self::ChildStderr { data } => Some(data),
            Self::ChildSpawned | Self::ChildExited => None,
        }
    }

    pub fn is_child_exited(&self) -> bool {
        matches!(self, Self::ChildExited)
    }
}

impl KeeperFrame {
    pub fn status(status: KeeperStatus) -> Self {
        Self::KeeperStatus { status }
    }

    pub fn child(message: KeeperMessage) -> Self {
        Self::KeeperMessage { message }
    }
}

pub fn write_json_line<W, T>(writer: &mut W, message: &T) -> Result<()>
where
    W: Write,
    T: Serialize,
{
    let mut data = serde_json::to_vec(message)?;
    data.push(b'\n');
    writer.write_all(&data)?;
    writer.flush()?;
    Ok(())
}

pub fn read_json_line<R, T>(reader: &mut R) -> Result<T>
where
    R: BufRead,
    T: serde::de::DeserializeOwned,
{
    let mut line = String::new();
    let size = reader.read_line(&mut line)?;
    if size == 0 {
        bail!("runner stream closed");
    }
    Ok(serde_json::from_str(
        line.trim_end_matches(['\n', '\r']),
    )?)
}
