//! The container lifecycle as an explicit state machine. Even though `cocoon run`
//! drives it start to finish in one process, modeling the transitions keeps the
//! rules in one readable place and makes the illegal moves testable.
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Created,
    Running { pid: i32 },
    Stopped { exit_code: i32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    pub id: String,
    pub state: State,
}

impl Container {
    /// A freshly created container: namespaces planned, nothing running yet.
    pub fn create(id: impl Into<String>) -> Container {
        Container {
            id: id.into(),
            state: State::Created,
        }
    }

    /// Created -> Running. Only a created container can start.
    pub fn start(&mut self, pid: i32) -> Result<()> {
        match self.state {
            State::Created => {
                self.state = State::Running { pid };
                Ok(())
            }
            State::Running { .. } => Err(Error::State("container is already running".into())),
            State::Stopped { .. } => Err(Error::State("cannot start a stopped container".into())),
        }
    }

    /// Running -> Stopped. Only a running container can stop.
    pub fn stop(&mut self, exit_code: i32) -> Result<()> {
        match self.state {
            State::Running { .. } => {
                self.state = State::Stopped { exit_code };
                Ok(())
            }
            State::Created => Err(Error::State(
                "cannot stop a container that never started".into(),
            )),
            State::Stopped { .. } => Err(Error::State("container is already stopped".into())),
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state, State::Running { .. })
    }

    pub fn exit_code(&self) -> Option<i32> {
        match self.state {
            State::Stopped { exit_code } => Some(exit_code),
            _ => None,
        }
    }
}
