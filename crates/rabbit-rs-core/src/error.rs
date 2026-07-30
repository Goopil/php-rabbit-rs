//! Error types exposed by the Rabbit RS core.

use std::{error::Error, fmt};

/// A configuration error associated with an exact input path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigError {
    path: String,
    message: String,
}

impl ConfigError {
    pub(crate) fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }

    /// Returns the configuration path responsible for the error.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl Error for ConfigError {}
