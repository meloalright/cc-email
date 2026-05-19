use std::process::Stdio;
use tokio::process::Command;

use crate::config::AgentConfig;
use crate::error::{CcEmailError, Result};

pub struct AgentResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub generated_files: Vec<crate::attachment::GeneratedFile>,
}

pub trait AgentRunner: Send + Sync {
    fn run(
        &self,
        prompt: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<AgentResult>> + Send + '_>>;
}

pub struct CommandAgent {
    config: AgentConfig,
}

impl CommandAgent {
    pub fn new(config: AgentConfig) -> Self {
        Self { config }
    }

    fn build_args(&self, prompt: &str) -> Vec<String> {
        self.config
            .args
            .iter()
            .map(|arg| arg.replace("{{prompt}}", prompt))
            .collect()
    }
}

impl AgentRunner for CommandAgent {
    fn run(
        &self,
        prompt: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<AgentResult>> + Send + '_>> {
        let args = self.build_args(prompt);
        let command = self.config.command.clone();
        let timeout_secs = self.config.timeout_seconds;

        Box::pin(async move {
            tracing::info!(cmd = %command, "running agent");

            let child = Command::new(&command)
                .args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| {
                    CcEmailError::Agent(format!("failed to spawn '{}': {}", command, e))
                })?;

            let output = tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs),
                child.wait_with_output(),
            )
            .await
            .map_err(|_| CcEmailError::Agent(format!("agent timed out after {}s", timeout_secs)))?
            .map_err(|e| CcEmailError::Agent(format!("agent process error: {}", e)))?;

            Ok(AgentResult {
                success: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                exit_code: output.status.code(),
                generated_files: Vec::new(),
            })
        })
    }
}
