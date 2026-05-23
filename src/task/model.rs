use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Running => "running",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "pending" => TaskStatus::Pending,
            "running" => TaskStatus::Running,
            "completed" => TaskStatus::Completed,
            "failed" => TaskStatus::Failed,
            _ => TaskStatus::Pending,
        }
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub email_message_id: String,
    pub from: String,
    pub subject: String,
    pub prompt: String,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub result_summary: Option<String>,
    pub raw_log_path: Option<String>,
}

impl Task {
    pub fn new(email_message_id: String, from: String, subject: String, prompt: String) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            email_message_id,
            from,
            subject,
            prompt,
            status: TaskStatus::Pending,
            created_at: now,
            updated_at: now,
            result_summary: None,
            raw_log_path: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_roundtrip() {
        for status in [
            TaskStatus::Pending,
            TaskStatus::Running,
            TaskStatus::Completed,
            TaskStatus::Failed,
        ] {
            assert_eq!(TaskStatus::parse(status.as_str()), status);
        }
    }

    #[test]
    fn test_status_display() {
        assert_eq!(format!("{}", TaskStatus::Completed), "completed");
        assert_eq!(format!("{}", TaskStatus::Failed), "failed");
    }

    #[test]
    fn test_unknown_status_defaults_to_pending() {
        assert_eq!(TaskStatus::parse("unknown"), TaskStatus::Pending);
    }

    #[test]
    fn test_task_new_defaults() {
        let task = Task::new(
            "<msg@test>".into(),
            "user@test.com".into(),
            "Test".into(),
            "do something".into(),
        );
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.from, "user@test.com");
        assert!(task.result_summary.is_none());
        assert!(task.raw_log_path.is_none());
        assert!(!task.id.is_empty());
    }

    #[test]
    fn test_task_ids_are_unique() {
        let t1 = Task::new("<a>".into(), "u".into(), "s".into(), "p".into());
        let t2 = Task::new("<b>".into(), "u".into(), "s".into(), "p".into());
        assert_ne!(t1.id, t2.id);
    }
}
