use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::agent::claude_code::{ClaudeCodeAgent, PermissionResponse};
use crate::agent::command_runner::{AgentResult, AgentRunner, CommandAgent};
use crate::command::builtins;
use crate::command::{detect_and_parse_command, CommandRegistry};
use crate::config::Config;
use crate::cron::CronScheduler;
use crate::diagnostics;
use crate::error::{CcEmailError, Result};
use crate::inbox::imap_poll::{ImapPoller, InboxAdapter};
use crate::mail::formatter::ReplyFormatter;
use crate::mail::parser::{parse_email, ParsedEmail};
use crate::mail::reply::{ReplyHandler, SmtpReplier};
use crate::permission::{
    self, match_permission_response, PendingPermission, PermissionDecision, PermissionRequest,
};
use crate::relay::RelayManager;
use crate::security::SecurityGuard;
use crate::session::{SessionManager, SessionStatus};
use crate::task::model::{Task, TaskStatus};
use crate::task::store::TaskStore;
use crate::webhook::WebhookServer;
use crate::workspace::WorkspaceRouter;

pub struct InFlightTask {
    pub task: Task,
    pub original_email: ParsedEmail,
    pub session_id: String,
    pub join_handle: tokio::task::JoinHandle<Result<AgentResult>>,
    pub perm_req_rx: tokio::sync::mpsc::Receiver<PermissionRequest>,
    pub perm_resp_tx: tokio::sync::mpsc::Sender<PermissionResponse>,
    pub pending_perm: Option<PendingPermission>,
    pub perm_message_id: Option<String>,
    pub approve_all: bool,
    pub agent_started_at: std::time::SystemTime,
}

pub struct Engine {
    pub config: Config,
    inbox: Box<dyn InboxAdapter>,
    outbox: Box<dyn ReplyHandler>,
    agent: Box<dyn AgentRunner>,
    claude_agent: Option<Arc<ClaudeCodeAgent>>,
    pub sessions: SessionManager,
    security: SecurityGuard,
    pub commands: CommandRegistry,
    pub formatter: ReplyFormatter,
    store: TaskStore,
    started_at: DateTime<Utc>,
    pub cron: CronScheduler,
    pub relay: RelayManager,
    pub webhook: WebhookServer,
    pub workspace: WorkspaceRouter,
    in_flight: Option<InFlightTask>,
}

impl Engine {
    pub fn new(config: Config) -> Result<Self> {
        let data_dir = PathBuf::from(&config.session.data_dir);
        std::fs::create_dir_all(&data_dir)?;

        let sessions_path = data_dir.join("sessions.json");
        let sessions = SessionManager::load(&sessions_path)
            .unwrap_or_else(|_| SessionManager::new(sessions_path));

        let db_path = data_dir.join("cc-email.db");
        let store = TaskStore::open(db_path.to_str().unwrap_or("cc-email.db"))?;

        let guard = SecurityGuard::new(config.security.clone());
        let inbox: Box<dyn InboxAdapter> = Box::new(ImapPoller::new(config.inbox.clone()));
        let mut claude_agent: Option<Arc<ClaudeCodeAgent>> = None;
        let agent: Box<dyn AgentRunner> = match config.agent.agent_type.as_str() {
            "claude-code" => {
                let binary = if config.agent.command.is_empty() {
                    "claude".to_string()
                } else {
                    config.agent.command.clone()
                };
                let work_dir = config
                    .agent
                    .work_dir
                    .as_ref()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let ca = Arc::new(ClaudeCodeAgent::new(
                    binary,
                    work_dir,
                    config.agent.model.clone(),
                    config.agent.permission_mode.clone(),
                    config.agent.timeout_seconds,
                ));
                if config.agent.permission_mode == "email" {
                    claude_agent = Some(ca.clone());
                }
                Box::new(ca)
            }
            _ => Box::new(CommandAgent::new(config.agent.clone())),
        };
        let replier: Box<dyn ReplyHandler> = Box::new(SmtpReplier::new(config.outbox.clone()));
        let formatter = ReplyFormatter::from_config(&config.display);
        let commands = CommandRegistry::new();
        let cron = CronScheduler::load(&data_dir).unwrap_or_else(|_| CronScheduler::new(&data_dir));
        let relay = RelayManager::new(&config.relay);
        let webhook = WebhookServer::new(config.webhook.clone());
        let workspace = WorkspaceRouter::new(config.workspace.clone());

        Ok(Self {
            config,
            inbox,
            outbox: replier,
            agent,
            claude_agent,
            sessions,
            security: guard,
            commands,
            formatter,
            store,
            started_at: Utc::now(),
            cron,
            relay,
            webhook,
            workspace,
            in_flight: None,
        })
    }

