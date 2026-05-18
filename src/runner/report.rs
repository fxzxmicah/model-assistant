use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunnerWarnings {
    pub messages: Vec<String>,
}

impl RunnerWarnings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, message: impl Into<String>) {
        self.messages.push(message.into());
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

}

impl fmt::Display for RunnerWarnings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.messages.join("\n"))
    }
}
