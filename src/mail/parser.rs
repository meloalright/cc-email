use mail_parser::MessageParser;

use crate::error::{CcEmailError, Result};

#[derive(Debug, Clone)]
pub struct ParsedEmail {
    pub message_id: String,
    pub from: String,
    pub subject: String,
    pub text_body: String,
    pub html_body: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub raw_size: usize,
}

pub fn parse_email(raw: &[u8]) -> Result<ParsedEmail> {
    let message = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| CcEmailError::MailParse("failed to parse email".into()))?;

    let message_id = message
        .message_id()
        .map(|s| format!("<{}>", s))
        .unwrap_or_default();

    let from = message
        .from()
        .and_then(|addrs| addrs.first())
        .map(|addr| addr.address().map(|a| a.to_string()).unwrap_or_default())
        .unwrap_or_default();

    let subject = message.subject().unwrap_or("").to_string();

    let text_body = message
        .body_text(0)
        .map(|s| s.to_string())
        .unwrap_or_default();

    let html_body = message.body_html(0).map(|s| s.to_string());

    let in_reply_to = message.in_reply_to().as_text().map(|s| s.to_string());

    let references = message.references().as_text().map(|s| s.to_string());

    Ok(ParsedEmail {
        message_id,
        from,
        subject,
        text_body,
        html_body,
        in_reply_to,
        references,
        raw_size: raw.len(),
    })
}
