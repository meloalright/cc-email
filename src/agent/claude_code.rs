use std::path::PathBuf;
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

use crate::agent::command_runner::{AgentResult, AgentRunner};
use crate::attachment::GeneratedFile;
use crate::error::{CcEmailError, Result};
use crate::permission::{PermissionDecision, PermissionRequest};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    pub event_type: AgentEventType,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEventType {
    Text(String),
    Thinking(String),
    ToolUse {
        name: String,
        input: String,
    },
    ToolResult {
        name: String,
        output: String,
        success: bool,
    },
    Error(String),
    Done {
        input_tokens: u64,
        output_tokens: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageReport {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_cost_usd: Option<f64>,
}

pub struct ClaudeCodeAgent {
    binary: String,
    work_dir: PathBuf,
    model: Option<String>,
    permission_mode: String,
    timeout_seconds: u64,
    last_usage: std::sync::Mutex<UsageReport>,
}

impl ClaudeCodeAgent {
    pub fn new(
        binary: String,
        work_dir: PathBuf,
        model: Option<String>,
        permission_mode: String,
        timeout_seconds: u64,
    ) -> Self {
        Self {
            binary,
            work_dir,
            model,
            permission_mode,
            timeout_seconds,
            last_usage: std::sync::Mutex::new(UsageReport::default()),
        }
    }

    pub fn get_model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn set_model(&mut self, model: &str) {
        self.model = Some(model.to_string());
    }

    pub fn get_permission_mode(&self) -> &str {
        &self.permission_mode
    }

    pub fn set_permission_mode(&mut self, mode: &str) {
        self.permission_mode = mode.to_string();
    }

    pub fn last_usage(&self) -> UsageReport {
        self.last_usage.lock().unwrap().clone()
    }

    fn build_command(&self) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.current_dir(&self.work_dir)
            .arg("-p")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--permission-mode")
            .arg(&self.permission_mode);

        if let Some(ref model) = self.model {
            cmd.arg("--model").arg(model);
        }

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        cmd
    }

    async fn run_stream(
        &self,
        prompt: &str,
    ) -> Result<(String, String, Vec<AgentEvent>, bool, Option<i32>)> {
        let mut child: Child = self.build_command().spawn().map_err(|e| {
            CcEmailError::Agent(format!("failed to spawn '{}': {}", self.binary, e))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| CcEmailError::Agent("failed to get stdin".to_string()))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CcEmailError::Agent("failed to get stdout".to_string()))?;

        let stderr_handle = child.stderr.take();

        let input_msg = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": prompt
            }
        });

        let mut stdin_writer = stdin;
        let input_bytes = format!("{}\n", input_msg);
        stdin_writer
            .write_all(input_bytes.as_bytes())
            .await
            .map_err(|e| CcEmailError::Agent(format!("failed to write to stdin: {}", e)))?;
        drop(stdin_writer);

        let mut events = Vec::new();
        let mut result_text = String::new();
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();

        let read_result = tokio::time::timeout(
            std::time::Duration::from_secs(self.timeout_seconds),
            async {
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                continue;
                            }
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                                match val.get("type").and_then(|t| t.as_str()) {
                                    Some("assistant") | Some("text") => {
                                        if let Some(content) = val.get("content").or_else(|| {
                                            val.get("message").and_then(|m| m.get("content"))
                                        }) {
                                            if let Some(text) = content.as_str() {
                                                result_text.push_str(text);
                                            } else if let Some(arr) = content.as_array() {
                                                for block in arr {
                                                    if let Some(text) =
                                                        block.get("text").and_then(|t| t.as_str())
                                                    {
                                                        result_text.push_str(text);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Some("content_block_delta") => {
                                        if let Some(delta) = val.get("delta") {
                                            if let Some(text) =
                                                delta.get("text").and_then(|t| t.as_str())
                                            {
                                                result_text.push_str(text);
                                            }
                                        }
                                    }
                                    Some("result") => {
                                        if let Some(result) =
                                            val.get("result").and_then(|r| r.as_str())
                                        {
                                            if result_text.is_empty() {
                                                result_text.push_str(result);
                                            }
                                        }
                                        if let Some(usage) = val.get("usage") {
                                            let input_tokens = usage
                                                .get("input_tokens")
                                                .and_then(|t| t.as_u64())
                                                .unwrap_or(0);
                                            let output_tokens = usage
                                                .get("output_tokens")
                                                .and_then(|t| t.as_u64())
                                                .unwrap_or(0);
                                            events.push(AgentEvent {
                                                event_type: AgentEventType::Done {
                                                    input_tokens,
                                                    output_tokens,
                                                },
                                                timestamp: chrono::Utc::now(),
                                            });
                                        }
                                    }
                                    Some("tool_use") => {
                                        let name = val
                                            .get("name")
                                            .or_else(|| val.get("tool").and_then(|t| t.get("name")))
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("unknown")
                                            .to_string();
                                        let input = val
                                            .get("input")
                                            .map(|i| i.to_string())
                                            .unwrap_or_default();
                                        events.push(AgentEvent {
                                            event_type: AgentEventType::ToolUse { name, input },
                                            timestamp: chrono::Utc::now(),
                                        });
                                    }
                                    Some("tool_result") => {
                                        let name = val
                                            .get("name")
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("unknown")
                                            .to_string();
                                        let output = val
                                            .get("output")
                                            .or_else(|| val.get("content"))
                                            .map(|o| {
                                                if o.is_string() {
                                                    o.as_str().unwrap_or("").to_string()
                                                } else {
                                                    o.to_string()
                                                }
                                            })
                                            .unwrap_or_default();
                                        let success = val
                                            .get("is_error")
                                            .and_then(|e| e.as_bool())
                                            .map(|e| !e)
                                            .unwrap_or(true);
                                        events.push(AgentEvent {
                                            event_type: AgentEventType::ToolResult {
                                                name,
                                                output,
                                                success,
                                            },
                                            timestamp: chrono::Utc::now(),
                                        });
                                    }
                                    Some("thinking") => {
                                        if let Some(text) =
                                            val.get("content").and_then(|c| c.as_str())
                                        {
                                            events.push(AgentEvent {
                                                event_type: AgentEventType::Thinking(
                                                    text.to_string(),
                                                ),
                                                timestamp: chrono::Utc::now(),
                                            });
                                        }
                                    }
                                    Some("error") => {
                                        let msg = val
                                            .get("error")
                                            .or_else(|| val.get("message"))
                                            .and_then(|e| e.as_str())
                                            .unwrap_or("unknown error")
                                            .to_string();
                                        events.push(AgentEvent {
                                            event_type: AgentEventType::Error(msg),
                                            timestamp: chrono::Utc::now(),
                                        });
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "error reading agent stdout");
                            break;
                        }
                    }
                }
                Ok::<(), CcEmailError>(())
            },
        )
        .await;

        if read_result.is_err() {
            let _ = child.kill().await;
            return Err(CcEmailError::Agent(format!(
                "agent timed out after {}s",
                self.timeout_seconds
            )));
        }

        let status = child
            .wait()
            .await
            .map_err(|e| CcEmailError::Agent(format!("agent process error: {}", e)))?;

        let stderr_output = if let Some(stderr) = stderr_handle {
            let mut buf = String::new();
            let mut reader = BufReader::new(stderr);
            let _ = tokio::io::AsyncReadExt::read_to_string(&mut reader, &mut buf).await;
            buf
        } else {
            String::new()
        };

        Ok((
            result_text,
            stderr_output,
            events,
            status.success(),
            status.code(),
        ))
    }

    fn build_command_for_permissions(&self) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.current_dir(&self.work_dir)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--input-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--permission-prompt-tool")
            .arg("stdio");

        if let Some(ref model) = self.model {
            cmd.arg("--model").arg(model);
        }

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        cmd
    }

    pub async fn run_with_permissions(
        &self,
        prompt: &str,
        perm_req_tx: tokio::sync::mpsc::Sender<PermissionRequest>,
        mut perm_resp_rx: tokio::sync::mpsc::Receiver<PermissionResponse>,
    ) -> Result<AgentResult> {
        tracing::info!(cmd = %self.binary, "running claude code agent with permission support");

        let mut child: Child = self.build_command_for_permissions().spawn().map_err(|e| {
            CcEmailError::Agent(format!("failed to spawn '{}': {}", self.binary, e))
        })?;

        let mut stdin_writer = child
            .stdin
            .take()
            .ok_or_else(|| CcEmailError::Agent("failed to get stdin".to_string()))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CcEmailError::Agent("failed to get stdout".to_string()))?;

        let stderr_handle = child.stderr.take();

        let input_msg = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": prompt
            }
        });

        let input_bytes = format!("{}\n", input_msg);
        stdin_writer
            .write_all(input_bytes.as_bytes())
            .await
            .map_err(|e| CcEmailError::Agent(format!("failed to write to stdin: {}", e)))?;
        // Keep stdin open for permission responses

        let mut events = Vec::new();
        let mut result_text = String::new();
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();

        let read_result = tokio::time::timeout(
            std::time::Duration::from_secs(self.timeout_seconds),
            async {
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                continue;
                            }
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                                match val.get("type").and_then(|t| t.as_str()) {
                                    Some("control_request") => {
                                        let request_id = val.get("request_id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown")
                                            .to_string();
                                        let request = val.get("request")
                                            .and_then(|v| v.as_object())
                                            .cloned()
                                            .unwrap_or_default();
                                        let subtype = request.get("subtype")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        if subtype != "can_use_tool" {
                                            continue;
                                        }
                                        let tool = request.get("tool_name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown")
                                            .to_string();
                                        let input = request.get("input")
                                            .map(|v| serde_json::Value::Object(
                                                v.as_object().cloned().unwrap_or_default()
                                            ))
                                            .unwrap_or(serde_json::Value::Null);

                                        let tool_name_copy = tool.clone();
                                        let req = PermissionRequest {
                                            id: request_id.clone(),
                                            tool_name: tool,
                                            tool_input: input.clone(),
                                        };

                                        tracing::info!(tool = %req.tool_name, "permission request received");
                                        let _ = perm_req_tx.send(req).await;

                                        if let Some(resp) = perm_resp_rx.recv().await {
                                            let allow = matches!(resp.decision, PermissionDecision::Allow | PermissionDecision::AllowAll);
                                            if allow {
                                                events.push(AgentEvent {
                                                    event_type: AgentEventType::ToolUse {
                                                        name: tool_name_copy.clone(),
                                                        input: input.to_string(),
                                                    },
                                                    timestamp: chrono::Utc::now(),
                                                });
                                            }
                                            let resp_json = if allow {
                                                serde_json::json!({
                                                    "type": "control_response",
                                                    "response": {
                                                        "subtype": "success",
                                                        "request_id": request_id,
                                                        "response": {
                                                            "behavior": "allow",
                                                            "updatedInput": input
                                                        }
                                                    }
                                                })
                                            } else {
                                                serde_json::json!({
                                                    "type": "control_response",
                                                    "response": {
                                                        "subtype": "success",
                                                        "request_id": request_id,
                                                        "response": {
                                                            "behavior": "deny",
                                                            "message": "The user denied this tool use via email."
                                                        }
                                                    }
                                                })
                                            };
                                            let resp_bytes = format!("{}\n", resp_json);
                                            if let Err(e) = stdin_writer.write_all(resp_bytes.as_bytes()).await {
                                                tracing::warn!(error = %e, "failed to write permission response");
                                                break;
                                            }
                                        } else {
                                            let resp_json = serde_json::json!({
                                                "type": "control_response",
                                                "response": {
                                                    "subtype": "success",
                                                    "request_id": request_id,
                                                    "response": {
                                                        "behavior": "deny",
                                                        "message": "Permission channel closed."
                                                    }
                                                }
                                            });
                                            let _ = stdin_writer.write_all(format!("{}\n", resp_json).as_bytes()).await;
                                            break;
                                        }
                                    }
                                    Some("assistant") | Some("text") => {
                                        if let Some(content) = val.get("content")
                                            .or_else(|| val.get("message").and_then(|m| m.get("content")))
                                        {
                                            if let Some(text) = content.as_str() {
                                                result_text.push_str(text);
                                            } else if let Some(arr) = content.as_array() {
                                                for block in arr {
                                                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                                        result_text.push_str(text);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Some("content_block_delta") => {
                                        if let Some(delta) = val.get("delta") {
                                            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                                result_text.push_str(text);
                                            }
                                        }
                                    }
                                    Some("result") => {
                                        if let Some(result) = val.get("result").and_then(|r| r.as_str()) {
                                            if result_text.is_empty() {
                                                result_text.push_str(result);
                                            }
                                        }
                                        if let Some(usage) = val.get("usage") {
                                            let input_tokens = usage.get("input_tokens")
                                                .and_then(|t| t.as_u64())
                                                .unwrap_or(0);
                                            let output_tokens = usage.get("output_tokens")
                                                .and_then(|t| t.as_u64())
                                                .unwrap_or(0);
                                            events.push(AgentEvent {
                                                event_type: AgentEventType::Done { input_tokens, output_tokens },
                                                timestamp: chrono::Utc::now(),
                                            });
                                        }
                                        tracing::info!("result event received, ending session");
                                        break;
                                    }
                                    Some("tool_use") => {
                                        let name = val.get("name")
                                            .or_else(|| val.get("tool").and_then(|t| t.get("name")))
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("unknown")
                                            .to_string();
                                        let input = val.get("input")
                                            .map(|i| i.to_string())
                                            .unwrap_or_default();
                                        events.push(AgentEvent {
                                            event_type: AgentEventType::ToolUse { name, input },
                                            timestamp: chrono::Utc::now(),
                                        });
                                    }
                                    Some("tool_result") => {
                                        let name = val.get("name")
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("unknown")
                                            .to_string();
                                        let output = val.get("output")
                                            .or_else(|| val.get("content"))
                                            .map(|o| if o.is_string() { o.as_str().unwrap_or("").to_string() } else { o.to_string() })
                                            .unwrap_or_default();
                                        let success = val.get("is_error")
                                            .and_then(|e| e.as_bool())
                                            .map(|e| !e)
                                            .unwrap_or(true);
                                        events.push(AgentEvent {
                                            event_type: AgentEventType::ToolResult { name, output, success },
                                            timestamp: chrono::Utc::now(),
                                        });
                                    }
                                    Some("thinking") => {
                                        if let Some(text) = val.get("content").and_then(|c| c.as_str()) {
                                            events.push(AgentEvent {
                                                event_type: AgentEventType::Thinking(text.to_string()),
                                                timestamp: chrono::Utc::now(),
                                            });
                                        }
                                    }
                                    Some("error") => {
                                        let msg = val.get("error")
                                            .or_else(|| val.get("message"))
                                            .and_then(|e| e.as_str())
                                            .unwrap_or("unknown error")
                                            .to_string();
                                        events.push(AgentEvent {
                                            event_type: AgentEventType::Error(msg),
                                            timestamp: chrono::Utc::now(),
                                        });
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "error reading agent stdout");
                            break;
                        }
                    }
                }
                Ok::<(), CcEmailError>(())
            },
        )
        .await;

        drop(stdin_writer);

        if read_result.is_err() {
            let _ = child.kill().await;
            return Err(CcEmailError::Agent(format!(
                "agent timed out after {}s",
                self.timeout_seconds
            )));
        }

        let status = child
            .wait()
            .await
            .map_err(|e| CcEmailError::Agent(format!("agent process error: {}", e)))?;

        let stderr_output = if let Some(stderr) = stderr_handle {
            let mut buf = String::new();
            let mut reader = BufReader::new(stderr);
            let _ = tokio::io::AsyncReadExt::read_to_string(&mut reader, &mut buf).await;
            buf
        } else {
            String::new()
        };

        let mut usage = self.last_usage.lock().unwrap();
        for event in &events {
            if let AgentEventType::Done {
                input_tokens,
                output_tokens,
            } = &event.event_type
            {
                usage.input_tokens += input_tokens;
                usage.output_tokens += output_tokens;
            }
        }

        let generated_files = extract_generated_files(&events);

        Ok(AgentResult {
            success: status.success(),
            stdout: result_text,
            stderr: stderr_output,
            exit_code: status.code(),
            generated_files,
        })
    }
}

