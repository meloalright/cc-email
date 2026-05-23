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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_raw(msg_id: &str, from: &str, subject: &str, body: &str) -> Vec<u8> {
        format!(
            "Message-ID: {}\r\nFrom: {}\r\nSubject: {}\r\nContent-Type: text/plain\r\n\r\n{}",
            msg_id, from, subject, body
        )
        .into_bytes()
    }

    #[test]
    fn test_parse_basic_email() {
        let raw = make_raw("<test@example.com>", "alice@example.com", "Hello", "world");
        let parsed = parse_email(&raw).unwrap();
        assert_eq!(parsed.message_id, "<test@example.com>");
        assert_eq!(parsed.from, "alice@example.com");
        assert_eq!(parsed.subject, "Hello");
        assert_eq!(parsed.text_body.trim(), "world");
    }

    #[test]
    fn test_parse_preserves_raw_size() {
        let raw = make_raw("<s@t>", "a@b", "S", "body");
        let size = raw.len();
        let parsed = parse_email(&raw).unwrap();
        assert_eq!(parsed.raw_size, size);
    }

    #[test]
    fn test_parse_empty_body() {
        let raw = make_raw("<e@t>", "a@b", "Empty", "");
        let parsed = parse_email(&raw).unwrap();
        assert!(parsed.text_body.is_empty());
    }

    #[test]
    fn test_parse_in_reply_to() {
        let raw = format!(
            "Message-ID: <r@t>\r\nFrom: a@b\r\nSubject: Re\r\nIn-Reply-To: <orig@t>\r\nContent-Type: text/plain\r\n\r\nreply"
        ).into_bytes();
        let parsed = parse_email(&raw).unwrap();
        assert!(parsed.in_reply_to.is_some());
        assert!(parsed.in_reply_to.unwrap().contains("orig@t"));
    }

    #[test]
    fn test_parse_invalid_email_fails() {
        let result = parse_email(b"not an email at all");
        assert!(result.is_err() || result.unwrap().from.is_empty());
    }
}
