use lettre::message::header::ContentType;
use lettre::message::{header, Attachment, Mailbox, MessageBuilder, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::attachment::AttachmentFile;
use crate::config::OutboxConfig;
use crate::error::{CcEmailError, Result};
use crate::mail::parser::ParsedEmail;
use crate::task::model::{Task, TaskStatus};

pub trait ReplyHandler: Send + Sync {
    fn send_reply<'a>(
        &'a self,
        original: &'a ParsedEmail,
        task: &'a Task,
        body: &'a str,
        subject_override: &'a str,
        attachments: &'a [AttachmentFile],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>>;
}

pub struct SmtpReplier {
    config: OutboxConfig,
}

impl SmtpReplier {
    pub fn new(config: OutboxConfig) -> Self {
        Self { config }
    }

    async fn do_send(
        &self,
        original: &ParsedEmail,
        task: &Task,
        body_text: &str,
        subject_override: &str,
        attachments: &[AttachmentFile],
    ) -> Result<String> {
        let password = self.config.resolve_password()?;

        let from_mailbox: Mailbox = self
            .config
            .from
            .parse()
            .map_err(|e| CcEmailError::Smtp(format!("invalid from address: {}", e)))?;

        let to_mailbox: Mailbox = original
            .from
            .parse()
            .map_err(|e| CcEmailError::Smtp(format!("invalid to address: {}", e)))?;

        let subject = if subject_override.is_empty() {
            format!("Re: {}", task.subject)
        } else {
            subject_override.to_string()
        };

        let from_domain = self
            .config
            .from
            .rsplit_once('@')
            .map(|(_, d)| d.trim_end_matches('>'))
            .unwrap_or("localhost");
        let msg_id = format!(
            "<{}.{}@{}>",
            uuid::Uuid::new_v4(),
            chrono::Utc::now().timestamp(),
            from_domain
        );

        let mut builder: MessageBuilder = Message::builder()
            .message_id(Some(msg_id.clone()))
            .from(from_mailbox)
            .to(to_mailbox)
            .subject(subject);

        if !original.message_id.is_empty() {
            builder = builder.header(header::InReplyTo::from(original.message_id.clone()));
            let refs = if let Some(ref existing_refs) = original.references {
                format!("{} {}", existing_refs, original.message_id)
            } else {
                original.message_id.clone()
            };
            builder = builder.header(header::References::from(refs));
        }

        let email = if attachments.is_empty() {
            builder
                .body(body_text.to_string())
                .map_err(|e| CcEmailError::Smtp(format!("failed to build email: {}", e)))?
        } else {
            let text_part = SinglePart::builder()
                .content_type(ContentType::TEXT_PLAIN)
                .body(body_text.to_string());

            let mut multipart = MultiPart::mixed().singlepart(text_part);

            for file in attachments {
                let ct: ContentType = file
                    .content_type
                    .parse()
                    .unwrap_or(ContentType::parse("application/octet-stream").unwrap());
                let attachment = Attachment::new(file.filename.clone()).body(file.data.clone(), ct);
                multipart = multipart.singlepart(attachment);
            }

            tracing::info!(
                count = attachments.len(),
                filenames = %attachments.iter().map(|a| a.filename.as_str()).collect::<Vec<_>>().join(", "),
                "attaching files to reply"
            );

            builder
                .multipart(multipart)
                .map_err(|e| CcEmailError::Smtp(format!("failed to build email: {}", e)))?
        };

        let creds = Credentials::new(self.config.username.clone(), password);

        let mailer: AsyncSmtpTransport<Tokio1Executor> = if self.config.port == 465 {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&self.config.host)
                .map_err(|e| CcEmailError::Smtp(format!("smtp relay error: {}", e)))?
                .port(self.config.port)
                .credentials(creds)
                .build()
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.config.host)
                .map_err(|e| CcEmailError::Smtp(format!("smtp relay error: {}", e)))?
                .port(self.config.port)
                .credentials(creds)
                .build()
        };

        mailer
            .send(email)
            .await
            .map_err(|e| CcEmailError::Smtp(format!("failed to send: {}", e)))?;

        tracing::info!(to = %original.from, message_id = %msg_id, "reply sent");
        Ok(msg_id)
    }
}

impl ReplyHandler for SmtpReplier {
    fn send_reply<'a>(
        &'a self,
        original: &'a ParsedEmail,
        task: &'a Task,
        body: &'a str,
        subject_override: &'a str,
        attachments: &'a [AttachmentFile],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(self.do_send(original, task, body, subject_override, attachments))
    }
}

pub fn format_reply_body(task: &Task, stdout: &str, stderr: &str) -> String {
    let status_label = match task.status {
        TaskStatus::Completed => "COMPLETED",
        TaskStatus::Failed => "FAILED",
        _ => "UNKNOWN",
    };

    let mut body = format!("Task: {}\nStatus: {}\n", task.subject, status_label);

    if let Some(ref summary) = task.result_summary {
        body.push_str(&format!("\n--- Summary ---\n{}\n", summary));
    }

    let stdout_trimmed = truncate_output(stdout, 4000);
    if !stdout_trimmed.is_empty() {
        body.push_str(&format!("\n--- Output ---\n{}\n", stdout_trimmed));
    }

    let stderr_trimmed = truncate_output(stderr, 2000);
    if !stderr_trimmed.is_empty() {
        body.push_str(&format!("\n--- Errors ---\n{}\n", stderr_trimmed));
    }

    body.push_str("\n--\nSent by cc-email\n");
    body
}

fn truncate_output(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}
