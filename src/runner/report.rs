use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunnerWarnings {
    pub messages: Vec<String>,
}

impl RunnerWarnings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_message(message: impl Into<String>) -> Self {
        Self {
            messages: vec![message.into()],
        }
    }

    pub fn push(&mut self, message: impl Into<String>) {
        self.messages.push(message.into());
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn body(&self) -> String {
        self.messages.join("\n")
    }
}
