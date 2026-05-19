use serde::Deserialize;
use std::path::Path;

use crate::error::{CcEmailError, Result};

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub inbox: InboxConfig,
    pub outbox: OutboxConfig,
    pub agent: AgentConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub display: DisplayConfig,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub heartbeat: HeartbeatConfig,
    #[serde(default)]
    pub webhook: crate::webhook::WebhookConfig,
    #[serde(default)]
    pub relay: crate::relay::RelayConfig,
    #[serde(default)]
    pub workspace: crate::workspace::WorkspaceConfig,
    #[serde(default)]
    pub attachments: crate::attachment::AttachmentConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct HeartbeatConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_heartbeat_schedule")]
    pub schedule: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            schedule: default_heartbeat_schedule(),
            prompt: None,
            reply_to: None,
        }
    }
}

fn default_heartbeat_schedule() -> String {
    "*/30 * * * *".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct SessionConfig {
    #[serde(default)]
    pub reset_on_idle_mins: Option<u64>,
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            reset_on_idle_mins: None,
            data_dir: default_data_dir(),
        }
    }
}

fn default_data_dir() -> String {
    dirs::home_dir()
        .map(|h| h.join(".cc-email").to_string_lossy().to_string())
        .unwrap_or_else(|| ".cc-email".to_string())
}

#[derive(Debug, Deserialize, Clone)]
pub struct DisplayConfig {
    #[serde(default)]
    pub show_thinking: bool,
    #[serde(default = "default_true")]
    pub show_tool_use: bool,
    #[serde(default = "default_true")]
    pub reply_footer: bool,
    #[serde(default = "default_max_output_chars")]
    pub max_output_chars: usize,
    #[serde(default = "default_max_error_chars")]
    pub max_error_chars: usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            show_thinking: false,
            show_tool_use: true,
            reply_footer: true,
            max_output_chars: 8000,
            max_error_chars: 4000,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_max_output_chars() -> usize {
    8000
}

fn default_max_error_chars() -> usize {
    4000
}

#[derive(Debug, Deserialize, Clone)]
pub struct InboxConfig {
    #[serde(rename = "type")]
    pub inbox_type: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub password_env: Option<String>,
    #[serde(default = "default_folder")]
    pub folder: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_seconds: u64,
    #[serde(default)]
    pub search_to: Option<String>,
    #[serde(default)]
    pub search_from: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OutboxConfig {
    #[serde(rename = "type")]
    pub outbox_type: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub password_env: Option<String>,
    pub from: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AgentConfig {
    #[serde(rename = "type")]
    pub agent_type: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    #[serde(default)]
    pub work_dir: Option<String>,
    #[serde(default = "default_permission_timeout")]
    pub permission_timeout_seconds: u64,
    #[serde(default = "default_permission_default")]
    pub permission_default: String,
}

fn default_permission_mode() -> String {
    "auto".to_string()
}

fn default_permission_timeout() -> u64 {
    300
}

fn default_permission_default() -> String {
    "deny".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProviderConfig {
    pub name: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SecurityConfig {
    #[serde(default)]
    pub allowed_senders: Vec<String>,
    #[serde(default = "default_max_body")]
    pub max_body_bytes: usize,
    #[serde(default = "default_max_attachment")]
    pub max_attachment_bytes: usize,
    #[serde(default)]
    pub admin_senders: Vec<String>,
    #[serde(default)]
    pub rate_limit_per_minute: Option<u32>,
    #[serde(default)]
    pub rate_limit_per_hour: Option<u32>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            allowed_senders: Vec::new(),
            max_body_bytes: 20_000,
            max_attachment_bytes: 5_000_000,
            admin_senders: Vec::new(),
            rate_limit_per_minute: None,
            rate_limit_per_hour: None,
        }
    }
}

fn default_folder() -> String {
    "INBOX".to_string()
}

fn default_poll_interval() -> u64 {
    30
}

fn default_timeout() -> u64 {
    300
}

fn default_max_body() -> usize {
    20_000
}

fn default_max_attachment() -> usize {
    5_000_000
}

impl InboxConfig {
    pub fn resolve_password(&self) -> Result<String> {
        if let Some(ref pw) = self.password {
            return Ok(pw.clone());
        }
        if let Some(ref env_var) = self.password_env {
            return std::env::var(env_var)
                .map_err(|_| CcEmailError::Config(format!("env var '{}' not set", env_var)));
        }
        Err(CcEmailError::Config(
            "inbox: no password or password_env configured".into(),
        ))
    }
}

impl OutboxConfig {
    pub fn resolve_password(&self) -> Result<String> {
        if let Some(ref pw) = self.password {
            return Ok(pw.clone());
        }
        if let Some(ref env_var) = self.password_env {
            return std::env::var(env_var)
                .map_err(|_| CcEmailError::Config(format!("env var '{}' not set", env_var)));
        }
        Err(CcEmailError::Config(
            "outbox: no password or password_env configured".into(),
        ))
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| CcEmailError::Config(format!("failed to read config file: {}", e)))?;
        let config: Config = toml::from_str(&content)
            .map_err(|e| CcEmailError::Config(format!("failed to parse config: {}", e)))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.inbox.host.is_empty() {
            return Err(CcEmailError::Config("inbox.host is required".into()));
        }
        if self.outbox.host.is_empty() {
            return Err(CcEmailError::Config("outbox.host is required".into()));
        }
        if self.agent.agent_type == "command" && self.agent.command.is_empty() {
            return Err(CcEmailError::Config(
                "agent.command is required for command type".into(),
            ));
        }
        Ok(())
    }
}