    pub fn new_with_parts(
        config: Config,
        inbox: Box<dyn InboxAdapter>,
        outbox: Box<dyn ReplyHandler>,
        agent: Box<dyn AgentRunner>,
        sessions: SessionManager,
        store: TaskStore,
    ) -> Self {
        let guard = SecurityGuard::new(config.security.clone());
        let formatter = ReplyFormatter::from_config(&config.display);
        let commands = CommandRegistry::new();
        let cron = CronScheduler::new(std::path::Path::new("/tmp"));
        let relay = RelayManager::new(&config.relay);
        let webhook = WebhookServer::new(config.webhook.clone());
        let workspace = WorkspaceRouter::new(config.workspace.clone());

        Self {
            config,
            inbox,
            outbox,
            agent,
            claude_agent: None,
            sessions,
            security: guard,
            commands,
            formatter,
            store,
            started_at: Utc::now(),
            cron,
            relay,
            webhook,
            workspace,
            in_flight: None,
        }
    }

    pub async fn start(&mut self) -> Result<()> {
        let poll_interval = std::time::Duration::from_secs(self.config.inbox.poll_interval_seconds);

        tracing::info!("cc-email engine started");

        loop {
            if let Err(e) = self.check_in_flight().await {
                tracing::error!(error = %e, "in-flight check error");
            }

            match self.process_cycle().await {
                Ok(count) => {
                    if count > 0 {
                        tracing::info!(processed = count, "cycle complete");
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "cycle error");
                }
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    async fn check_in_flight(&mut self) -> Result<usize> {
        let inflight = match self.in_flight.as_mut() {
            Some(t) => t,
            None => return Ok(0),
        };

        // Check for new permission requests from the agent (non-blocking)
        if let Ok(perm_req) = inflight.perm_req_rx.try_recv() {
            if inflight.approve_all {
                tracing::info!(tool = %perm_req.tool_name, "auto-approving (allow all mode)");
                let _ = inflight.perm_resp_tx.try_send(PermissionResponse {
                    id: perm_req.id,
                    decision: PermissionDecision::Allow,
                });
            } else {
                tracing::info!(tool = %perm_req.tool_name, "permission request, sending email");
                let perm_body = permission::format_permission_email(&perm_req);
                let perm_subject = format!(
                    "Re: {} — Permission Request",
                    inflight.original_email.subject
                );
                match self
                    .outbox
                    .send_reply(
                        &inflight.original_email,
                        &inflight.task,
                        &perm_body,
                        &perm_subject,
                        &[],
                    )
                    .await
                {
                    Ok(sent_id) => {
                        inflight.perm_message_id = Some(sent_id);
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to send permission email");
                    }
                }
                inflight.pending_perm = Some(PendingPermission {
                    request: perm_req,
                    asked_at: Utc::now(),
                });
            }
        }

        // Check for permission timeout
        if let Some(ref perm) = inflight.pending_perm {
            let timeout_secs = self.config.agent.permission_timeout_seconds;
            let elapsed = (Utc::now() - perm.asked_at).num_seconds() as u64;
            if elapsed > timeout_secs {
                tracing::warn!("permission request timed out, applying default");
                let decision = if self.config.agent.permission_default == "allow" {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Deny
                };
                let _ = inflight.perm_resp_tx.try_send(PermissionResponse {
                    id: perm.request.id.clone(),
                    decision,
                });
                inflight.pending_perm = None;
            }
        }

        // Check if agent completed
        if inflight.join_handle.is_finished() {
            let inflight = self.in_flight.take().unwrap();
            let result = inflight
                .join_handle
                .await
                .map_err(|e| CcEmailError::Agent(format!("agent task panicked: {}", e)))?;

            self.finalize_agent_result(
                result,
                inflight.task,
                &inflight.original_email,
                &inflight.session_id,
                inflight.agent_started_at,
            )
            .await?;
            return Ok(1);
        }

        Ok(0)
    }

    async fn finalize_agent_result(
        &mut self,
        result: Result<AgentResult>,
        mut task: Task,
        original_email: &ParsedEmail,
        session_id: &str,
        agent_started_at: std::time::SystemTime,
    ) -> Result<()> {
        let (stdout, stderr, _generated_files) = match &result {
            Ok(r) => {
                let log_path = save_log(&task.id, &r.stdout, &r.stderr);
                if r.success {
                    let summary = r.stdout.lines().take(5).collect::<Vec<_>>().join("\n");
                    task.status = TaskStatus::Completed;
                    task.result_summary = Some(summary);
                    task.raw_log_path = log_path;
                    self.store.update_status(
                        &task.id,
                        TaskStatus::Completed,
                        task.result_summary.as_deref(),
                        task.raw_log_path.as_deref(),
                    )?;
                } else {
                    let summary = format!(
                        "exit code: {:?}\n{}",
                        r.exit_code,
                        r.stderr.lines().take(5).collect::<Vec<_>>().join("\n")
                    );
                    task.status = TaskStatus::Failed;
                    task.result_summary = Some(summary);
                    task.raw_log_path = log_path;
                    self.store.update_status(
                        &task.id,
                        TaskStatus::Failed,
                        task.result_summary.as_deref(),
                        task.raw_log_path.as_deref(),
                    )?;
                }
                (
                    r.stdout.clone(),
                    r.stderr.clone(),
                    r.generated_files.clone(),
                )
            }
            Err(e) => {
                let summary = format!("agent error: {}", e);
                task.status = TaskStatus::Failed;
                task.result_summary = Some(summary.clone());
                self.store
                    .update_status(&task.id, TaskStatus::Failed, Some(&summary), None)?;
                (String::new(), summary, Vec::new())
            }
        };

        let session = self.sessions.get_session(session_id);
        let formatted = self.formatter.format_reply(
            &task.subject,
            task.status == TaskStatus::Completed,
            &stdout,
            &stderr,
            session,
        );

        let work_dir = self
            .config
            .agent
            .work_dir
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let attachments = crate::attachment::scan_new_files(
            &work_dir,
            agent_started_at,
            &self.config.attachments,
        );
        tracing::info!(count = attachments.len(), "attachments collected for reply");

        self.sessions.add_history(session_id, "assistant", &stdout);
        self.sessions.set_status(session_id, SessionStatus::Idle);
        self.sessions
            .register_message_id(&original_email.message_id, session_id);
        self.sessions.save().ok();

        let result_subject = if task.status == TaskStatus::Completed {
            format!("Re: {} — Done", task.subject)
        } else {
            format!("Re: {} — Failed", task.subject)
        };

        if let Err(e) = self
            .outbox
            .send_reply(
                original_email,
                &task,
                &formatted,
                &result_subject,
                &attachments,
            )
            .await
        {
            tracing::error!(error = %e, "failed to send reply");
        }

        tracing::info!(task_id = %task.id, "in-flight task completed");
        Ok(())
    }

    pub async fn process_cycle(&mut self) -> Result<usize> {
        let messages = self.inbox.fetch_unseen().await?;
        let mut processed = 0;

        for (_uid, raw) in &messages {
            let parsed = match parse_email(raw) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(error = %e, "skipping unparseable email");
                    continue;
                }
            };

            if parsed.message_id.is_empty() {
                tracing::warn!("skipping email with no message-id");
                continue;
            }

            if self.store.is_processed(&parsed.message_id)? {
                tracing::debug!(id = %parsed.message_id, "already processed, skipping");
                continue;
            }

            if let Err(e) = self.security.validate_sender(&parsed.from) {
                tracing::warn!(from = %parsed.from, error = %e, "sender rejected");
                continue;
            }

            let body = if !parsed.text_body.is_empty() {
                parsed.text_body.clone()
            } else if let Some(ref html) = parsed.html_body {
                html.clone()
            } else {
                tracing::warn!(id = %parsed.message_id, "no body found");
                continue;
            };

            if let Err(e) = self.security.validate_body(&body) {
                tracing::warn!(error = %e, "body validation failed");
                continue;
            }

            if body.starts_with("--\nSent by cc-email") || body.starts_with("-- \nSent by cc-email")
            {
                tracing::debug!(id = %parsed.message_id, "skipping own reply (loop prevention)");
                continue;
            }

            // Check if this email is a permission response for an in-flight task
            if let Some(ref mut inflight) = self.in_flight {
                let same_sender =
                    parsed.from.to_lowercase() == inflight.original_email.from.to_lowercase();
                let thread_ids: Vec<String> = [
                    Some(inflight.original_email.message_id.as_str()),
                    inflight.perm_message_id.as_deref(),
                ]
                .into_iter()
                .flatten()
                .map(|id| id.trim_matches(|c| c == '<' || c == '>').to_string())
                .collect();
                let is_reply_thread = parsed
                    .in_reply_to
                    .as_ref()
                    .map(|r| thread_ids.iter().any(|id| r.contains(id)))
                    .unwrap_or(false)
                    || parsed
                        .references
                        .as_ref()
                        .map(|r| thread_ids.iter().any(|id| r.contains(id)))
                        .unwrap_or(false);
                if inflight.pending_perm.is_some() && same_sender {
                    tracing::info!(
                        in_reply_to = ?parsed.in_reply_to,
                        references = ?parsed.references,
                        thread_ids = ?thread_ids,
                        is_reply_thread = is_reply_thread,
                        "permission reply candidate"
                    );
                }
                if inflight.pending_perm.is_some() && same_sender && is_reply_thread {
                    if let Some(decision) = match_permission_response(&body) {
                        let perm = inflight.pending_perm.take().unwrap();
                        tracing::info!(
                            decision = ?format!("{:?}", decision),
                            tool = %perm.request.tool_name,
                            "permission response received"
                        );

                        if matches!(decision, PermissionDecision::AllowAll) {
                            inflight.approve_all = true;
                        }

                        let _ = inflight.perm_resp_tx.try_send(PermissionResponse {
                            id: perm.request.id.clone(),
                            decision,
                        });

                        let mut perm_task = Task::new(
                            parsed.message_id.clone(),
                            parsed.from.clone(),
                            parsed.subject.clone(),
                            body.clone(),
                        );
                        perm_task.status = TaskStatus::Completed;
                        perm_task.result_summary = Some("permission response".to_string());
                        self.store.insert(&perm_task)?;
                        self.store.update_status(
                            &perm_task.id,
                            TaskStatus::Completed,
                            perm_task.result_summary.as_deref(),
                            None,
                        )?;

                        processed += 1;
                        continue;
                    }
                }
            }

            // Drop stale permission replies that arrived after the task completed
            if match_permission_response(&body).is_some()
                && parsed.subject.contains("Permission Request")
            {
                tracing::debug!(id = %parsed.message_id, "skipping stale permission reply");
                continue;
            }

            let session_id = self.sessions.resolve_or_create_session(
                &parsed.from,
                parsed.in_reply_to.as_deref(),
                parsed.references.as_deref(),
            );

            if let Some((cmd, args)) = detect_and_parse_command(&body) {
                let reply_body = self.handle_command(&cmd, &args, &parsed.from);

                let mut task = Task::new(
                    parsed.message_id.clone(),
                    parsed.from.clone(),
                    parsed.subject.clone(),
                    body.clone(),
                );
                task.status = TaskStatus::Completed;
                task.result_summary = Some(format!("command: /{}", cmd));
                self.store.insert(&task)?;
                self.store.update_status(
                    &task.id,
                    TaskStatus::Completed,
                    task.result_summary.as_deref(),
                    None,
                )?;

                let formatted = self.formatter.format_command_reply(&cmd, &reply_body);

                if let Err(e) = self
                    .outbox
                    .send_reply(&parsed, &task, &formatted, "", &[])
                    .await
                {
                    tracing::error!(error = %e, "failed to send command reply");
                }

                self.sessions.add_history(&session_id, "user", &body);
                self.sessions
                    .add_history(&session_id, "assistant", &reply_body);
                self.sessions.save().ok();

                processed += 1;
                continue;
            }

            // Skip new agent tasks if one is already in-flight
            if self.in_flight.is_some() {
                tracing::warn!("agent task already in-flight, skipping new task");
                continue;
            }

            let mut task = Task::new(
                parsed.message_id.clone(),
                parsed.from.clone(),
                parsed.subject.clone(),
                body.clone(),
            );

            self.store.insert(&task)?;
            tracing::info!(task_id = %task.id, subject = %task.subject, "task created");

            self.store
                .update_status(&task.id, TaskStatus::Running, None, None)?;
            task.status = TaskStatus::Running;

            self.sessions.set_status(&session_id, SessionStatus::Busy);
            self.sessions.add_history(&session_id, "user", &body);

            // Use interactive permission path if configured
            if let Some(ref claude) = self.claude_agent {
                let claude = claude.clone();
                let prompt = task.prompt.clone();
                let (perm_req_tx, perm_req_rx) = tokio::sync::mpsc::channel(4);
                let (perm_resp_tx, perm_resp_rx) = tokio::sync::mpsc::channel(4);

                let join_handle = tokio::spawn(async move {
                    claude
                        .run_with_permissions(&prompt, perm_req_tx, perm_resp_rx)
                        .await
                });

                self.in_flight = Some(InFlightTask {
                    task,
                    original_email: parsed.clone(),
                    session_id: session_id.clone(),
                    join_handle,
                    perm_req_rx,
                    perm_resp_tx,
                    pending_perm: None,
                    perm_message_id: None,
                    approve_all: false,
                    agent_started_at: std::time::SystemTime::now(),
                });

                tracing::info!("agent task spawned with permission support");
                processed += 1;
                continue;
            }

            // Regular (non-interactive) agent run
            let result = self.agent.run(&task.prompt).await;

            let (stdout, stderr) = match &result {
                Ok(r) => {
                    let log_path = save_log(&task.id, &r.stdout, &r.stderr);

                    if r.success {
                        let summary = r.stdout.lines().take(5).collect::<Vec<_>>().join("\n");
                        task.status = TaskStatus::Completed;
                        task.result_summary = Some(summary);
                        task.raw_log_path = log_path;
                        self.store.update_status(
                            &task.id,
                            TaskStatus::Completed,
                            task.result_summary.as_deref(),
                            task.raw_log_path.as_deref(),
                        )?;
                    } else {
                        let summary = format!(
                            "exit code: {:?}\n{}",
                            r.exit_code,
                            r.stderr.lines().take(5).collect::<Vec<_>>().join("\n")
                        );
                        task.status = TaskStatus::Failed;
                        task.result_summary = Some(summary);
                        task.raw_log_path = log_path;
                        self.store.update_status(
                            &task.id,
                            TaskStatus::Failed,
                            task.result_summary.as_deref(),
                            task.raw_log_path.as_deref(),
                        )?;
                    }
                    (r.stdout.clone(), r.stderr.clone())
                }
                Err(e) => {
                    let summary = format!("agent error: {}", e);
                    task.status = TaskStatus::Failed;
                    task.result_summary = Some(summary.clone());
                    self.store
                        .update_status(&task.id, TaskStatus::Failed, Some(&summary), None)?;
                    (String::new(), summary)
                }
            };

            let session = self.sessions.get_session(&session_id);
            let formatted = self.formatter.format_reply(
                &task.subject,
                task.status == TaskStatus::Completed,
                &stdout,
                &stderr,
                session,
            );

            self.sessions.add_history(&session_id, "assistant", &stdout);
            self.sessions.set_status(&session_id, SessionStatus::Idle);
            self.sessions
                .register_message_id(&parsed.message_id, &session_id);
            self.sessions.save().ok();

            let result_subject = if task.status == TaskStatus::Completed {
                format!("Re: {} — Done", task.subject)
            } else {
                format!("Re: {} — Failed", task.subject)
            };

            if let Err(e) = self
                .outbox
                .send_reply(&parsed, &task, &formatted, &result_subject, &[])
                .await
            {
                tracing::error!(error = %e, "failed to send reply");
            }

            processed += 1;
        }

        Ok(processed)
    }

    fn handle_command(&mut self, cmd: &str, args: &str, sender: &str) -> String {
        match cmd {
            "help" => builtins::handle_help(&self.commands),
            "new" => builtins::handle_new(&mut self.sessions, sender, args),
            "doctor" => {
                let report = diagnostics::run_doctor(
                    &self.config,
                    &self.sessions,
                    &self.cron,
                    self.started_at,
                );
                diagnostics::format_report(&report)
            }
            _ => format!(
                "Unknown command: /{}. Use /help to see available commands.",
                cmd
            ),
        }
    }

    pub fn started_at(&self) -> DateTime<Utc> {
        self.started_at
    }

    pub fn reload_config(&mut self, path: &std::path::Path) -> Result<()> {
        let new_config = Config::load(path)?;
        self.security = SecurityGuard::new(new_config.security.clone());
        self.formatter = ReplyFormatter::from_config(&new_config.display);
        self.relay = RelayManager::new(&new_config.relay);
        self.workspace = WorkspaceRouter::new(new_config.workspace.clone());
        self.config = new_config;
        tracing::info!("config reloaded");
        Ok(())
    }
}

fn save_log(task_id: &str, stdout: &str, stderr: &str) -> Option<String> {
    let log_dir = PathBuf::from("logs");
    std::fs::create_dir_all(&log_dir).ok()?;
    let path = log_dir.join(format!("{}.log", task_id));
    let content = format!("=== STDOUT ===\n{}\n\n=== STDERR ===\n{}\n", stdout, stderr);
    std::fs::write(&path, content).ok()?;
    Some(path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::command_runner::AgentResult;
    use crate::config::*;
    use crate::mail::parser::ParsedEmail;
    use std::sync::{Arc, Mutex};

    struct MockInbox {
        messages: Vec<(String, Vec<u8>)>,
    }

    impl InboxAdapter for MockInbox {
        fn fetch_unseen(&self) -> crate::inbox::imap_poll::FetchFuture<'_> {
            let msgs = self.messages.clone();
            Box::pin(async move { Ok(msgs) })
        }
    }

    struct MockAgent {
        stdout: String,
        stderr: String,
        success: bool,
    }

    impl AgentRunner for MockAgent {
        fn run(
            &self,
            _prompt: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<AgentResult>> + Send + '_>>
        {
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

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    struct SentReply {
        to: String,
        stdout: String,
    }

    struct MockReplier {
        sent: Arc<Mutex<Vec<SentReply>>>,
    }

    impl ReplyHandler for MockReplier {
        fn send_reply<'a>(
            &'a self,
            original: &'a ParsedEmail,
            _task: &'a Task,
            body: &'a str,
            _subject_override: &'a str,
            _attachments: &'a [crate::attachment::AttachmentFile],
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>>
        {
            self.sent.lock().unwrap().push(SentReply {
                to: original.from.clone(),
                stdout: body.to_string(),
            });
            Box::pin(async { Ok("<mock@cc-email>".to_string()) })
        }
    }

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
            heartbeat: crate::config::HeartbeatConfig::default(),
            webhook: crate::webhook::WebhookConfig::default(),
            relay: crate::relay::RelayConfig::default(),
            workspace: crate::workspace::WorkspaceConfig::default(),
            attachments: crate::attachment::AttachmentConfig::default(),
        }
    }

    fn tmp_store() -> TaskStore {
        let path = format!("/tmp/cc-email-engine-test-{}.db", uuid::Uuid::new_v4());
        TaskStore::open(&path).unwrap()
    }

    #[tokio::test]
    async fn test_engine_command_routing() {
        let raw = make_raw_email("<cmd-001@example.com>", "user@example.com", "Help", "/help");

        let sent = Arc::new(Mutex::new(Vec::new()));
        let sessions = SessionManager::new(PathBuf::from("/tmp/test-engine-sessions-cmd.json"));

        let mut engine = Engine::new_with_parts(
            test_config(),
            Box::new(MockInbox {
                messages: vec![("1".into(), raw)],
            }),
            Box::new(MockReplier { sent: sent.clone() }),
            Box::new(MockAgent {
                stdout: "should not run".into(),
                stderr: String::new(),
                success: true,
            }),
            sessions,
            tmp_store(),
        );

        let count = engine.process_cycle().await.unwrap();
        assert_eq!(count, 1);

        let replies = sent.lock().unwrap();
        assert_eq!(replies.len(), 1);
        assert!(replies[0].stdout.contains("/help"));
        assert!(replies[0].stdout.contains("/new"));
    }

    #[tokio::test]
    async fn test_engine_regular_message() {
        let raw = make_raw_email(
            "<msg-001@example.com>",
            "user@example.com",
            "Fix bug",
            "Please fix the login bug",
        );

        let sent = Arc::new(Mutex::new(Vec::new()));
        let sessions = SessionManager::new(PathBuf::from("/tmp/test-engine-sessions-msg.json"));

        let mut engine = Engine::new_with_parts(
            test_config(),
            Box::new(MockInbox {
                messages: vec![("1".into(), raw)],
            }),
            Box::new(MockReplier { sent: sent.clone() }),
            Box::new(MockAgent {
                stdout: "Bug fixed".into(),
                stderr: String::new(),
                success: true,
            }),
            sessions,
            tmp_store(),
        );

        let count = engine.process_cycle().await.unwrap();
        assert_eq!(count, 1);

        let replies = sent.lock().unwrap();
        assert_eq!(replies.len(), 1);
        assert!(replies[0].stdout.contains("Bug fixed"));
    }

    #[tokio::test]
    async fn test_engine_session_creation() {
        let raw = make_raw_email(
            "<sess-001@example.com>",
            "user@example.com",
            "Task",
            "do something",
        );

        let sessions = SessionManager::new(PathBuf::from("/tmp/test-engine-sessions-create.json"));

        let mut engine = Engine::new_with_parts(
            test_config(),
            Box::new(MockInbox {
                messages: vec![("1".into(), raw)],
            }),
            Box::new(MockReplier {
                sent: Arc::new(Mutex::new(Vec::new())),
            }),
            Box::new(MockAgent {
                stdout: "ok".into(),
                stderr: String::new(),
                success: true,
            }),
            sessions,
            tmp_store(),
        );

        engine.process_cycle().await.unwrap();

        assert_eq!(engine.sessions.session_count(), 1);
        let session = engine
            .sessions
            .get_active_session("user@example.com")
            .unwrap();
        assert_eq!(session.history.len(), 2);
    }

    #[tokio::test]
    async fn test_engine_separate_senders() {
        let raw1 = make_raw_email(
            "<multi-001@example.com>",
            "alice@example.com",
            "Task A",
            "alice task",
        );
        let raw2 = make_raw_email(
            "<multi-002@example.com>",
            "bob@example.com",
            "Task B",
            "bob task",
        );

        let sessions = SessionManager::new(PathBuf::from("/tmp/test-engine-sessions-multi.json"));

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
            sessions,
            tmp_store(),
        );

        engine.process_cycle().await.unwrap();

        assert_eq!(engine.sessions.session_count(), 2);
        assert!(engine
            .sessions
            .get_active_session("alice@example.com")
            .is_some());
        assert!(engine
            .sessions
            .get_active_session("bob@example.com")
            .is_some());
    }

    #[tokio::test]
    async fn test_engine_new_command() {
        let raw = make_raw_email(
            "<new-001@example.com>",
            "user@example.com",
            "New Session",
            "/new my-project",
        );

        let sent = Arc::new(Mutex::new(Vec::new()));
        let sessions = SessionManager::new(PathBuf::from("/tmp/test-engine-sessions-new.json"));

        let mut engine = Engine::new_with_parts(
            test_config(),
            Box::new(MockInbox {
                messages: vec![("1".into(), raw)],
            }),
            Box::new(MockReplier { sent: sent.clone() }),
            Box::new(MockAgent {
                stdout: "should not run".into(),
                stderr: String::new(),
                success: true,
            }),
            sessions,
            tmp_store(),
        );

        engine.process_cycle().await.unwrap();

        let replies = sent.lock().unwrap();
        assert!(replies[0].stdout.contains("Created new session"));
        assert!(replies[0].stdout.contains("my-project"));
    }

    #[tokio::test]
    async fn test_engine_unknown_command() {
        let raw = make_raw_email(
            "<unk-001@example.com>",
            "user@example.com",
            "Unknown",
            "/foobar",
        );

        let sent = Arc::new(Mutex::new(Vec::new()));
        let sessions = SessionManager::new(PathBuf::from("/tmp/test-engine-sessions-unk.json"));

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
            sessions,
            tmp_store(),
        );

        engine.process_cycle().await.unwrap();

        let replies = sent.lock().unwrap();
        assert!(replies[0].stdout.contains("Unknown command: /foobar"));
    }
}
