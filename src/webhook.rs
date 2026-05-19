use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WebhookRequest {
    pub event: String,
    pub session_key: String,
    pub prompt: String,
    #[serde(default)]
    pub silent: bool,
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebhookConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_webhook_port")]
    pub port: u16,
    #[serde(default = "default_webhook_path")]
    pub path: String,
    #[serde(default)]
    pub token_env: Option<String>,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 9111,
            path: "/hook".to_string(),
            token_env: None,
        }
    }
}

fn default_webhook_port() -> u16 {
    9111
}

fn default_webhook_path() -> String {
    "/hook".to_string()
}

pub struct WebhookServer {
    config: WebhookConfig,
    pending: std::sync::Mutex<Vec<WebhookRequest>>,
}

impl WebhookServer {
    pub fn new(config: WebhookConfig) -> Self {
        Self {
            config,
            pending: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn port(&self) -> u16 {
        self.config.port
    }

    pub fn path(&self) -> &str {
        &self.config.path
    }

    pub fn resolve_token(&self) -> Option<String> {
        self.config
            .token_env
            .as_ref()
            .and_then(|env| std::env::var(env).ok())
    }

    pub fn validate_token(&self, provided: &str) -> bool {
        match self.resolve_token() {
            Some(expected) => provided == expected,
            None => true,
        }
    }

    pub fn enqueue(&self, request: WebhookRequest) {
        self.pending.lock().unwrap().push(request);
    }

    pub fn drain_pending(&self) -> Vec<WebhookRequest> {
        let mut pending = self.pending.lock().unwrap();
        std::mem::take(&mut *pending)
    }

    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_webhook_config_default() {
        let config = WebhookConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.port, 9111);
        assert_eq!(config.path, "/hook");
    }

    #[test]
    fn test_webhook_server() {
        let server = WebhookServer::new(WebhookConfig {
            enabled: true,
            port: 8080,
            path: "/webhook".to_string(),
            token_env: None,
        });
        assert!(server.is_enabled());
        assert_eq!(server.port(), 8080);
        assert_eq!(server.path(), "/webhook");
    }

    #[test]
    fn test_enqueue_and_drain() {
        let server = WebhookServer::new(WebhookConfig::default());
        server.enqueue(WebhookRequest {
            event: "test".to_string(),
            session_key: "user@example.com".to_string(),
            prompt: "do something".to_string(),
            silent: false,
            payload: None,
        });
        assert_eq!(server.pending_count(), 1);
        let drained = server.drain_pending();
        assert_eq!(drained.len(), 1);
        assert_eq!(server.pending_count(), 0);
    }

    #[test]
    fn test_validate_token_no_env() {
        let server = WebhookServer::new(WebhookConfig::default());
        assert!(server.validate_token("anything"));
    }

    #[test]
    fn test_webhook_request_parse() {
        let json = r#"{
            "event": "ci:build_failed",
            "session_key": "admin@example.com",
            "prompt": "Investigate the build failure",
            "silent": false,
            "payload": {"branch": "main"}
        }"#;
        let req: WebhookRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.event, "ci:build_failed");
        assert_eq!(req.session_key, "admin@example.com");
        assert!(!req.silent);
        assert!(req.payload.is_some());
    }
}
