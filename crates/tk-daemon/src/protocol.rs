//! The socket protocol: JSON lines, one object per line, as
//! `contracts/daemon-protocol.md` section 2 defines. These types are the
//! contract's Rust form; lane B's server and lane D's and J's clients use
//! them. The fixture `contracts/fixtures/daemon/exchange.jsonl` is parsed by
//! the test at the bottom.

use serde::{Deserialize, Serialize};
use tk_core::event::Event;

/// A client line. `op` selects the variant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    /// First line on every connection. `token` is the daemon's token or a
    /// chat token; the reply says which scope it got.
    Hello { token: String, client: String },
    /// Store one event. The id is the client's; a second event with the same
    /// id replaces the first (case 35).
    Event { event: Event },
    /// Stream events as they arrive, after a backlog from `since`
    /// (milliseconds since the epoch) when given. `chat` filters.
    Tail {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat: Option<String>,
    },
    /// One JSON report over a window.
    Report {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat: Option<String>,
        since: i64,
        until: i64,
    },
    /// Ask a connected approver. The daemon answers when the first approver
    /// answers, or `no` at the timeout, or `no` at once when none is
    /// connected (case 63).
    Approve { request: ApprovalRequest },
    /// Become an approver: the daemon then sends `approve` lines on this
    /// connection and reads `answer` lines back.
    Approver {},
    /// An approver's answer to one request. `chat` names the request's chat;
    /// it may be left out while only one chat waits on that id (integration
    /// w2, security finding S8).
    Answer {
        id: String,
        answer: Answer,
        #[serde(default)]
        chat: Option<String>,
    },
    /// Mint a chat token. Needs the daemon scope.
    #[serde(rename = "token.mint")]
    TokenMint { chat: String, harness: String },
    /// Computer use's controls, which went with the fork's menu bar (lane
    /// K's request 3, integration w2): its status, stop one chat or every
    /// chat, take one app back from a chat. Needs the daemon scope.
    Cu {
        action: CuAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CuAction {
    Status,
    Stop,
    StopAll,
    Revoke,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Answer {
    Yes,
    No,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalRequest {
    pub id: String,
    pub chat: String,
    pub harness: String,
    /// Normalized tool name (`contracts/policy.md`).
    pub tool: String,
    pub summary: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u32,
}

fn default_timeout() -> u32 {
    60
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Daemon,
    Chat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnsweredBy {
    Approver,
    Timeout,
    None,
}

/// A daemon line. Every request gets exactly one `Reply`; `tail` and
/// `approver` then stream `Push` lines until the client closes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServerLine {
    Reply(Reply),
    Push(Push),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon: Option<DaemonInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<AnsweredBy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DaemonInfo {
    pub version: String,
    pub pid: u32,
    pub protocol: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Push {
    Event(Event),
    Approve(ApprovalRequest),
}

pub const PROTOCOL_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../contracts/fixtures/daemon/exchange.jsonl");

    #[test]
    fn the_recorded_exchange_parses_line_by_line() {
        let mut requests = 0;
        let mut replies = 0;
        for (n, line) in FIXTURE.lines().enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (dir, body) = line.split_at(2);
            match dir {
                "> " => {
                    serde_json::from_str::<Request>(body)
                        .unwrap_or_else(|e| panic!("line {}: {e}: {body}", n + 1));
                    requests += 1;
                }
                "< " => {
                    serde_json::from_str::<ServerLine>(body)
                        .unwrap_or_else(|e| panic!("line {}: {e}: {body}", n + 1));
                    replies += 1;
                }
                _ => panic!("line {}: no direction marker", n + 1),
            }
        }
        assert!(requests >= 8, "{requests} requests");
        assert!(
            replies >= requests,
            "{replies} replies for {requests} requests"
        );
    }

    #[test]
    fn token_mint_op_name_has_a_dot() {
        let r = Request::TokenMint {
            chat: "c1".into(),
            harness: "pi".into(),
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"op":"token.mint","chat":"c1","harness":"pi"}"#
        );
    }

    #[test]
    fn unknown_ops_and_fields_are_refused() {
        assert!(serde_json::from_str::<Request>(r#"{"op":"shutdown"}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"op":"approver","force":true}"#).is_err());
    }
}
