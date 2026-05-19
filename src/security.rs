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
