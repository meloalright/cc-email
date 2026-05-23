use crate::config::SecurityConfig;
use crate::error::{CcEmailError, Result};

pub struct SecurityGuard {
    config: SecurityConfig,
}

impl SecurityGuard {
    pub fn new(config: SecurityConfig) -> Self {
        Self { config }
    }

    pub fn validate_sender(&self, sender: &str) -> Result<()> {
        if self.config.allowed_senders.is_empty() {
            return Ok(());
        }
        let sender_lower = sender.to_lowercase();
        let allowed = self
            .config
            .allowed_senders
            .iter()
            .any(|s| sender_lower.contains(&s.to_lowercase()));
        if !allowed {
            return Err(CcEmailError::Security(format!(
                "sender '{}' not in allowed list",
                sender
            )));
        }
        Ok(())
    }

    pub fn validate_body(&self, body: &str) -> Result<()> {
        if body.trim().is_empty() {
            return Err(CcEmailError::Security("empty task body".into()));
        }
        if body.len() > self.config.max_body_bytes {
            return Err(CcEmailError::Security(format!(
                "body size {} exceeds limit {}",
                body.len(),
                self.config.max_body_bytes
            )));
        }
        Ok(())
    }

    pub fn validate_attachment_size(&self, size: usize) -> Result<()> {
        if size > self.config.max_attachment_bytes {
            return Err(CcEmailError::Security(format!(
                "attachment size {} exceeds limit {}",
                size, self.config.max_attachment_bytes
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(senders: Vec<&str>) -> SecurityGuard {
        SecurityGuard::new(SecurityConfig {
            allowed_senders: senders.into_iter().map(String::from).collect(),
            max_body_bytes: 100,
            max_attachment_bytes: 500,
            ..Default::default()
        })
    }

    #[test]
    fn test_allowed_sender_passes() {
        let g = guard(vec!["alice@example.com"]);
        assert!(g.validate_sender("alice@example.com").is_ok());
    }

    #[test]
    fn test_sender_case_insensitive() {
        let g = guard(vec!["Alice@Example.COM"]);
        assert!(g.validate_sender("alice@example.com").is_ok());
    }

    #[test]
    fn test_rejected_sender() {
        let g = guard(vec!["alice@example.com"]);
        assert!(g.validate_sender("bob@example.com").is_err());
    }

    #[test]
    fn test_empty_allowlist_permits_all() {
        let g = guard(vec![]);
        assert!(g.validate_sender("anyone@example.com").is_ok());
    }

    #[test]
    fn test_valid_body() {
        let g = guard(vec![]);
        assert!(g.validate_body("hello world").is_ok());
    }

    #[test]
    fn test_empty_body_rejected() {
        let g = guard(vec![]);
        assert!(g.validate_body("   ").is_err());
    }

    #[test]
    fn test_oversized_body_rejected() {
        let g = guard(vec![]);
        let body = "x".repeat(101);
        assert!(g.validate_body(&body).is_err());
    }

    #[test]
    fn test_body_at_exact_limit() {
        let g = guard(vec![]);
        let body = "x".repeat(100);
        assert!(g.validate_body(&body).is_ok());
    }

    #[test]
    fn test_attachment_within_limit() {
        let g = guard(vec![]);
        assert!(g.validate_attachment_size(500).is_ok());
    }

    #[test]
    fn test_attachment_over_limit() {
        let g = guard(vec![]);
        assert!(g.validate_attachment_size(501).is_err());
    }
}
