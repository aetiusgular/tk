//! The apps each chat may drive (C2).
//!
//! A grant is one app for one chat, written only from the approval card. The
//! table lives in the app's memory for this run: a reload of the window keeps
//! it, a quit ends every grant, and the next use asks again. Nothing is kept
//! on disk, so no file an agent can write adds an app.

use std::collections::HashMap;

use super::apps::{parse_bundle_id, AppRefusal};

/// The most apps one chat may hold at once.
pub const MAX_APPS_PER_CHAT: usize = 16;
/// The most chats with grants at once. Past this, the oldest chat's grants end.
pub const MAX_CHATS: usize = 64;
/// The most ended grants kept per chat, for the record shown in the chat.
const MAX_ENDED_PER_CHAT: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    pub chat_id: String,
    pub bundle_id: String,
    /// The app's name as the card showed it. Display only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    /// Milliseconds since the epoch.
    pub granted_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    /// The activity log's approval record id for this grant.
    pub approval_id: String,
}

impl Grant {
    pub fn holds_at(&self, at: i64) -> bool {
        self.granted_at <= at && self.ended_at.is_none_or(|end| at < end)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrantError {
    App(AppRefusal),
    NoChat,
    TooMany,
}

impl GrantError {
    pub fn message(&self) -> String {
        match self {
            GrantError::App(refusal) => refusal.message().to_string(),
            GrantError::NoChat => "No chat to grant the app to.".into(),
            GrantError::TooMany => {
                format!("A chat can hold at most {MAX_APPS_PER_CHAT} apps. Remove one first.")
            }
        }
    }
}

#[derive(Default)]
pub struct GrantTable {
    chats: HashMap<String, Vec<Grant>>,
    /// Chat ids in the order their first grant was made, oldest first.
    order: Vec<String>,
}

impl GrantTable {
    /// Grants one app to one chat. Granting an app the chat already holds
    /// returns the grant it has, unchanged.
    pub fn grant(
        &mut self,
        chat_id: &str,
        bundle_id: &str,
        app_name: Option<&str>,
        now: i64,
        approval_id: String,
    ) -> Result<(Grant, Vec<Grant>), GrantError> {
        if chat_id.is_empty() || chat_id.len() > 200 {
            return Err(GrantError::NoChat);
        }
        let bundle_id = parse_bundle_id(bundle_id).map_err(GrantError::App)?;
        if let Some(existing) = self
            .active(chat_id, now)
            .into_iter()
            .find(|grant| grant.bundle_id.eq_ignore_ascii_case(&bundle_id))
        {
            return Ok((existing, Vec::new()));
        }
        if self.active(chat_id, now).len() >= MAX_APPS_PER_CHAT {
            return Err(GrantError::TooMany);
        }
        // Make room for a new chat by ending the oldest chat's grants.
        let mut evicted = Vec::new();
        if !self.chats.contains_key(chat_id) && self.order.len() >= MAX_CHATS {
            let oldest = self.order.remove(0);
            evicted = self.end_chat(&oldest, now);
            self.chats.remove(&oldest);
        }
        let grant = Grant {
            chat_id: chat_id.to_string(),
            bundle_id,
            app_name: app_name.map(clean_name).filter(|name| !name.is_empty()),
            granted_at: now,
            ended_at: None,
            approval_id,
        };
        if !self.chats.contains_key(chat_id) {
            self.order.push(chat_id.to_string());
        }
        let list = self.chats.entry(chat_id.to_string()).or_default();
        list.push(grant.clone());
        trim_ended(list);
        Ok((grant, evicted))
    }

    /// Ends one app's grant for a chat. Returns the ended grant.
    pub fn end(&mut self, chat_id: &str, bundle_id: &str, now: i64) -> Option<Grant> {
        let list = self.chats.get_mut(chat_id)?;
        let grant = list.iter_mut().find(|grant| {
            grant.ended_at.is_none() && grant.bundle_id.eq_ignore_ascii_case(bundle_id)
        })?;
        grant.ended_at = Some(now.max(grant.granted_at));
        Some(grant.clone())
    }

    /// Ends every grant a chat holds. Returns those it ended.
    pub fn end_chat(&mut self, chat_id: &str, now: i64) -> Vec<Grant> {
        let Some(list) = self.chats.get_mut(chat_id) else {
            return Vec::new();
        };
        let mut ended = Vec::new();
        for grant in list.iter_mut().filter(|grant| grant.ended_at.is_none()) {
            grant.ended_at = Some(now.max(grant.granted_at));
            ended.push(grant.clone());
        }
        trim_ended(list);
        ended
    }

    /// Ends every grant of every chat. Tests only: at quit and from the menu
    /// bar, `Core::stop_all` ends each chat through `Core::stop`, which also
    /// writes the approval records and stops the runtimes.
    #[cfg(test)]
    pub fn end_all(&mut self, now: i64) -> Vec<Grant> {
        let chats: Vec<String> = self.chats.keys().cloned().collect();
        chats
            .iter()
            .flat_map(|chat| self.end_chat(chat, now))
            .collect()
    }

    /// The grants that hold for a chat now, in the order they were made.
    pub fn active(&self, chat_id: &str, now: i64) -> Vec<Grant> {
        self.chats
            .get(chat_id)
            .map(|list| {
                list.iter()
                    .filter(|grant| grant.holds_at(now))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every grant kept for a chat, ended ones included.
    #[cfg(test)]
    pub fn history(&self, chat_id: &str) -> Vec<Grant> {
        self.chats.get(chat_id).cloned().unwrap_or_default()
    }

    /// Chats that hold at least one grant now.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(
            dead_code,
            reason = "only Core::stop_all, called from the macOS menu bar, uses it"
        )
    )]
    pub fn chats_with_grants(&self, now: i64) -> Vec<String> {
        self.order
            .iter()
            .filter(|chat| !self.active(chat, now).is_empty())
            .cloned()
            .collect()
    }
}

/// A display name, one line, at most 80 characters, no control characters.
fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect::<String>()
        .trim()
        .to_string()
}

fn trim_ended(list: &mut Vec<Grant>) {
    let ended = list.iter().filter(|grant| grant.ended_at.is_some()).count();
    let mut drop = ended.saturating_sub(MAX_ENDED_PER_CHAT);
    list.retain(|grant| {
        if drop > 0 && grant.ended_at.is_some() {
            drop -= 1;
            false
        } else {
            true
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: usize) -> String {
        format!("a{n}")
    }

    #[test]
    fn grants_one_app_to_one_chat() {
        let mut table = GrantTable::default();
        let (grant, evicted) = table
            .grant("c1", "com.apple.calculator", Some("Calculator"), 10, id(1))
            .unwrap();
        assert!(evicted.is_empty());
        assert_eq!(grant.app_name.as_deref(), Some("Calculator"));
        assert_eq!(table.active("c1", 10).len(), 1);
        assert!(
            table.active("c2", 10).is_empty(),
            "another chat holds nothing"
        );
        assert!(table.active("c1", 9).is_empty(), "nothing before the grant");
    }

    #[test]
    fn granting_twice_keeps_one_grant() {
        let mut table = GrantTable::default();
        table
            .grant("c1", "com.apple.calculator", None, 10, id(1))
            .unwrap();
        let (again, _) = table
            .grant("c1", "COM.APPLE.CALCULATOR", None, 20, id(2))
            .unwrap();
        assert_eq!(again.granted_at, 10);
        assert_eq!(table.active("c1", 30).len(), 1);
    }

    #[test]
    fn refuses_monocode_and_bad_ids() {
        let mut table = GrantTable::default();
        assert_eq!(
            table.grant("c1", "com.monocode.desktop", None, 1, id(1)),
            Err(GrantError::App(AppRefusal::Never))
        );
        assert_eq!(
            table.grant("c1", "calc", None, 1, id(1)),
            Err(GrantError::App(AppRefusal::Invalid))
        );
        assert_eq!(
            table.grant("", "com.apple.calculator", None, 1, id(1)),
            Err(GrantError::NoChat)
        );
    }

    #[test]
    fn ends_one_app_or_the_whole_chat() {
        let mut table = GrantTable::default();
        table
            .grant("c1", "com.apple.calculator", None, 10, id(1))
            .unwrap();
        table
            .grant("c1", "com.apple.TextEdit", None, 11, id(2))
            .unwrap();
        let ended = table.end("c1", "com.apple.calculator", 20).unwrap();
        assert_eq!(ended.ended_at, Some(20));
        assert_eq!(table.active("c1", 21).len(), 1);
        assert!(table.end("c1", "com.apple.calculator", 22).is_none());
        let ended = table.end_chat("c1", 30);
        assert_eq!(ended.len(), 1);
        assert!(table.active("c1", 31).is_empty());
        assert_eq!(table.history("c1").len(), 2);
        // A regrant after an end is a new grant.
        let (again, _) = table
            .grant("c1", "com.apple.calculator", None, 40, id(3))
            .unwrap();
        assert_eq!(again.granted_at, 40);
    }

    #[test]
    fn an_end_never_comes_before_the_grant() {
        let mut table = GrantTable::default();
        table
            .grant("c1", "com.apple.calculator", None, 100, id(1))
            .unwrap();
        let ended = table.end("c1", "com.apple.calculator", 50).unwrap();
        assert_eq!(ended.ended_at, Some(100));
    }

    #[test]
    fn caps_apps_per_chat_and_chats() {
        let mut table = GrantTable::default();
        for n in 0..MAX_APPS_PER_CHAT {
            table
                .grant("c1", &format!("com.example.app{n}"), None, 1, id(n))
                .unwrap();
        }
        assert_eq!(
            table.grant("c1", "com.example.more", None, 1, id(99)),
            Err(GrantError::TooMany)
        );
        let mut table = GrantTable::default();
        for n in 0..MAX_CHATS {
            table
                .grant(
                    &format!("c{n}"),
                    "com.apple.calculator",
                    None,
                    n as i64,
                    id(n),
                )
                .unwrap();
        }
        let (_, evicted) = table
            .grant("new", "com.apple.calculator", None, 1000, id(1000))
            .unwrap();
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].chat_id, "c0");
        assert!(table.active("c0", 1001).is_empty());
        assert_eq!(table.chats_with_grants(1001).len(), MAX_CHATS);
    }

    #[test]
    fn end_all_ends_every_chat() {
        let mut table = GrantTable::default();
        table
            .grant("c1", "com.apple.calculator", None, 1, id(1))
            .unwrap();
        table
            .grant("c2", "com.apple.TextEdit", None, 1, id(2))
            .unwrap();
        assert_eq!(table.end_all(5).len(), 2);
        assert!(table.chats_with_grants(6).is_empty());
    }

    #[test]
    fn names_are_one_clean_line() {
        let mut table = GrantTable::default();
        let (grant, _) = table
            .grant(
                "c1",
                "com.apple.calculator",
                Some("Calc\n\u{7}ulator"),
                1,
                id(1),
            )
            .unwrap();
        assert_eq!(grant.app_name.as_deref(), Some("Calculator"));
    }
}
