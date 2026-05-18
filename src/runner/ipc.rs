use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ServerMessage {
    Spawned {
        pid: i32,
    },
    Output {
        data: Vec<u8>,
    },
    Exited,
    Error {
        message: String,
    },
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
    Ok(serde_json::from_str(line.trim_end())?)
}
