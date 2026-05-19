use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cc_email::agent::command_runner::{AgentResult, AgentRunner};
use cc_email::config::*;
use cc_email::daemon::process_cycle;
use cc_email::engine::Engine;
use cc_email::error::Result;
use cc_email::inbox::imap_poll::{FetchFuture, InboxAdapter};
use cc_email::mail::parser::ParsedEmail;
use cc_email::mail::reply::ReplyHandler;
use cc_email::security::SecurityGuard;
use cc_email::session::SessionManager;
use cc_email::task::model::Task;
use cc_email::task::store::TaskStore;

fn make_raw_email(message_id: &str, from: &str, subject: &str, body: &str) -> Vec<u8> {
    format!(
        "Message-ID: {message_id}\r\n\
         From: {from}\r\n\
         To: agent@example.com\r\n\
         Subject: {subject}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         \r\n\
         {body}"
    )
    .into_bytes()
}

// --- Mock inbox ---

struct MockInbox {
    messages: Vec<(String, Vec<u8>)>,
}

impl InboxAdapter for MockInbox {
    fn fetch_unseen(&self) -> FetchFuture<'_> {
        let msgs = self.messages.clone();
        Box::pin(async move { Ok(msgs) })
    }
}

// --- Mock agent ---

struct MockAgent {
    stdout: String,
    stderr: String,
    success: bool,
}

impl AgentRunner for MockAgent {
    fn run(
        &self,
        _prompt: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<AgentResult>> + Send + '_>> {
        let result = AgentResult {
            success: self.success,
            stdout: self.stdout.clone(),
            stderr: self.stderr.clone(),
            exit_code: if self.success { Some(0) } else { Some(1) },
            generated_files: Vec::new(),
        };
        Box::pin(async move { Ok(result) })
    }
}

// --- Mock replier ---

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct SentReply {
    to: String,
    subject: String,
    stdout: String,
    stderr: String,
}

struct MockReplier {
    sent: Arc<Mutex<Vec<SentReply>>>,
}

impl ReplyHandler for MockReplier {
    fn send_reply<'a>(
        &'a self,
        original: &'a ParsedEmail,
        task: &'a Task,
        body: &'a str,
        _subject_override: &'a str,
        _attachments: &'a [cc_email::attachment::AttachmentFile],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        self.sent.lock().unwrap().push(SentReply {
            to: original.from.clone(),
            subject: task.subject.clone(),
            stdout: body.to_string(),
            stderr: String::new(),
        });
        Box::pin(async { Ok("<mock@cc-email>".to_string()) })
    }
}

fn tmp_store() -> TaskStore {
    let path = format!("/tmp/cc-email-test-{}.db", uuid::Uuid::new_v4());
    TaskStore::open(&path).unwrap()
}

fn test_config() -> Config {
    Config {
        inbox: InboxConfig {
            inbox_type: "imap".to_string(),
            host: "localhost".to_string(),
            port: 993,
            username: "test".to_string(),
            password: Some("test".to_string()),
            password_env: None,
            folder: "INBOX".to_string(),
            poll_interval_seconds: 30,
            search_to: None,
            search_from: Vec::new(),
        },
        outbox: OutboxConfig {
            outbox_type: "smtp".to_string(),
            host: "localhost".to_string(),
            port: 587,
            username: "test".to_string(),
            password: Some("test".to_string()),
            password_env: None,
            from: "agent@example.com".to_string(),
        },
        agent: AgentConfig {
            agent_type: "command".to_string(),
            command: "echo".to_string(),
            args: vec!["{{prompt}}".to_string()],
            timeout_seconds: 300,
            model: None,
            permission_mode: "auto".to_string(),
            work_dir: None,
            permission_timeout_seconds: 300,
            permission_default: "deny".to_string(),
        },
        security: SecurityConfig::default(),
        session: SessionConfig::default(),
        display: DisplayConfig::default(),
        providers: vec![],
        heartbeat: cc_email::config::HeartbeatConfig::default(),
        webhook: cc_email::webhook::WebhookConfig::default(),
        relay: cc_email::relay::RelayConfig::default(),
        workspace: cc_email::workspace::WorkspaceConfig::default(),
        attachments: cc_email::attachment::AttachmentConfig::default(),
    }
}

