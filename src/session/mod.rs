use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{CcEmailError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SessionStatus {
    Idle,
    Busy,
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionStatus::Idle => write!(f, "idle"),
            SessionStatus::Busy => write!(f, "busy"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub name: Option<String>,
    pub sender: String,
    pub history: Vec<HistoryEntry>,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub agent_session_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionManager {
    sessions: HashMap<String, Session>,
    active_session: HashMap<String, String>,
    counter: u64,
    store_path: PathBuf,
    #[serde(default)]
    message_id_map: HashMap<String, String>,
}

impl SessionManager {
    pub fn new(store_path: PathBuf) -> Self {
        Self {
            sessions: HashMap::new(),
            active_session: HashMap::new(),
            counter: 0,
            store_path,
            message_id_map: HashMap::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new(path.to_path_buf()));
        }
        let content = std::fs::read_to_string(path)
            .map_err(|e| CcEmailError::Config(format!("failed to read sessions file: {}", e)))?;
        let mut mgr: SessionManager = serde_json::from_str(&content)
            .map_err(|e| CcEmailError::Config(format!("failed to parse sessions: {}", e)))?;
        mgr.store_path = path.to_path_buf();
        Ok(mgr)
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.store_path.parent() {
            std::fs::create_dir_all(parent).map_err(CcEmailError::Io)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| CcEmailError::Config(format!("failed to serialize sessions: {}", e)))?;
        std::fs::write(&self.store_path, json)?;
        Ok(())
    }

    pub fn get_or_create_active(&mut self, sender: &str) -> &mut Session {
        let sender_lower = sender.to_lowercase();
        if let Some(session_id) = self.active_session.get(&sender_lower).cloned() {
            if self.sessions.contains_key(&session_id) {
                return self.sessions.get_mut(&session_id).unwrap();
            }
        }

        let id = self.next_id();
        let session = Session {
            id: id.clone(),
            name: None,
            sender: sender_lower.clone(),
            history: Vec::new(),
            status: SessionStatus::Idle,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            agent_session_id: None,
        };
        self.sessions.insert(id.clone(), session);
        self.active_session.insert(sender_lower, id.clone());
        self.sessions.get_mut(&id).unwrap()
    }

    pub fn new_session(&mut self, sender: &str, name: Option<&str>) -> String {
        let sender_lower = sender.to_lowercase();
        let id = self.next_id();
        let session = Session {
            id: id.clone(),
            name: name.map(|s| s.to_string()),
            sender: sender_lower.clone(),
            history: Vec::new(),
            status: SessionStatus::Idle,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            agent_session_id: None,
        };
        self.sessions.insert(id.clone(), session);
        self.active_session.insert(sender_lower, id.clone());
        id
    }

    pub fn switch_session(&mut self, sender: &str, target: &str) -> Result<()> {
        let sender_lower = sender.to_lowercase();

        let session_id = self
            .sessions
            .iter()
            .find(|(id, s)| {
                s.sender == sender_lower && (*id == target || s.name.as_deref() == Some(target))
            })
            .map(|(id, _)| id.clone())
            .ok_or_else(|| CcEmailError::Config(format!("session '{}' not found", target)))?;

        self.active_session.insert(sender_lower, session_id);
        Ok(())
    }

    pub fn list_sessions(&self, sender: &str) -> Vec<&Session> {
        let sender_lower = sender.to_lowercase();
        self.sessions
            .values()
            .filter(|s| s.sender == sender_lower)
            .collect()
    }

    pub fn delete_session(&mut self, id: &str) -> Result<()> {
        let session = self
            .sessions
            .remove(id)
            .ok_or_else(|| CcEmailError::Config(format!("session '{}' not found", id)))?;

        if let Some(active_id) = self.active_session.get(&session.sender) {
            if active_id == id {
                self.active_session.remove(&session.sender);
            }
        }

        Ok(())
    }

    pub fn add_history(&mut self, session_id: &str, role: &str, content: &str) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.history.push(HistoryEntry {
                role: role.to_string(),
                content: content.to_string(),
                timestamp: Utc::now(),
            });
            session.updated_at = Utc::now();
        }
    }

    pub fn set_status(&mut self, session_id: &str, status: SessionStatus) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.status = status;
            session.updated_at = Utc::now();
        }
    }

    pub fn get_active_session(&self, sender: &str) -> Option<&Session> {
        let sender_lower = sender.to_lowercase();
        self.active_session
            .get(&sender_lower)
            .and_then(|id| self.sessions.get(id))
    }

    pub fn get_session(&self, id: &str) -> Option<&Session> {
        self.sessions.get(id)
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn all_senders(&self) -> Vec<String> {
        let mut senders: Vec<String> = self
            .sessions
            .values()
            .map(|s| s.sender.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        senders.sort();
        senders
    }

    pub fn register_message_id(&mut self, message_id: &str, session_id: &str) {
        self.message_id_map
            .insert(message_id.to_string(), session_id.to_string());
    }

    pub fn resolve_session_by_message_id(
        &self,
        in_reply_to: Option<&str>,
        references: Option<&str>,
    ) -> Option<&str> {
        if let Some(reply_to) = in_reply_to {
            if let Some(session_id) = self.message_id_map.get(reply_to) {
                return Some(session_id.as_str());
            }
        }
        if let Some(refs) = references {
            for msg_id in refs.split_whitespace().rev() {
                if let Some(session_id) = self.message_id_map.get(msg_id) {
                    return Some(session_id.as_str());
                }
            }
        }
        None
    }

    pub fn resolve_or_create_session(
        &mut self,
        sender: &str,
        in_reply_to: Option<&str>,
        references: Option<&str>,
    ) -> String {
        if let Some(session_id) = self.resolve_session_by_message_id(in_reply_to, references) {
            let session_id = session_id.to_string();
            let sender_lower = sender.to_lowercase();
            self.active_session.insert(sender_lower, session_id.clone());
            return session_id;
        }
        let session = self.get_or_create_active(sender);
        session.id.clone()
    }

    pub fn name_session(&mut self, session_id: &str, name: &str) -> Result<()> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| CcEmailError::Config(format!("session '{}' not found", session_id)))?;
        session.name = Some(name.to_string());
        session.updated_at = Utc::now();
        Ok(())
    }

    fn next_id(&mut self) -> String {
        self.counter += 1;
        format!("s{}", self.counter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manager() -> SessionManager {
        SessionManager::new(PathBuf::from("/tmp/test-sessions.json"))
    }

    #[test]
    fn test_get_or_create_active() {
        let mut mgr = test_manager();
        let session = mgr.get_or_create_active("user@example.com");
        assert_eq!(session.id, "s1");
        assert_eq!(session.sender, "user@example.com");

        let session = mgr.get_or_create_active("user@example.com");
        assert_eq!(session.id, "s1");
    }

    #[test]
    fn test_new_session() {
        let mut mgr = test_manager();
        let id1 = mgr.new_session("user@example.com", Some("project-a"));
        let id2 = mgr.new_session("user@example.com", None);
        assert_ne!(id1, id2);

        let active = mgr.get_active_session("user@example.com").unwrap();
        assert_eq!(active.id, id2);
    }

    #[test]
    fn test_switch_session() {
        let mut mgr = test_manager();
        let id1 = mgr.new_session("user@example.com", Some("project-a"));
        let _id2 = mgr.new_session("user@example.com", None);

        mgr.switch_session("user@example.com", &id1).unwrap();
        let active = mgr.get_active_session("user@example.com").unwrap();
        assert_eq!(active.id, id1);

        mgr.switch_session("user@example.com", "project-a").unwrap();
        let active = mgr.get_active_session("user@example.com").unwrap();
        assert_eq!(active.name.as_deref(), Some("project-a"));
    }

    #[test]
    fn test_list_sessions() {
        let mut mgr = test_manager();
        mgr.new_session("alice@example.com", None);
        mgr.new_session("alice@example.com", None);
        mgr.new_session("bob@example.com", None);

        assert_eq!(mgr.list_sessions("alice@example.com").len(), 2);
        assert_eq!(mgr.list_sessions("bob@example.com").len(), 1);
    }

    #[test]
    fn test_delete_session() {
        let mut mgr = test_manager();
        let id = mgr.new_session("user@example.com", None);
        assert!(mgr.get_session(&id).is_some());

        mgr.delete_session(&id).unwrap();
        assert!(mgr.get_session(&id).is_none());
    }

    #[test]
    fn test_history() {
        let mut mgr = test_manager();
        let session = mgr.get_or_create_active("user@example.com");
        let id = session.id.clone();

        mgr.add_history(&id, "user", "hello");
        mgr.add_history(&id, "assistant", "hi there");

        let session = mgr.get_session(&id).unwrap();
        assert_eq!(session.history.len(), 2);
        assert_eq!(session.history[0].role, "user");
        assert_eq!(session.history[1].content, "hi there");
    }

    #[test]
    fn test_message_id_mapping() {
        let mut mgr = test_manager();
        let id = mgr.new_session("user@example.com", None);
        mgr.register_message_id("<reply-001@example.com>", &id);

        let resolved = mgr.resolve_session_by_message_id(Some("<reply-001@example.com>"), None);
        assert_eq!(resolved, Some(id.as_str()));
    }

    #[test]
    fn test_separate_senders() {
        let mut mgr = test_manager();
        let s1 = mgr.get_or_create_active("alice@example.com");
        let id1 = s1.id.clone();
        let s2 = mgr.get_or_create_active("bob@example.com");
        let id2 = s2.id.clone();

        assert_ne!(id1, id2);
        assert_eq!(mgr.get_active_session("alice@example.com").unwrap().id, id1);
        assert_eq!(mgr.get_active_session("bob@example.com").unwrap().id, id2);
    }

    #[test]
    fn test_save_load() {
        let path = PathBuf::from(format!(
            "/tmp/cc-email-test-sessions-{}.json",
            uuid::Uuid::new_v4()
        ));
        let mut mgr = SessionManager::new(path.clone());
        mgr.new_session("user@example.com", Some("my-session"));
        mgr.add_history("s1", "user", "hello");
        mgr.save().unwrap();

        let loaded = SessionManager::load(&path).unwrap();
        assert_eq!(loaded.session_count(), 1);
        let session = loaded.get_session("s1").unwrap();
        assert_eq!(session.name.as_deref(), Some("my-session"));
        assert_eq!(session.history.len(), 1);

        std::fs::remove_file(&path).ok();
    }
}
