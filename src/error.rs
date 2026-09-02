//! One error type across config parsing, planning, the lifecycle state machine,
//! and the Linux runtime.
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Config(String),
    Plan(String),
    State(String),
    Runtime(String),
    Unsupported(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(m) => write!(f, "config error: {m}"),
            Error::Plan(m) => write!(f, "plan error: {m}"),
            Error::State(m) => write!(f, "state error: {m}"),
            Error::Runtime(m) => write!(f, "runtime error: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