fn test_sessions() -> SessionManager {
    let path = format!("/tmp/cc-email-integ-sessions-{}.json", uuid::Uuid::new_v4());
    SessionManager::new(PathBuf::from(path))
}

// ==========================================
// Existing daemon::process_cycle tests
// ==========================================

#[tokio::test]
async fn test_full_cycle_success() {
    let raw = make_raw_email(
        "<test-001@example.com>",
        "user@example.com",
        "Fix the bug",
        "Please fix the login timeout bug",
    );

    let inbox = MockInbox {
        messages: vec![("1".into(), raw)],
    };
    let agent = MockAgent {
        stdout: "Bug fixed successfully.\nAll tests pass.".into(),
        stderr: String::new(),
        success: true,
    };
    let sent = Arc::new(Mutex::new(Vec::new()));
    let replier = MockReplier { sent: sent.clone() };
    let guard = SecurityGuard::new(SecurityConfig {
        allowed_senders: vec!["user@example.com".into()],
        ..Default::default()
    });
    let store = tmp_store();

    let count = process_cycle(&inbox, &agent, &replier, &guard, &store)
        .await
        .unwrap();

    assert_eq!(count, 1);
    assert!(store.is_processed("<test-001@example.com>").unwrap());

    let replies = sent.lock().unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].to, "user@example.com");
    assert_eq!(replies[0].subject, "Fix the bug");
    assert!(replies[0].stdout.contains("Bug fixed"));
}

