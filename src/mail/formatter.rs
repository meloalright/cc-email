use crate::config::DisplayConfig;
use crate::session::Session;

pub struct ReplyFormatter {
    pub show_thinking: bool,
    pub show_tool_use: bool,
    pub show_context_indicator: bool,
    pub reply_footer: bool,
    pub max_output_chars: usize,
    pub max_error_chars: usize,
}

impl ReplyFormatter {
    pub fn from_config(config: &DisplayConfig) -> Self {
        Self {
            show_thinking: config.show_thinking,
            show_tool_use: config.show_tool_use,
            show_context_indicator: true,
            reply_footer: config.reply_footer,
            max_output_chars: config.max_output_chars,
            max_error_chars: config.max_error_chars,
        }
    }

    pub fn format_reply(
        &self,
        _subject: &str,
        success: bool,
        stdout: &str,
        stderr: &str,
        _session: Option<&Session>,
    ) -> String {
        let mut body = String::new();

        if success {
            let output = extract_summary(stdout, self.max_output_chars);
            if !output.is_empty() {
                body.push_str(&output);
            } else {
                body.push_str("Done.");
            }
        } else {
            body.push_str("Task failed.\n\n");
            let err = extract_summary(stderr, self.max_error_chars);
            if !err.is_empty() {
                body.push_str(&err);
            }
        }

        body.push('\n');

        if self.reply_footer {
            body.push_str("\n--\nSent by cc-email\n");
        }

        body
    }

    pub fn format_command_reply(&self, command: &str, output: &str) -> String {
        let mut body = format!("Command: /{}\n\n{}\n", command, output);

        if self.reply_footer {
            body.push_str("\n--\nSent by cc-email\n");
        }

        body
    }
}

impl Default for ReplyFormatter {
    fn default() -> Self {
        Self {
            show_thinking: false,
            show_tool_use: true,
            show_context_indicator: true,
            reply_footer: true,
            max_output_chars: 1500,
            max_error_chars: 1000,
        }
    }
}

fn extract_summary(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.len() <= max {
        trimmed.to_string()
    } else {
        let cut = &trimmed[..max];
        if let Some(pos) = cut.rfind('\n') {
            cut[..pos].to_string()
        } else {
            format!("{}...", cut)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Session, SessionStatus};
    use chrono::Utc;

    fn make_session() -> Session {
        Session {
            id: "s1".to_string(),
            name: Some("my-session".to_string()),
            sender: "user@example.com".to_string(),
            history: vec![],
            status: SessionStatus::Idle,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            agent_session_id: None,
        }
    }

    #[test]
    fn test_format_reply_success() {
        let fmt = ReplyFormatter::default();
        let session = make_session();
        let result = fmt.format_reply("Fix bug", true, "done", "", Some(&session));
        assert!(result.contains("done"));
        assert!(result.contains("Sent by cc-email"));
        assert!(!result.contains("--- Output ---"));
    }

    #[test]
    fn test_format_reply_failure() {
        let fmt = ReplyFormatter::default();
        let result = fmt.format_reply("Build", false, "", "error", None);
        assert!(result.contains("Task failed"));
        assert!(result.contains("error"));
    }

    #[test]
    fn test_format_reply_no_footer() {
        let fmt = ReplyFormatter {
            reply_footer: false,
            ..Default::default()
        };
        let result = fmt.format_reply("Task", true, "ok", "", None);
        assert!(!result.contains("Sent by cc-email"));
    }

    #[test]
    fn test_format_reply_empty_output() {
        let fmt = ReplyFormatter::default();
        let result = fmt.format_reply("Task", true, "", "", None);
        assert!(result.contains("Done."));
    }

    #[test]
    fn test_format_command_reply() {
        let fmt = ReplyFormatter::default();
        let result = fmt.format_command_reply("help", "Available commands:\n  /help");
        assert!(result.contains("Command: /help"));
        assert!(result.contains("Available commands"));
    }

    #[test]
    fn test_extract_summary() {
        assert_eq!(extract_summary("hello", 10), "hello");
        assert_eq!(extract_summary("hello world", 5), "hello...");
        assert_eq!(extract_summary("line1\nline2\nline3", 12), "line1\nline2");
    }
}
