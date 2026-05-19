use std::path::PathBuf;

use crate::agent::command_runner::{AgentRunner, CommandAgent};
use crate::config::Config;
use crate::error::Result;
use crate::inbox::imap_poll::{ImapPoller, InboxAdapter};
use crate::mail::parser::parse_email;
use crate::mail::reply::{ReplyHandler, SmtpReplier};
use crate::security::SecurityGuard;
use crate::task::model::{Task, TaskStatus};
use crate::task::store::TaskStore;

pub async fn run(config: Config) -> Result<()> {
    let db_path = "cc-email.db";
    let store = TaskStore::open(db_path)?;
    let guard = SecurityGuard::new(config.security.clone());
    let inbox: Box<dyn InboxAdapter> = Box::new(ImapPoller::new(config.inbox.clone()));
    let agent: Box<dyn AgentRunner> = Box::new(CommandAgent::new(config.agent.clone()));
    let replier: Box<dyn ReplyHandler> = Box::new(SmtpReplier::new(config.outbox.clone()));
    let poll_interval = std::time::Duration::from_secs(config.inbox.poll_interval_seconds);

    tracing::info!("cc-email daemon started");

    loop {
        match process_cycle(&*inbox, &*agent, &*replier, &guard, &store).await {
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

pub async fn process_cycle(
    inbox: &dyn InboxAdapter,
    agent: &dyn AgentRunner,
    replier: &dyn ReplyHandler,
    guard: &SecurityGuard,
    store: &TaskStore,
) -> Result<usize> {
    let messages = inbox.fetch_unseen().await?;
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

        if store.is_processed(&parsed.message_id)? {
            tracing::debug!(id = %parsed.message_id, "already processed, skipping");
            continue;
        }

        if let Err(e) = guard.validate_sender(&parsed.from) {
            tracing::warn!(from = %parsed.from, error = %e, "sender rejected");
            continue;
        }

        let prompt = if !parsed.text_body.is_empty() {
            &parsed.text_body
        } else if let Some(ref html) = parsed.html_body {
            html
        } else {
            tracing::warn!(id = %parsed.message_id, "no body found");
            continue;
        };

        if let Err(e) = guard.validate_body(prompt) {
            tracing::warn!(error = %e, "body validation failed");
            continue;
        }

        let mut task = Task::new(
            parsed.message_id.clone(),
            parsed.from.clone(),
            parsed.subject.clone(),
            prompt.to_string(),
        );

        store.insert(&task)?;
        tracing::info!(task_id = %task.id, subject = %task.subject, "task created");

        store.update_status(&task.id, TaskStatus::Running, None, None)?;
        task.status = TaskStatus::Running;

        let result = agent.run(&task.prompt).await;

        let (stdout, stderr) = match &result {
            Ok(r) => {
                let log_path = save_log(&task.id, &r.stdout, &r.stderr);

                if r.success {
                    let summary = r.stdout.lines().take(5).collect::<Vec<_>>().join("\n");
                    task.status = TaskStatus::Completed;
                    task.result_summary = Some(summary);
                    task.raw_log_path = log_path;
                    store.update_status(
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
                    store.update_status(
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
                store.update_status(&task.id, TaskStatus::Failed, Some(&summary), None)?;
                (String::new(), summary)
            }
        };

        let reply_body = crate::mail::reply::format_reply_body(&task, &stdout, &stderr);
        if let Err(e) = replier
            .send_reply(&parsed, &task, &reply_body, "", &[])
            .await
        {
            tracing::error!(error = %e, "failed to send reply");
        }

        processed += 1;
    }

    Ok(processed)
}

fn save_log(task_id: &str, stdout: &str, stderr: &str) -> Option<String> {
    let log_dir = PathBuf::from("logs");
    std::fs::create_dir_all(&log_dir).ok()?;
    let path = log_dir.join(format!("{}.log", task_id));
    let content = format!("=== STDOUT ===\n{}\n\n=== STDERR ===\n{}\n", stdout, stderr);
    std::fs::write(&path, content).ok()?;
    Some(path.to_string_lossy().to_string())
}