#[tokio::test]
async fn test_rejected_sender() {
    let raw = make_raw_email(
        "<test-002@example.com>",
        "hacker@evil.com",
        "Drop tables",
        "DROP TABLE users;",
    );

    let inbox = MockInbox {
        messages: vec![("2".into(), raw)],
    };
    let agent = MockAgent {
        stdout: String::new(),
        stderr: String::new(),
        success: true,
    };
    let sent = Arc::new(Mutex::new(Vec::new()));
    let replier = MockReplier { sent: sent.clone() };
    let guard = SecurityGuard::new(SecurityConfig {
        allowed_senders: vec!["user@example.com".into()],
        ..Default::default()
    });
    let store = tmp_store();

    let count = process_cycle(&inbox, &agent, &replier, &guard, &store)
        .await
        .unwrap();

    assert_eq!(count, 0);
    assert!(!store.is_processed("<test-002@example.com>").unwrap());
    assert!(sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn test_duplicate_email_skipped() {
    let raw = make_raw_email(
        "<test-003@example.com>",
        "user@example.com",
        "Do something",
        "Run tests",
    );

    let inbox = MockInbox {
        messages: vec![("3".into(), raw.clone()), ("4".into(), raw)],
    };
    let agent = MockAgent {
        stdout: "done".into(),
        stderr: String::new(),
        success: true,
    };
    let sent = Arc::new(Mutex::new(Vec::new()));
    let replier = MockReplier { sent: sent.clone() };
    let guard = SecurityGuard::new(SecurityConfig::default());
    let store = tmp_store();

    let count = process_cycle(&inbox, &agent, &replier, &guard, &store)
        .await
        .unwrap();

    // Same message-id twice in one batch — only the first is processed
    assert_eq!(count, 1);
    assert_eq!(sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn test_agent_failure_records_failed_status() {
    let raw = make_raw_email(
        "<test-004@example.com>",
        "user@example.com",
        "Build project",
        "cargo build",
    );

    let inbox = MockInbox {
        messages: vec![("5".into(), raw)],
    };
    let agent = MockAgent {
        stdout: String::new(),
        stderr: "error[E0308]: mismatched types".into(),
        success: false,
    };
    let sent = Arc::new(Mutex::new(Vec::new()));
    let replier = MockReplier { sent: sent.clone() };
    let guard = SecurityGuard::new(SecurityConfig::default());
    let store = tmp_store();

    let count = process_cycle(&inbox, &agent, &replier, &guard, &store)
        .await
        .unwrap();

    assert_eq!(count, 1);
    let replies = sent.lock().unwrap();
    assert_eq!(replies.len(), 1);
    assert!(replies[0].stdout.contains("mismatched types"));
}

#[tokio::test]
async fn test_empty_body_rejected() {
    let raw = make_raw_email(
        "<test-005@example.com>",
        "user@example.com",
        "Empty task",
        "   ",
    );

    let inbox = MockInbox {
        messages: vec![("6".into(), raw)],
    };
    let agent = MockAgent {
        stdout: String::new(),
        stderr: String::new(),
        success: true,
    };
    let sent = Arc::new(Mutex::new(Vec::new()));
    let replier = MockReplier { sent: sent.clone() };
    let guard = SecurityGuard::new(SecurityConfig::default());
    let store = tmp_store();

    let count = process_cycle(&inbox, &agent, &replier, &guard, &store)
        .await
        .unwrap();

    assert_eq!(count, 0);
    assert!(sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn test_body_too_large_rejected() {
    let large_body = "x".repeat(100);
    let raw = make_raw_email(
        "<test-006@example.com>",
        "user@example.com",
        "Big task",
        &large_body,
    );

    let inbox = MockInbox {
        messages: vec![("7".into(), raw)],
    };
    let agent = MockAgent {
        stdout: String::new(),
        stderr: String::new(),
        success: true,
    };
    let sent = Arc::new(Mutex::new(Vec::new()));
    let replier = MockReplier { sent: sent.clone() };
    let guard = SecurityGuard::new(SecurityConfig {
        max_body_bytes: 50,
        ..Default::default()
    });
    let store = tmp_store();

    let count = process_cycle(&inbox, &agent, &replier, &guard, &store)
        .await
        .unwrap();

    assert_eq!(count, 0);
    assert!(sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn test_empty_inbox() {
    let inbox = MockInbox { messages: vec![] };
    let agent = MockAgent {
        stdout: String::new(),
        stderr: String::new(),
        success: true,
    };
    let sent = Arc::new(Mutex::new(Vec::new()));
    let replier = MockReplier { sent: sent.clone() };
    let guard = SecurityGuard::new(SecurityConfig::default());
    let store = tmp_store();

    let count = process_cycle(&inbox, &agent, &replier, &guard, &store)
        .await
        .unwrap();

    assert_eq!(count, 0);
    assert!(sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn test_multiple_emails_in_batch() {
    let raw1 = make_raw_email(
        "<batch-1@example.com>",
        "alice@example.com",
        "Task one",
        "First task prompt",
    );
    let raw2 = make_raw_email(
        "<batch-2@example.com>",
        "bob@example.com",
        "Task two",
        "Second task prompt",
    );

    let inbox = MockInbox {
        messages: vec![("10".into(), raw1), ("11".into(), raw2)],
    };
    let agent = MockAgent {
        stdout: "ok".into(),
        stderr: String::new(),
        success: true,
    };
    let sent = Arc::new(Mutex::new(Vec::new()));
    let replier = MockReplier { sent: sent.clone() };
    let guard = SecurityGuard::new(SecurityConfig::default());
    let store = tmp_store();

    let count = process_cycle(&inbox, &agent, &replier, &guard, &store)
        .await
        .unwrap();

    assert_eq!(count, 2);
    let replies = sent.lock().unwrap();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0].to, "alice@example.com");
    assert_eq!(replies[1].to, "bob@example.com");
}

#[tokio::test]
async fn test_second_cycle_skips_already_processed() {
    let raw = make_raw_email(
        "<test-repeat@example.com>",
        "user@example.com",
        "Repeat task",
        "do something",
    );

    let agent = MockAgent {
        stdout: "done".into(),
        stderr: String::new(),
        success: true,
    };
    let sent = Arc::new(Mutex::new(Vec::new()));
    let replier = MockReplier { sent: sent.clone() };
    let guard = SecurityGuard::new(SecurityConfig::default());
    let store = tmp_store();

    // First cycle
    let inbox1 = MockInbox {
        messages: vec![("20".into(), raw.clone())],
    };
    let count1 = process_cycle(&inbox1, &agent, &replier, &guard, &store)
        .await
        .unwrap();
    assert_eq!(count1, 1);

    // Second cycle — same message-id reappears
    let inbox2 = MockInbox {
        messages: vec![("21".into(), raw)],
    };
    let count2 = process_cycle(&inbox2, &agent, &replier, &guard, &store)
        .await
        .unwrap();
    assert_eq!(count2, 0);

    assert_eq!(sent.lock().unwrap().len(), 1);
}

// ==========================================
// New Engine-based integration tests
// ==========================================

#[tokio::test]
async fn test_engine_command_detection_help() {
    let raw = make_raw_email(
        "<eng-help-001@example.com>",
        "user@example.com",
        "Help request",
        "/help",
    );

    let sent = Arc::new(Mutex::new(Vec::new()));
    let mut engine = Engine::new_with_parts(
        test_config(),
        Box::new(MockInbox {
            messages: vec![("1".into(), raw)],
        }),
        Box::new(MockReplier { sent: sent.clone() }),
        Box::new(MockAgent {
            stdout: "agent should not run for commands".into(),
            stderr: String::new(),
            success: true,
        }),
        test_sessions(),
        tmp_store(),
    );

    let count = engine.process_cycle().await.unwrap();
    assert_eq!(count, 1);

    let replies = sent.lock().unwrap();
    assert_eq!(replies.len(), 1);
    assert!(replies[0].stdout.contains("/help"));
    assert!(replies[0].stdout.contains("/new"));
    assert!(replies[0].stdout.contains("/doctor"));
    assert!(!replies[0].stdout.contains("agent should not run"));
}

#[tokio::test]
async fn test_engine_session_creation_and_persistence() {
    let raw1 = make_raw_email(
        "<eng-sess-001@example.com>",
        "user@example.com",
        "First task",
        "do something",
    );
    let raw2 = make_raw_email(
        "<eng-sess-002@example.com>",
        "user@example.com",
        "Second task",
        "do more",
    );

    let mut engine = Engine::new_with_parts(
        test_config(),
        Box::new(MockInbox {
            messages: vec![("1".into(), raw1), ("2".into(), raw2)],
        }),
        Box::new(MockReplier {
            sent: Arc::new(Mutex::new(Vec::new())),
        }),
        Box::new(MockAgent {
            stdout: "ok".into(),
            stderr: String::new(),
            success: true,
        }),
        test_sessions(),
        tmp_store(),
    );

    engine.process_cycle().await.unwrap();

    assert_eq!(engine.sessions.session_count(), 1);
    let session = engine
        .sessions
        .get_active_session("user@example.com")
        .unwrap();
    assert_eq!(session.history.len(), 4);
    assert_eq!(session.history[0].role, "user");
    assert_eq!(session.history[1].role, "assistant");
}

#[tokio::test]
async fn test_engine_multiple_senders_separate_sessions() {
    let raw1 = make_raw_email(
        "<eng-multi-001@example.com>",
        "alice@example.com",
        "Alice task",
        "alice work",
    );
    let raw2 = make_raw_email(
        "<eng-multi-002@example.com>",
        "bob@example.com",
        "Bob task",
        "bob work",
    );

    let mut engine = Engine::new_with_parts(
        test_config(),
        Box::new(MockInbox {
            messages: vec![("1".into(), raw1), ("2".into(), raw2)],
        }),
        Box::new(MockReplier {
            sent: Arc::new(Mutex::new(Vec::new())),
        }),
        Box::new(MockAgent {
            stdout: "ok".into(),
            stderr: String::new(),
            success: true,
        }),
        test_sessions(),
        tmp_store(),
    );

    engine.process_cycle().await.unwrap();

    assert_eq!(engine.sessions.session_count(), 2);

    let alice_session = engine
        .sessions
        .get_active_session("alice@example.com")
        .unwrap();
    let bob_session = engine
        .sessions
        .get_active_session("bob@example.com")
        .unwrap();

    assert_ne!(alice_session.id, bob_session.id);
    assert_eq!(alice_session.sender, "alice@example.com");
    assert_eq!(bob_session.sender, "bob@example.com");
}

#[tokio::test]
async fn test_engine_new_command_creates_session() {
    let raw = make_raw_email(
        "<eng-new-001@example.com>",
        "user@example.com",
        "New session",
        "/new my-project",
    );

    let sent = Arc::new(Mutex::new(Vec::new()));
    let mut engine = Engine::new_with_parts(
        test_config(),
        Box::new(MockInbox {
            messages: vec![("1".into(), raw)],
        }),
        Box::new(MockReplier { sent: sent.clone() }),
        Box::new(MockAgent {
            stdout: "".into(),
            stderr: String::new(),
            success: true,
        }),
        test_sessions(),
        tmp_store(),
    );

    engine.process_cycle().await.unwrap();

    let replies = sent.lock().unwrap();
    assert!(replies[0].stdout.contains("Created new session"));
    assert!(replies[0].stdout.contains("my-project"));

    let sessions = engine.sessions.list_sessions("user@example.com");
    assert!(!sessions.is_empty());
}

#[tokio::test]
async fn test_engine_unknown_command() {
    let raw = make_raw_email(
        "<eng-unk-001@example.com>",
        "user@example.com",
        "Unknown",
        "/nonexistent",
    );

    let sent = Arc::new(Mutex::new(Vec::new()));
    let mut engine = Engine::new_with_parts(
        test_config(),
        Box::new(MockInbox {
            messages: vec![("1".into(), raw)],
        }),
        Box::new(MockReplier { sent: sent.clone() }),
        Box::new(MockAgent {
            stdout: "".into(),
            stderr: String::new(),
            success: true,
        }),
        test_sessions(),
        tmp_store(),
    );

    engine.process_cycle().await.unwrap();

    let replies = sent.lock().unwrap();
    assert!(replies[0].stdout.contains("Unknown command: /nonexistent"));
    assert!(replies[0].stdout.contains("/help"));
}

#[tokio::test]
async fn test_engine_session_persistence_save_load() {
    let session_path = format!("/tmp/cc-email-integ-persist-{}.json", uuid::Uuid::new_v4());

    let raw = make_raw_email(
        "<eng-persist-001@example.com>",
        "user@example.com",
        "Task",
        "/new persistent-session",
    );

    let sessions = SessionManager::new(PathBuf::from(&session_path));
    let mut engine = Engine::new_with_parts(
        test_config(),
        Box::new(MockInbox {
            messages: vec![("1".into(), raw)],
        }),
        Box::new(MockReplier {
            sent: Arc::new(Mutex::new(Vec::new())),
        }),
        Box::new(MockAgent {
            stdout: "".into(),
            stderr: String::new(),
            success: true,
        }),
        sessions,
        tmp_store(),
    );

    engine.process_cycle().await.unwrap();

    let loaded = SessionManager::load(&PathBuf::from(&session_path)).unwrap();
    assert!(loaded.session_count() > 0);

    let sessions_list = loaded.list_sessions("user@example.com");
    let found = sessions_list
        .iter()
        .any(|s| s.name.as_deref() == Some("persistent-session"));
    assert!(found, "Persisted session should be loadable from disk");

    std::fs::remove_file(&session_path).ok();
}

#[tokio::test]
async fn test_engine_doctor_command() {
    let raw = make_raw_email(
        "<ph4-doctor-001@example.com>",
        "user@example.com",
        "Doctor",
        "/doctor",
    );

    let sent = Arc::new(Mutex::new(Vec::new()));
    let mut engine = Engine::new_with_parts(
        test_config(),
        Box::new(MockInbox {
            messages: vec![("1".into(), raw)],
        }),
        Box::new(MockReplier { sent: sent.clone() }),
        Box::new(MockAgent {
            stdout: "".into(),
            stderr: String::new(),
            success: true,
        }),
        test_sessions(),
        tmp_store(),
    );

    engine.process_cycle().await.unwrap();

    let replies = sent.lock().unwrap();
    assert!(replies[0].stdout.contains("Diagnostic Report"));
    assert!(replies[0].stdout.contains("Agent binary"));
    assert!(replies[0].stdout.contains("Sessions"));
}