fn extract_generated_files(events: &[AgentEvent]) -> Vec<GeneratedFile> {
    let mut files = Vec::new();
    for event in events {
        if let AgentEventType::ToolUse { name, input } = &event.event_type {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(input) {
                match name.as_str() {
                    "Write" | "write" => {
                        if let Some(path) = val.get("file_path").and_then(|v| v.as_str()) {
                            files.push(GeneratedFile {
                                path: PathBuf::from(path),
                                tool_name: name.clone(),
                            });
                        }
                    }
                    "NotebookEdit" | "notebook_edit" => {
                        if let Some(path) = val.get("notebook_path").and_then(|v| v.as_str()) {
                            files.push(GeneratedFile {
                                path: PathBuf::from(path),
                                tool_name: name.clone(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    files
}

#[derive(Debug)]
pub struct PermissionResponse {
    pub id: String,
    pub decision: PermissionDecision,
}

impl AgentRunner for ClaudeCodeAgent {
    fn run(
        &self,
        prompt: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<AgentResult>> + Send + '_>> {
        let prompt = prompt.to_string();
        Box::pin(async move {
            tracing::info!(cmd = %self.binary, model = ?self.model, "running claude code agent");

            let (stdout, stderr, events, success, exit_code) = self.run_stream(&prompt).await?;

            let mut usage = self.last_usage.lock().unwrap();
            for event in &events {
                if let AgentEventType::Done {
                    input_tokens,
                    output_tokens,
                } = &event.event_type
                {
                    usage.input_tokens += input_tokens;
                    usage.output_tokens += output_tokens;
                }
            }

            let generated_files = extract_generated_files(&events);

            Ok(AgentResult {
                success,
                stdout,
                stderr,
                exit_code,
                generated_files,
            })
        })
    }
}

impl AgentRunner for std::sync::Arc<ClaudeCodeAgent> {
    fn run(
        &self,
        prompt: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<AgentResult>> + Send + '_>> {
        (**self).run(prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_code_agent_config() {
        let agent = ClaudeCodeAgent::new(
            "claude".to_string(),
            PathBuf::from("/tmp"),
            Some("claude-sonnet-4-20250514".to_string()),
            "auto".to_string(),
            120,
        );
        assert_eq!(agent.get_model(), Some("claude-sonnet-4-20250514"));
        assert_eq!(agent.get_permission_mode(), "auto");
    }

    #[test]
    fn test_set_model() {
        let mut agent = ClaudeCodeAgent::new(
            "claude".to_string(),
            PathBuf::from("/tmp"),
            None,
            "auto".to_string(),
            120,
        );
        assert_eq!(agent.get_model(), None);
        agent.set_model("claude-opus-4-20250514");
        assert_eq!(agent.get_model(), Some("claude-opus-4-20250514"));
    }

    #[test]
    fn test_usage_tracking() {
        let agent = ClaudeCodeAgent::new(
            "claude".to_string(),
            PathBuf::from("/tmp"),
            None,
            "auto".to_string(),
            120,
        );
        let usage = agent.last_usage();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
    }

    #[test]
    fn test_build_command_with_model() {
        let agent = ClaudeCodeAgent::new(
            "echo".to_string(),
            PathBuf::from("/tmp"),
            Some("claude-sonnet-4-20250514".to_string()),
            "auto".to_string(),
            120,
        );
        let cmd = agent.build_command();
        let prog = cmd.as_std().get_program().to_str().unwrap().to_string();
        assert_eq!(prog, "echo");
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_str().unwrap().to_string())
            .collect();
        assert!(args.contains(&"stream-json".to_string()));
        assert!(args.contains(&"auto".to_string()));
        assert!(args.contains(&"claude-sonnet-4-20250514".to_string()));
    }

    #[test]
    fn test_build_command_without_model() {
        let agent = ClaudeCodeAgent::new(
            "echo".to_string(),
            PathBuf::from("/tmp"),
            None,
            "bypassPermissions".to_string(),
            120,
        );
        let cmd = agent.build_command();
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_str().unwrap().to_string())
            .collect();
        assert!(args.contains(&"bypassPermissions".to_string()));
        assert!(!args.contains(&"--model".to_string()));
    }
}
