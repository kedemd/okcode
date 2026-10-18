//! Errors crossing the API. SHARED FILE (architect-owned).
//!
//! JS throws `Error`s carrying a `code` (OKCODE_BAD_QUERY, OKCODE_NOT_INDEXED, ENV_NOT_FOUND, …) and
//! sometimes extra properties; refusals that are RESULTS (`{ ok: false, … }`) are values, not errors.
use serde_json::{Map, Value};
use std::fmt;

#[derive(Clone, Debug)]
pub struct Error {
    /// The JS `err.code`, when the JS error has one.
    pub code: Option<String>,
    pub message: String,
    /// Extra own properties copied onto the JS error (`details`, …).
    pub extra: Map<String, Value>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn msg(message: impl Into<String>) -> Error {
        Error { code: None, message: message.into(), extra: Map::new() }
    }
    pub fn code(code: impl Into<String>, message: impl Into<String>) -> Error {
        Error { code: Some(code.into()), message: message.into(), extra: Map::new() }
    }
    /// A skeleton stub: the slice that owns `what` has not implemented it yet.
    pub fn todo(what: &str) -> Error {
        Error::code("OKCODE_NOT_IMPLEMENTED", format!("not implemented in the Rust port yet: {what}"))
    }
    pub fn with(mut self, key: &str, v: Value) -> Error {
        self.extra.insert(key.into(), v);
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.code {
            Some(c) => write!(f, "{c}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(s: String) -> Error {
        Error::msg(s)
    }
}
impl From<&str> for Error {
    fn from(s: &str) -> Error {
        Error::msg(s)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Error {
        Error::msg(e.to_string())
    }
}
