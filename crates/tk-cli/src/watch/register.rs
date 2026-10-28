//! Whether tk's gate is registered for an agent whose gate is built, read
//! from the agent's own user-level files. Nothing here runs the agent.
//! Filled from the lane J research (status doc, Research).

use super::status::Env;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Registered {
    Yes(String),
    No(String),
    Unknown(String),
    NotBuilt,
}

impl Registered {
    pub fn word(&self) -> &'static str {
        match self {
            Registered::Yes(_) => "yes",
            Registered::No(_) => "no",
            Registered::Unknown(_) => "unknown",
            Registered::NotBuilt => "n/a",
        }
    }

    pub fn detail(&self) -> Option<String> {
        match self {
            Registered::Yes(d) | Registered::No(d) | Registered::Unknown(d) => Some(d.clone()),
            Registered::NotBuilt => None,
        }
    }
}

pub fn check(id: &str, env: &Env) -> Registered {
    let _ = env;
    Registered::Unknown(format!("{id}: not read yet"))
}
