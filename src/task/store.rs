use chrono::Utc;
use rusqlite::Connection;

use crate::error::Result;
use crate::task::model::{Task, TaskStatus};

pub struct TaskStore {
    conn: Connection,
}

impl TaskStore {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                email_message_id TEXT NOT NULL UNIQUE,
                sender TEXT NOT NULL,
                subject TEXT NOT NULL,
                prompt TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                result_summary TEXT,
                raw_log_path TEXT
            );",
        )?;
        Ok(())
    }

    pub fn is_processed(&self, message_id: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE email_message_id = ?1",
            [message_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn insert(&self, task: &Task) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tasks (id, email_message_id, sender, subject, prompt, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                task.id,
                task.email_message_id,
                task.from,
                task.subject,
                task.prompt,
                task.status.as_str(),
                task.created_at.to_rfc3339(),
                task.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn update_status(
        &self,
        task_id: &str,
        status: TaskStatus,
        summary: Option<&str>,
        log_path: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET status = ?1, result_summary = ?2, raw_log_path = ?3, updated_at = ?4 WHERE id = ?5",
            rusqlite::params![
                status.as_str(),
                summary,
                log_path,
                Utc::now().to_rfc3339(),
                task_id,
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store() -> TaskStore {
        TaskStore::open(":memory:").unwrap()
    }

    #[test]
    fn test_open_and_migrate() {
        let store = tmp_store();
        assert!(!store.is_processed("<nonexistent>").unwrap());
    }

    #[test]
    fn test_insert_and_is_processed() {
        let store = tmp_store();
        let task = Task::new("<msg1>".into(), "u@t.com".into(), "S".into(), "P".into());
        store.insert(&task).unwrap();
        assert!(store.is_processed("<msg1>").unwrap());
        assert!(!store.is_processed("<msg2>").unwrap());
    }

    #[test]
    fn test_duplicate_insert_fails() {
        let store = tmp_store();
        let task = Task::new("<dup>".into(), "u@t.com".into(), "S".into(), "P".into());
        store.insert(&task).unwrap();
        let task2 = Task::new("<dup>".into(), "u@t.com".into(), "S".into(), "P".into());
        assert!(store.insert(&task2).is_err());
    }

    #[test]
    fn test_update_status() {
        let store = tmp_store();
        let task = Task::new("<upd>".into(), "u@t.com".into(), "S".into(), "P".into());
        store.insert(&task).unwrap();
        store
            .update_status(&task.id, TaskStatus::Completed, Some("done"), Some("/tmp/log"))
            .unwrap();
    }
}
