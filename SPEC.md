# cc-email Feature Spec

Spec for evolving cc-email from a simple IMAP-poll-and-reply daemon into a full-featured, cc-connect-grade engine — adapted for the email transport.

Reference implementation: [cc-connect](https://github.com/anthropics/cc-connect) `core/engine.go` (40 feature groups, 150+ functions).

---

## 1. Engine Core

### 1.1 Initialization & Lifecycle

The current `daemon::run()` loop should become an `Engine` struct that owns all subsystems.

```rust
pub struct Engine {
    config: Config,
    inbox: Box<dyn InboxAdapter>,
    outbox: Box<dyn ReplyHandler>,
    agent: Box<dyn AgentRunner>,
    sessions: SessionManager,
    security: SecurityGuard,
    cron: CronScheduler,
    relay: RelayManager,
    webhook: WebhookServer,
    commands: CommandRegistry,
    state: ProjectStateStore,
}

impl Engine {
    pub async fn start(&mut self) -> Result<()>;
    pub async fn stop(&mut self) -> Result<()>;
    pub async fn reload_config(&mut self, path: &Path) -> Result<()>;
}
```

Lifecycle:
- `start()`: open DB, connect inbox, spawn agent, start cron scheduler, start webhook server, enter poll loop.
- `stop()`: drain in-flight tasks, close agent sessions with graceful timeout (130s for stop hooks), flush state, disconnect.
- `reload_config()`: hot-reload security rules, cron jobs, agent config without restarting.

### 1.2 Platform Lifecycle

Email is inherently async — no persistent connection to manage. But the inbox adapter should support readiness tracking for future adapters (IMAP IDLE, Gmail API push).

```rust
pub trait InboxAdapter: Send + Sync {
    async fn start(&mut self) -> Result<()>;
    async fn fetch_unseen(&self) -> Result<Vec<(String, Vec<u8>)>>;
    async fn stop(&mut self) -> Result<()>;
    fn is_ready(&self) -> bool;
}
```

Future adapters:
- `ImapIdleInbox` — persistent IMAP IDLE connection with reconnect.
- `GmailApiInbox` — Google Pub/Sub push notifications.
- `WebhookInbox` — HTTP endpoint for Cloudflare Email Workers, SendGrid inbound parse, etc.

---

## 2. Session Management

### 2.1 Session Model

Upgrade from flat task table to full session tracking. Each sender gets their own session with conversation history.

```rust
pub struct Session {
    pub id: String,
    pub name: Option<String>,
    pub agent_session_id: Option<String>,
    pub sender: String,                    // email address as session key
    pub history: Vec<HistoryEntry>,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct HistoryEntry {
    pub role: String,       // "user" | "assistant"
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub email_message_id: Option<String>,
}

pub enum SessionStatus {
    Idle,
    Busy,
    Stopped,
}
```

### 2.2 SessionManager

```rust
pub struct SessionManager {
    sessions: HashMap<String, Session>,          // session_id -> Session
    active_session: HashMap<String, String>,     // sender -> active session_id
    sender_sessions: HashMap<String, Vec<String>>, // sender -> list of session_ids
    counter: u64,
    store_path: PathBuf,
}

impl SessionManager {
    pub fn get_or_create_active(&mut self, sender: &str) -> &mut Session;
    pub fn new_session(&mut self, sender: &str, name: Option<&str>) -> &mut Session;
    pub fn switch_session(&mut self, sender: &str, target: &str) -> Result<()>;
    pub fn list_sessions(&self, sender: &str) -> Vec<&Session>;
    pub fn delete_session(&mut self, id: &str) -> Result<()>;
    pub fn save(&self) -> Result<()>;
    pub fn load(path: &Path) -> Result<Self>;
}
```

Persistence: JSON file at `~/.cc-email/sessions.json`.

### 2.3 Session Concurrency

Each session has a lock. If a new email arrives while the session is busy:
- Queue the message.
- After the current agent turn completes, drain the queue.
- If the queue overflows (configurable limit), reply with "busy, try again later".

### 2.4 Auto-Reset on Idle

```toml
[session]
reset_on_idle_mins = 60
```

If a session has been idle longer than the threshold when a new email arrives, automatically create a fresh session (new agent context).

---

## 3. Message Routing & Handling

### 3.1 Inbound Pipeline

```
fetch_unseen()
  → parse_email()
  → validate_sender()           # security allowlist
  → validate_body()             # size limits, empty check
  → detect_command()            # check if body starts with /command
  → resolve_session()           # get or create session for sender
  → acquire_session_lock()      # queue if busy
  → route:
      if command  → handle_command()
      if message  → run_agent()
  → send_reply()
  → mark_processed()
```

### 3.2 Email Threading as Sessions

Map email threads to sessions:
- `In-Reply-To` / `References` headers identify thread membership.
- If an email is part of an existing thread, route to the same session.
- New thread = new conversation turn (or new session if `thread_isolation = true`).

### 3.3 Multi-Sender Support

Each sender gets independent sessions. A shared mailbox can serve multiple users simultaneously. Config:

```toml
[session]
share_session = false           # false = per-sender sessions (default)
                                # true = all senders share one session
```

---

## 4. Command System

### 4.1 Email Commands

Users send commands by putting a `/command` as the first line of the email body.

Built-in commands:

| Command | Description |
|---------|-------------|
| `/help` | List available commands |
| `/new` | Start a new session |
| `/list` | List all sessions |
| `/switch <name>` | Switch to a named session |
| `/name <name>` | Name the current session |
| `/delete <id>` | Delete a session |
| `/current` | Show current session info |
| `/history` | Show conversation history |
| `/status` | Show engine status (uptime, agent, config) |
| `/stop` | Stop the currently running agent |
| `/compress` | Trigger context compression |
| `/model <name>` | Switch model |
| `/mode <mode>` | Switch permission mode |
| `/quiet` | Toggle thinking/tool output in replies |
| `/config` | Show/set config values |
| `/cron` | Manage scheduled jobs |
| `/memory` | Show/append to memory file |
| `/usage` | Show API usage |
| `/whoami` | Show sender/session identity |
| `/doctor` | Run health diagnostics |
| `/version` | Show version info |

### 4.2 Custom Commands

```toml
[[commands]]
name = "deploy"
prompt = "Run the deployment script for the staging environment"

[[commands]]
name = "test"
exec = "cargo test --all"
```

Users invoke with `/deploy` or `/test` in email body.

### 4.3 Alias System

```toml
[[aliases]]
name = "d"
command = "/deploy"

[[aliases]]
name = "t"
command = "/test --verbose"
```

### 4.4 Command Registry

```rust
pub struct CommandRegistry {
    builtins: Vec<Command>,
    custom: Vec<CustomCommand>,
    aliases: HashMap<String, String>,
}

pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
    pub admin_only: bool,
    pub handler: fn(&Engine, &ParsedEmail, &str) -> BoxFuture<Result<String>>,
}
```

---

## 5. Agent System

### 5.1 Agent Trait (Expanded)

```rust
pub trait AgentRunner: Send + Sync {
    async fn start_session(&self, session_id: &str) -> Result<AgentSession>;
    async fn list_sessions(&self) -> Result<Vec<AgentSessionInfo>>;
    async fn stop(&self) -> Result<()>;
    fn name(&self) -> &str;
}

pub trait AgentSession: Send + Sync {
    async fn send(&self, prompt: &str, attachments: &[Attachment]) -> Result<AgentResponse>;
    async fn close(&self) -> Result<()>;
    fn alive(&self) -> bool;
    fn session_id(&self) -> &str;
}

pub struct AgentResponse {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub events: Vec<AgentEvent>,    // for streaming-capable agents
}

pub enum AgentEvent {
    Text(String),
    Thinking(String),
    ToolUse { name: String, input: String },
    ToolResult { name: String, output: String, success: bool },
    PermissionRequest { id: String, tool: String, input: String },
    Error(String),
    Done { input_tokens: u64, output_tokens: u64 },
}
```

### 5.2 Command Agent (Current)

Simple subprocess execution. Prompt goes as argument, stdout/stderr captured.

### 5.3 Claude Code Agent (Stream JSON IPC)

Full streaming IPC with Claude Code CLI:

```rust
pub struct ClaudeCodeAgent {
    binary: String,
    work_dir: PathBuf,
    model: Option<String>,
    permission_mode: String,
    max_context_tokens: Option<u64>,
}
```

Two execution modes:

**One-shot mode** (no permission support):
```bash
claude -p "<prompt>" --output-format stream-json --verbose
```

**Interactive mode** (with permission support, matches cc-connect):
```bash
claude --output-format stream-json --input-format stream-json --verbose --permission-prompt-tool stdio
```

The `--permission-prompt-tool stdio` flag tells Claude Code to emit `control_request` events on stdout and wait for `control_response` on stdin, instead of showing interactive terminal prompts. The `-p` flag is omitted in interactive mode — it conflicts with bidirectional stream-json communication.

IPC protocol:
- **stdin** (interactive mode): `{"type": "user", "message": {"role": "user", "content": "..."}}` for prompts, `{"type": "control_response", ...}` for permission decisions
- **stdout**: Stream of JSON events (`text`, `tool_use`, `tool_result`, `thinking`, `result`, `error`, `control_request`)
- **Session end**: Break the read loop on `result` event to close stdin and let Claude exit gracefully
- **Permission handling**: When `permission_mode = "email"`, permission requests are forwarded to the user via email reply and resolved when they respond

### 5.4 OpenCode Agent (Future)

> **Deferred** — not planned for this version. Spec retained for future reference.
>
> Reference: [cc-connect `agent/opencode/`](https://github.com/chenhg5/cc-connect/tree/main/agent/opencode) — NDJSON streaming via `opencode run --format json`, session resumption via `--session <id>`, model discovery via `opencode models`. No permission protocol (auto-approves in headless mode). See cc-connect source for full implementation details.

### 5.5 Agent Capabilities (Optional Traits)

```rust
pub trait ModelSwitcher {
    fn set_model(&mut self, model: &str);
    fn get_model(&self) -> &str;
    fn available_models(&self) -> Vec<String>;
}

pub trait ProviderSwitcher {
    fn set_active_provider(&mut self, name: &str) -> bool;
    fn get_active_provider(&self) -> Option<&ProviderConfig>;
    fn list_providers(&self) -> &[ProviderConfig];
}

pub trait ModeSwitcher {
    fn set_mode(&mut self, mode: &str);
    fn get_mode(&self) -> &str;
    fn available_modes(&self) -> Vec<String>;
}

pub trait WorkDirSwitcher {
    fn set_work_dir(&mut self, path: &Path);
    fn get_work_dir(&self) -> &Path;
}

pub trait ContextCompressor {
    async fn compress(&self) -> Result<()>;
}

pub trait UsageReporter {
    async fn get_usage(&self) -> Result<UsageReport>;
}

pub trait MemoryFileProvider {
    fn project_memory_file(&self) -> Option<PathBuf>;
    fn global_memory_file(&self) -> Option<PathBuf>;
}

pub trait HealthChecker {
    async fn doctor(&self) -> Result<DiagnosticReport>;
}
```

### 5.5 Provider Config

```rust
pub struct ProviderConfig {
    pub name: String,
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub models: Vec<ModelOption>,
    pub env: HashMap<String, String>,
}
```

```toml
[[providers]]
name = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
model = "claude-sonnet-4-20250514"

[[providers]]
name = "bedrock"
base_url = "https://bedrock-runtime.us-east-1.amazonaws.com"
model = "anthropic.claude-sonnet-4-20250514-v1:0"
```

---

## 6. Reply Formatting

### 6.1 Reply Body Structure

```
Subject: Re: <original subject>

Task: <subject>
Status: COMPLETED | FAILED

--- Agent ---
<model> | <reasoning effort> | <permission mode>

--- Summary ---
<first 10 lines of output>

--- Thinking ---                    (if /quiet is off)
<thinking content, truncated>

--- Tool Usage ---                  (if /quiet is off)
> bash: ls -la /tmp
> read: src/main.rs

--- Output ---
<stdout, truncated to 8000 chars>

--- Errors ---                      (only if stderr non-empty)
<stderr, truncated to 4000 chars>

--- Context ---
Tokens: 1,234 in / 5,678 out
Session: s3 "my-session"
Work dir: /root/project

--
Sent by cc-email
```

### 6.2 Reply Footer (Configurable)

```toml
[display]
reply_footer = true
show_thinking = false               # include thinking in reply
show_tool_use = true                # include tool calls in reply
show_context_indicator = true       # token usage
max_output_chars = 8000
max_error_chars = 4000
```

### 6.3 Attachment Support

For agent outputs that produce files (patches, images, logs):
- Attach files to the reply email as MIME attachments.
- Configurable max attachment size.

---

## 7. Cron & Scheduled Jobs

### 7.1 CronJob Model

```rust
pub struct CronJob {
    pub id: String,
    pub cron_expr: String,              // 5-field cron expression
    pub prompt: Option<String>,         // agent prompt
    pub exec: Option<String>,           // shell command (mutually exclusive)
    pub description: String,
    pub enabled: bool,
    pub mute: bool,                     // suppress reply emails
    pub session_mode: SessionMode,      // Reuse | NewPerRun
    pub timeout_mins: Option<u64>,      // default 30, 0 = unlimited
    pub reply_to: String,               // email address for results
    pub created_at: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

pub enum SessionMode {
    Reuse,
    NewPerRun,
}
```

### 7.2 CronScheduler

```rust
pub struct CronScheduler {
    store: CronStore,
    engine: Arc<Engine>,
    handles: HashMap<String, JoinHandle<()>>,
}

impl CronScheduler {
    pub fn start(&mut self) -> Result<()>;
    pub fn stop(&mut self) -> Result<()>;
    pub fn add_job(&mut self, job: CronJob) -> Result<()>;
    pub fn remove_job(&mut self, id: &str) -> Result<bool>;
    pub fn enable_job(&mut self, id: &str) -> Result<()>;
    pub fn disable_job(&mut self, id: &str) -> Result<()>;
    pub fn list_jobs(&self) -> Vec<&CronJob>;
    pub fn next_run(&self, id: &str) -> Option<DateTime<Utc>>;
}
```

### 7.3 Cron via Email Commands

```
/cron add --schedule "0 9 * * 1-5" --prompt "Run cargo test and report results"
/cron add --schedule "0 6 * * *" --exec "df -h && free -m" --desc "Daily system check"
/cron list
/cron del <job-id>
/cron toggle <job-id>
/cron mute <job-id>
```

### 7.4 Execution Flow

1. Scheduler fires at cron time.
2. Create synthetic email message for the configured prompt/exec.
3. Route through engine (session resolution, agent execution).
4. If `mute = false`, send result email to `reply_to` address.
5. Record `last_run` and `last_error` in store.

Persistence: `~/.cc-email/crons.json`

---

## 8. Heartbeat

### 8.1 Config

```toml
[heartbeat]
enabled = true
schedule = "*/30 * * * *"          # every 30 minutes
prompt = "Check system health and report any issues"
reply_to = "admin@example.com"
```

### 8.2 Implementation

Heartbeat is a special cron job that:
- Runs on a fixed schedule.
- Sends results to a configured address.
- Can be paused/resumed via `/heartbeat pause` and `/heartbeat resume`.
- `/heartbeat run` triggers an immediate execution.

---

## 9. Bot-to-Bot Relay

### 9.1 Relay via Email

Enable multiple cc-email instances to communicate by forwarding messages between mailboxes.

```toml
[[relay.peers]]
name = "code-reviewer"
address = "reviewer-agent@example.com"
```

### 9.2 Relay Protocol

When an agent needs to consult another bot:
1. Send email to peer address with `X-CC-Email-Relay: true` header.
2. Peer processes and replies.
3. Original agent receives reply and continues.

### 9.3 RelayManager

```rust
pub struct RelayManager {
    peers: HashMap<String, RelayPeer>,
    inbox: Arc<dyn InboxAdapter>,
    outbox: Arc<dyn ReplyHandler>,
    timeout: Duration,
}

pub struct RelayPeer {
    pub name: String,
    pub address: String,
}

impl RelayManager {
    pub async fn send(&self, to: &str, message: &str) -> Result<String>;
    pub fn list_peers(&self) -> &HashMap<String, RelayPeer>;
}
```

---

## 10. Webhook Triggers

### 10.1 HTTP Webhook Server

Accept external triggers (CI/CD, git hooks, monitoring) via HTTP.

```toml
[webhook]
enabled = true
port = 9111
path = "/hook"
token_env = "CC_EMAIL_WEBHOOK_TOKEN"
```

### 10.2 Webhook Request

```json
{
  "event": "ci:build_failed",
  "session_key": "admin@example.com",
  "prompt": "The CI build failed on main. Investigate and fix.",
  "silent": false,
  "payload": {
    "repo": "myorg/myrepo",
    "branch": "main",
    "commit": "abc123",
    "log_url": "https://ci.example.com/build/456"
  }
}
```

### 10.3 Webhook Handler

```rust
pub struct WebhookServer {
    config: WebhookConfig,
    engine: Arc<Engine>,
}

impl WebhookServer {
    pub async fn start(&self) -> Result<()>;
    pub async fn stop(&self) -> Result<()>;
}
```

Authentication: `Authorization: Bearer <token>` header.

---

## 11. Security

### 11.1 Current (Keep)

- Sender allowlist.
- Body size limits.
- Attachment size limits.
- No arbitrary shell from email content.

### 11.2 New

- **Rate limiting**: Per-sender message rate limits.
  ```toml
  [security]
  rate_limit_per_minute = 5
  rate_limit_per_hour = 30
  ```

- **Admin authorization**: Certain commands (`/cron add-exec`, `/config set`, `/mode`) restricted to admin senders.
  ```toml
  [security]
  admin_senders = ["admin@example.com"]
  ```

- **Banned words**: Content filter.
  ```toml
  [security]
  banned_words = ["DROP TABLE", "rm -rf /"]
  ```

- **Permission mode enforcement**: Control what the agent can do.
  ```toml
  [agent]
  permission_mode = "default"       # default | auto | plan | acceptEdits | bypassPermissions
  ```

---

## 12. Multi-Workspace

### 12.1 Workspace Mode

Route different senders or subjects to different working directories.

```toml
[workspace]
mode = "multi"
base_dir = "/home/projects"

[[workspace.routes]]
match_sender = "frontend@example.com"
work_dir = "/home/projects/frontend"

[[workspace.routes]]
match_subject_prefix = "[api]"
work_dir = "/home/projects/api"
```

### 12.2 Workspace Agent Isolation

Each workspace gets its own agent instance with independent:
- Working directory
- Session history
- Memory files

### 12.3 Idle Reaper

Workspaces idle for longer than `idle_timeout_mins` get their agent sessions closed to free resources.

```toml
[workspace]
idle_timeout_mins = 120
```

---

## 13. Provider & Model Management

### 13.1 Email Commands

```
/model claude-sonnet-4-20250514
/model list
/provider add anthropic --api-key-env ANTHROPIC_API_KEY
/provider remove bedrock
/provider list
/reasoning high
```

### 13.2 Provider Hot-Switch

When switching providers or models:
1. Stop current agent session.
2. Apply new provider/model config.
3. Start new session with existing history context.
4. Reply with confirmation.

---

## 14. Display & Output Control

### 14.1 Quiet Mode

Toggle verbose output sections in reply emails.

```
/quiet                              # toggle thinking + tool display
/quiet thinking off                 # hide thinking only
/quiet tools off                    # hide tool usage only
```

### 14.2 Config

```toml
[display]
show_thinking = false
show_tool_use = true
show_context_indicator = true
reply_footer = true
max_output_chars = 8000
max_error_chars = 4000
max_thinking_chars = 2000
```

---

## 15. History & Search

### 15.1 Commands

```
/history                            # last 10 turns
/history 20                         # last 20 turns
/search <query>                     # search sessions by name/content
```

### 15.2 History in Reply

When `/history` is invoked via email, reply with a formatted conversation log:

```
[1] 2026-05-20 02:45 — User: fix the login bug
    Assistant: I've fixed the login timeout...

[2] 2026-05-20 02:47 — User: run tests
    Assistant: All 42 tests pass...
```

---

## 16. Diagnostics

### 16.1 /status

Reply with:
- Engine uptime
- Current agent type and model
- Active sessions count
- Inbox connection status
- Last poll time
- Pending tasks in queue
- Cron jobs summary

### 16.2 /doctor

Run health checks:
- IMAP connectivity
- SMTP connectivity (send test to self)
- Agent binary availability
- Database integrity
- Disk space
- Cron scheduler status

### 16.3 /usage

Reply with API usage from agent (if `UsageReporter` is implemented):
- Tokens consumed (input/output)
- Time windows (last 5h, last 7d)
- Rate limit status

### 16.4 /whoami

Reply with:
- Sender email address
- Active session ID and name
- Permission mode
- Workspace (if multi-workspace)
- Rate limit status

---

## 17. Memory Management

### 17.1 Commands

```
/memory                             # show project memory file contents
/memory global                      # show global memory file
/memory add <text>                  # append to project memory
```

### 17.2 Integration

Read/write the agent's CLAUDE.md or memory files. Memory persists across sessions and is loaded into agent context.

---

## 18. Permission Approval via Email

### 18.1 Overview

cc-connect uses interactive cards (Feishu) or inline buttons (Telegram) for real-time tool permission approval. Email has no callback mechanism, but can achieve the same flow through a **reply-based approval loop**: send a permission request email, wait for the user's reply, then forward the decision to Claude Code.

### 18.2 Permission Request Flow

```
Claude Code emits control_request event (via --permission-prompt-tool stdio)
  → Agent module sends request through perm_req_tx channel
  → Engine receives on perm_req_rx, stores PendingPermission
  → Engine sends permission email to sender:
      Subject: Re: <original subject> — Permission Request
      Body:
        Claude wants to use: **Write**
        Input: {"file_path": "/root/cc-email/10.txt", ...}

        Reply with:
        • "allow"  — approve this tool use
        • "deny"   — block this tool use
        • "allow all" — auto-approve all remaining permissions

        ⏳ Waiting for your response (timeout: 5 min)

        --
        Sent by cc-email
  → Engine stores the sent email's Message-ID in perm_message_id
  → Next poll picks up user's reply email
  → Engine matches reply by In-Reply-To/References against both:
      - original task email Message-ID
      - permission email Message-ID (with angle bracket normalization)
  → Engine sends decision through perm_resp_tx channel
  → Agent module writes control_response JSON to Claude's stdin
  → Claude Code continues execution
  → On result event, agent breaks read loop and session ends
```

#### control_request / control_response Protocol

This follows the same protocol as cc-connect. Claude Code emits:

```json
{
  "type": "control_request",
  "request_id": "uuid",
  "request": {
    "subtype": "can_use_tool",
    "tool_name": "Write",
    "input": {"file_path": "...", "content": "..."}
  }
}
```

The engine responds via stdin:

```json
{
  "type": "control_response",
  "response": {
    "subtype": "success",
    "request_id": "uuid",
    "response": {
      "behavior": "allow",
      "updatedInput": {"file_path": "...", "content": "..."}
    }
  }
}
```

For denials, `behavior` is `"deny"` with an optional `message` field.

### 18.3 In-Flight Task Model

```rust
pub struct InFlightTask {
    pub task: Task,
    pub original_email: ParsedEmail,
    pub session_id: String,
    pub join_handle: JoinHandle<Result<AgentResult>>,
    pub perm_req_rx: Receiver<PermissionRequest>,     // from agent
    pub perm_resp_tx: Sender<PermissionResponse>,     // to agent
    pub pending_perm: Option<PendingPermission>,
    pub perm_message_id: Option<String>,              // Message-ID of permission email we sent
    pub approve_all: bool,                            // set by "allow all" reply
}

pub struct PendingPermission {
    pub request_id: String,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub input_preview: String,
    pub asked_at: DateTime<Utc>,
    pub timeout: Duration,
}
```

Each in-flight task holds at most one pending permission at a time. The agent's read loop blocks on the permission channel until a response arrives or timeout occurs.

### 18.4 Response Matching

Reply matching uses two criteria:

1. **Thread matching**: The reply's `In-Reply-To` or `References` header must reference either the original task email's Message-ID or the permission email's Message-ID. Angle brackets are normalized before comparison (mail parsers strip them inconsistently).

2. **Body keyword matching**:

```rust
fn match_permission_response(body: &str) -> Option<PermissionDecision> {
    let lower = body.trim().to_lowercase();
    if is_approve_all(&lower) {   // "allow all", "approve all", "yes all"
        Some(PermissionDecision::AllowAll)
    } else if is_allow(&lower) {  // "allow", "yes", "ok", "y", "allow"
        Some(PermissionDecision::Allow)
    } else if is_deny(&lower) {   // "deny", "no", "n", "reject"
        Some(PermissionDecision::Deny)
    } else {
        None  // not a permission response, treat as normal message
    }
}
```

Stale permission replies (arriving after task timeout) are detected by checking if the email subject contains "Permission Request" and body matches a permission keyword — these are skipped rather than spawning a new task.

If the reply doesn't match a permission keyword, treat it as a regular message and re-queue it for after the permission resolves.

### 18.5 Timeout Handling

Email round-trip is slow (polling interval + user think time). Configurable timeout:

```toml
[agent]
permission_timeout_seconds = 300   # 5 minutes default
permission_default = "deny"        # deny | allow — action on timeout
```

On timeout:
- Apply `permission_default` action (deny by default).
- Send a follow-up email: "⏰ Permission request timed out. Tool use was denied."
- Agent continues with the denial.

### 18.6 Allow-All Mode

When the user replies "allow all":
- Set `approve_all = true` on the session state.
- All subsequent `PermissionRequest` events in this session are auto-approved without sending emails.
- Send confirmation: "✅ All permissions auto-approved for this session."
- Flag resets on new session or `/new` command.

### 18.7 Permission Email Format

The permission request email includes enough context for the user to make an informed decision:

```
Subject: Re: fix the login bug — ⚠️ Permission Request

⚠️ Permission Request

Claude wants to use: **Bash**

Command:
  rm -rf /tmp/old-build && cargo build --release

Working directory: /root/cc-email

Reply with one of:
  • allow     — approve this tool use
  • deny      — block this tool use
  • allow all — auto-approve all remaining permissions

⏳ This request will timeout in 5 minutes.
If no response, the tool use will be denied.

--
Sent by cc-email
```

### 18.8 Engine Integration

The permission flow uses async channels between the engine and agent:

1. **Agent side** (`run_with_permissions` in `claude_code.rs`):
   - Spawns Claude Code with `--permission-prompt-tool stdio` and `--input-format stream-json`
   - Sends the user prompt via stdin as a `user` message
   - Reads stdout line-by-line, parsing JSON events
   - On `control_request` with `subtype: "can_use_tool"`: sends `PermissionRequest` through `perm_req_tx`, blocks on `perm_resp_rx`
   - On response: writes `control_response` JSON to Claude's stdin
   - On `result` event: breaks the read loop and exits cleanly

2. **Engine side** (`check_in_flight` in `engine.rs`):
   - Polls `perm_req_rx` for new permission requests (non-blocking `try_recv`)
   - If `approve_all` is set, auto-approves without email
   - Otherwise, sends permission email and stores `PendingPermission` + `perm_message_id`
   - Each poll cycle checks incoming emails for permission replies (thread-matched)
   - On match, sends `PermissionResponse` through `perm_resp_tx`
   - On timeout, applies `permission_default` (deny) and sends timeout notification

3. **Message-ID handling**:
   - Reply emails use the sender's actual domain for Message-ID (e.g., `@gmail.com`) to avoid SPF/DKIM rejection by recipients
   - `ReplyHandler::send_reply` returns the generated Message-ID as `Result<String>`
   - The engine tracks this ID for thread matching on the user's reply

### 18.9 Comparison with cc-connect

Both cc-connect and cc-email use the same Claude Code IPC protocol (`--permission-prompt-tool stdio`, `control_request`/`control_response`). The difference is the user-facing transport:

| Aspect | cc-connect | cc-email |
|--------|-----------|----------|
| **Claude Code flags** | Same: `--permission-prompt-tool stdio --input-format stream-json --output-format stream-json --verbose` | Same |
| **Protocol** | `control_request` / `control_response` | Same |
| **Prompt delivery** | Interactive card with buttons (Feishu/Telegram) | Email with reply instructions |
| **User response** | Button click (instant) | Reply email (15-30s polling) |
| **Round-trip time** | <1 second | 15-60 seconds |
| **Thread matching** | Card callback ID | Email In-Reply-To / References + Message-ID |
| **Allow All** | Session-scoped flag | Same |
| **Timeout** | Configurable | Configurable (default 5min) |
| **Multi-language** | EN/ZH/JA/ES | EN (extensible) |
| **Visual feedback** | In-place card update | Follow-up email |

### 18.10 Config

```toml
[agent]
permission_mode = "email"              # auto | email | deny_all | allow_all
permission_timeout_seconds = 300
permission_default = "deny"            # deny | allow — fallback on timeout
```

Permission modes:
- `auto` — agent handles permissions internally (current behavior, no email approval)
- `email` — send permission emails and wait for reply (new)
- `deny_all` — auto-deny all permission requests
- `allow_all` — auto-approve all permission requests (use with caution)

---

## 18b. Voice & Attachment Processing

### 18.1 Audio Attachments

If an email contains an audio attachment (.mp3, .wav, .ogg):
1. Extract the attachment.
2. Transcribe via configured STT service.
3. Use transcription as the prompt.

```toml
[speech]
stt_provider = "whisper"            # whisper | google | azure
stt_api_key_env = "OPENAI_API_KEY"
```

### 18.2 Image Attachments

If the agent supports vision (Claude), extract inline images and pass as attachments to the agent.

### 18.3 File Attachments

Extract file attachments, save to temp dir, and include paths in the agent prompt context.

---

## 18c. Attachment Send-Back

### 18c.1 Overview

When Claude Code generates files during task execution (patches, images, logs, scripts, data exports), cc-email should attach them to the reply email instead of dumping file contents inline. This mirrors cc-connect's ability to send generated files back to the chat platform.

cc-connect reference: `sendImageMessage()`, `sendFileMessage()` in `core/engine.go` — the agent writes files to a temp directory, and the engine attaches them to the platform message.

### 18c.2 Detection: Which Files to Attach

The agent stream-json output includes `tool_use` and `tool_result` events. cc-email should track file-producing tool calls:

```rust
pub struct GeneratedFile {
    pub path: PathBuf,
    pub tool_name: String,       // "Write", "Bash", "Edit"
    pub created_at: DateTime<Utc>,
    pub size: u64,
}
```

Detection rules:
1. **Write tool**: Any `tool_use` with `name = "Write"` — extract `file_path` from input. The file is created by Claude.
2. **Bash tool**: Parse `tool_use` input command for output redirection (`> file`, `>> file`, `tee file`). Also detect common generators (`curl -o`, `wget -O`, `cp`, `mv`, `convert`, `ffmpeg -o`).
3. **NotebookEdit tool**: Track the notebook path.
4. **Explicit send-back**: If Claude's text output contains a marker like `[attach: /path/to/file]`, treat it as an explicit request to attach that file.

### 18c.3 File Collection

After the agent completes (on `result` event), collect all generated files:

```rust
pub struct AttachmentCollector {
    generated_files: Vec<GeneratedFile>,
    work_dir: PathBuf,
    max_attachment_size: u64,
    max_total_size: u64,
    allowed_extensions: Option<Vec<String>>,
}

impl AttachmentCollector {
    pub fn track_tool_use(&mut self, tool_name: &str, input: &serde_json::Value);
    pub fn collect(&self) -> Vec<AttachmentFile>;
}

pub struct AttachmentFile {
    pub filename: String,
    pub content_type: String,     // MIME type
    pub data: Vec<u8>,
}
```

Collection logic:
1. Filter out files that no longer exist (deleted during session).
2. Filter out files exceeding `max_attachment_size`.
3. Skip binary files unless they are images or known safe types.
4. Determine MIME type from extension.
5. Total attachment size must not exceed `max_total_size`.

### 18c.4 MIME Email Construction

Replace the current plain-text email with a `multipart/mixed` MIME message when attachments are present:

```
Content-Type: multipart/mixed; boundary="----cc-email-boundary"

------cc-email-boundary
Content-Type: text/plain; charset=utf-8

<reply body text>

------cc-email-boundary
Content-Type: application/octet-stream
Content-Disposition: attachment; filename="patch.diff"
Content-Transfer-Encoding: base64

<base64 encoded file>

------cc-email-boundary--
```

Using `lettre`'s `MultiPart` and `Attachment` builders:

```rust
use lettre::message::{MultiPart, SinglePart, Attachment, header::ContentType};

fn build_email_with_attachments(
    body: &str,
    attachments: &[AttachmentFile],
) -> MultiPart {
    let text_part = SinglePart::builder()
        .content_type(ContentType::TEXT_PLAIN)
        .body(body.to_string());

    let mut multipart = MultiPart::mixed().singlepart(text_part);

    for file in attachments {
        let content_type: ContentType = file.content_type.parse()
            .unwrap_or(ContentType::parse("application/octet-stream").unwrap());
        let attachment = Attachment::new(file.filename.clone())
            .body(file.data.clone(), content_type);
        multipart = multipart.singlepart(attachment);
    }

    multipart
}
```

### 18c.5 Reply Handler Changes

`SmtpReplier::do_send` currently builds a plain-text `Message`. When attachments are present, switch to `multipart/mixed`:

```rust
// In do_send():
let email = if attachments.is_empty() {
    builder.body(body)?
} else {
    let multipart = build_email_with_attachments(&body, attachments);
    builder.multipart(multipart)?
};
```

The `ReplyHandler` trait needs an optional attachments parameter:

```rust
pub trait ReplyHandler: Send + Sync {
    fn send_reply<'a>(
        &'a self,
        original: &'a ParsedEmail,
        task: &'a Task,
        body: &'a str,
        subject_override: &'a str,
        attachments: &'a [AttachmentFile],
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;
}
```

### 18c.6 Engine Integration

In `finalize_agent_result()`:

```rust
// After formatting the reply body:
let attachments = self.collect_attachments(&task, &agent_result);

self.outbox
    .send_reply(original_email, &task, &formatted, &result_subject, &attachments)
    .await?;
```

The `AgentResult` should carry the list of generated file paths:

```rust
pub struct AgentResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub generated_files: Vec<GeneratedFile>,  // NEW
}
```

### 18c.7 Config

```toml
[attachments]
enabled = true
max_file_size_bytes = 10000000        # 10MB per file
max_total_size_bytes = 25000000       # 25MB total per email
allowed_extensions = ["txt", "md", "rs", "py", "js", "ts", "json", "toml", "yaml",
                      "csv", "log", "diff", "patch", "png", "jpg", "svg", "pdf",
                      "html", "css", "sh", "sql"]
# blocked_extensions = ["exe", "bin", "so", "dll"]
attach_mode = "auto"                  # auto | explicit | off
                                      # auto: attach all detected generated files
                                      # explicit: only attach files with [attach:] marker
                                      # off: never attach, inline content only
```

### 18c.8 Security Considerations

- **Size limits**: Enforce per-file and total size limits to avoid email delivery failures (Gmail limit: 25MB).
- **Extension allowlist**: Only attach known-safe file types. Block executables by default.
- **Path traversal**: Validate that attached file paths are within the agent's work directory. Reject paths containing `..` or absolute paths outside work_dir.
- **Sensitive files**: Never attach files matching patterns like `.env`, `credentials.*`, `*.key`, `*.pem`, `id_rsa*`, `*.secret`. Configurable blocklist.
- **Binary detection**: Skip large binary files unless explicitly in the allowed extensions list.

### 18c.9 Comparison with cc-connect

| Aspect | cc-connect | cc-email |
|--------|-----------|----------|
| **Delivery** | Platform file upload API | MIME email attachment |
| **Size limit** | Platform-dependent (Feishu: 30MB) | 25MB (Gmail/SMTP limit) |
| **Image preview** | Inline image in chat | Attachment (no inline preview) |
| **Detection** | Agent writes to temp dir | Track Write/Bash tool_use events |
| **Streaming** | Send files as they're created | Batch all at task completion |

---

## 19. Daemon Management

### 19.1 CLI Commands

```bash
cc-email daemon install             # install systemd/launchd service
cc-email daemon uninstall
cc-email daemon start
cc-email daemon stop
cc-email daemon restart
cc-email daemon status
cc-email daemon logs --follow
```

### 19.2 Implementation

```rust
pub trait DaemonManager {
    fn install(&self, config: &DaemonConfig) -> Result<()>;
    fn uninstall(&self) -> Result<()>;
    fn start(&self) -> Result<()>;
    fn stop(&self) -> Result<()>;
    fn restart(&self) -> Result<()>;
    fn status(&self) -> Result<DaemonStatus>;
}
```

Platform detection:
- Linux: systemd
- macOS: launchd
- Windows: Windows Service (future)

---

## 20. CLI Expansion

### 20.1 Subcommands

```
cc-email listen --config <path>             # start listener (current)
cc-email daemon <install|start|stop|...>    # service management
cc-email send --to <addr> --body <text>     # send a one-off email
cc-email sessions list                      # list sessions
cc-email sessions delete <id>               # delete session
cc-email cron add --schedule "..." --prompt "..."
cc-email cron list
cc-email cron del <id>
cc-email config get <key>
cc-email config set <key> <value>
cc-email doctor                             # health check
cc-email version                            # version info
cc-email upgrade                            # check and apply updates
```

---

## 21. Persistence Layer

### 21.1 Current: SQLite (Tasks)

Keep the existing `tasks` table for task/email tracking.

### 21.2 New: JSON State Files

Following cc-connect's pattern, store runtime state as JSON:

```
~/.cc-email/
├── sessions.json               # SessionManager state
├── crons.json                  # CronScheduler jobs
├── relay.json                  # RelayManager bindings
├── daemon.json                 # service metadata
├── config.toml                 # copied/linked config
└── logs/
    └── <task-id>.log           # per-task logs (current)
```

### 21.3 SQLite for Tasks (Keep)

```sql
CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    email_message_id TEXT NOT NULL UNIQUE,
    session_id TEXT,                     -- NEW: link to session
    sender TEXT NOT NULL,
    subject TEXT NOT NULL,
    prompt TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    result_summary TEXT,
    raw_log_path TEXT,
    input_tokens INTEGER,               -- NEW: usage tracking
    output_tokens INTEGER               -- NEW: usage tracking
);
```

---

## 22. Configuration (Full)

```toml
# cc-email.toml — Full configuration reference

[inbox]
type = "imap"                           # imap | gmail_api | webhook
host = "imap.gmail.com"
port = 993
username = "agent@example.com"
password_env = "CC_EMAIL_IMAP_PASSWORD"
folder = "INBOX"
poll_interval_seconds = 15
search_to = "agent+task@example.com"    # filter by recipient

[outbox]
type = "smtp"
host = "smtp.gmail.com"
port = 587
username = "agent@example.com"
password_env = "CC_EMAIL_SMTP_PASSWORD"
from = "agent+task@example.com"

[agent]
type = "claude-code"                    # command | claude-code
command = "claude"
timeout_seconds = 600
work_dir = "/home/projects"
model = "claude-sonnet-4-20250514"
permission_mode = "email"               # auto | email | deny_all | allow_all
permission_timeout_seconds = 300
permission_default = "deny"             # deny | allow — fallback on timeout

[security]
allowed_senders = ["me@example.com"]
admin_senders = ["me@example.com"]
max_body_bytes = 20000
max_attachment_bytes = 5000000
rate_limit_per_minute = 5
rate_limit_per_hour = 30
banned_words = []

[session]
reset_on_idle_mins = 60
share_session = false
thread_isolation = false
max_queue_size = 5

[display]
show_thinking = false
show_tool_use = true
show_context_indicator = true
reply_footer = true
max_output_chars = 8000
max_error_chars = 4000

[workspace]
mode = "single"                         # single | multi
base_dir = ""
idle_timeout_mins = 120

[[workspace.routes]]
match_sender = ""
match_subject_prefix = ""
work_dir = ""

[[providers]]
name = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
model = "claude-sonnet-4-20250514"

[[commands]]
name = "deploy"
prompt = "Run the deployment script"

[[commands]]
name = "test"
exec = "cargo test --all"

[[aliases]]
name = "d"
command = "/deploy"

[cron]
default_session_mode = "reuse"          # reuse | new_per_run
default_timeout_mins = 30

[heartbeat]
enabled = false
schedule = "*/30 * * * *"
prompt = "Check system health"
reply_to = ""

[[relay.peers]]
name = ""
address = ""

[webhook]
enabled = false
port = 9111
path = "/hook"
token_env = "CC_EMAIL_WEBHOOK_TOKEN"

[speech]
stt_provider = ""
stt_api_key_env = ""

[tts]
enabled = false
provider = ""

[log]
level = "info"                          # trace | debug | info | warn | error
file = ""
max_size_mb = 50

language = "en"                         # en | zh
```

---

## 22b. Email Provider Compatibility

cc-email works with any email provider that supports standard IMAP + SMTP. Below are tested/supported provider configurations.

### 22b.1 Gmail

```toml
[inbox]
type = "imap"
host = "imap.gmail.com"
port = 993
username = "you@gmail.com"
password_env = "CC_EMAIL_IMAP_PASSWORD"   # App Password (2FA required)

[outbox]
type = "smtp"
host = "smtp.gmail.com"
port = 587
username = "you@gmail.com"
password_env = "CC_EMAIL_SMTP_PASSWORD"   # Same App Password
from = "you@gmail.com"
```

**Notes:**
- Requires [App Password](https://myaccount.google.com/apppasswords) — regular password won't work with IMAP
- Gmail blocks `.js`, `.sh`, `.exe` attachments (552 security filter) — these are excluded from `DEFAULT_ALLOWED_EXTENSIONS`
- 25 MB attachment limit per email

### 22b.2 NetEase 163 Mail

```toml
[inbox]
type = "imap"
host = "imap.163.com"
port = 993
username = "you@163.com"
password_env = "CC_EMAIL_IMAP_PASSWORD"   # Authorization code (not login password)

[outbox]
type = "smtp"
host = "smtp.163.com"
port = 465
username = "you@163.com"
password_env = "CC_EMAIL_SMTP_PASSWORD"   # Same authorization code
from = "you@163.com"
```

**Notes:**
- Must enable IMAP/SMTP in 163 mail settings: Settings → POP3/SMTP/IMAP → Enable IMAP and SMTP
- Uses **authorization code** (授权码), not login password — generated when enabling IMAP/SMTP service
- SMTP port 465 uses implicit TLS (SSL); port 25 is plain text (not recommended)
- IMAP port 993 uses implicit TLS
- 50 MB attachment limit per email
- NetEase may rate-limit frequent sends — if hitting limits, increase `poll_interval_seconds`

### 22b.3 Other Providers (Reference)

| Provider | IMAP Host | IMAP Port | SMTP Host | SMTP Port | Auth Notes |
|----------|-----------|-----------|-----------|-----------|------------|
| Gmail | imap.gmail.com | 993 | smtp.gmail.com | 587 | App Password |
| 163 Mail | imap.163.com | 993 | smtp.163.com | 465 | Authorization code (授权码) |
| QQ Mail | imap.qq.com | 993 | smtp.qq.com | 465 | Authorization code (授权码) |
| Outlook | outlook.office365.com | 993 | smtp.office365.com | 587 | App Password or OAuth |
| Yahoo | imap.mail.yahoo.com | 993 | smtp.mail.yahoo.com | 465 | App Password |
| iCloud | imap.mail.me.com | 993 | smtp.mail.me.com | 587 | App-specific password |
| Fastmail | imap.fastmail.com | 993 | smtp.fastmail.com | 465 | App Password |

### 22b.4 Implementation Notes

The current IMAP/SMTP implementation uses `async-native-tls` for TLS. Both implicit TLS (port 993/465) and STARTTLS (port 587) are supported — the connection mode is auto-detected based on port:

- **Port 993 (IMAP) / 465 (SMTP)**: Implicit TLS — TLS handshake on connect
- **Port 587 (SMTP)**: STARTTLS — plain connect, then upgrade to TLS
- **Port 143 (IMAP) / 25 (SMTP)**: Plain text — not recommended, no encryption

If a provider requires implicit TLS on a non-standard port, the implementation may need a `tls_mode` config field (future enhancement).

---

## 23. Architecture

```
src/
├── main.rs                         # CLI entry point (clap subcommands)
├── lib.rs                          # Public API, module declarations
├── cli.rs                          # clap CLI definitions
├── engine.rs                       # Engine orchestrator, in-flight task mgmt
├── config.rs                       # TOML config parsing
├── error.rs                        # Error types
├── security.rs                     # SecurityGuard (sender allowlist, body validation)
├── permission.rs                   # Email-based permission approval flow
├── attachment.rs                   # Attachment collection, file scanning, security
├── diagnostics.rs                  # /doctor health checks
├── daemon.rs                       # Legacy daemon loop (pre-engine)
│
├── inbox/
│   ├── mod.rs                      # InboxAdapter trait
│   └── imap_poll.rs                # IMAP polling implementation
│
├── mail/
│   ├── mod.rs
│   ├── parser.rs                   # MIME email parsing (mail-parser)
│   ├── reply.rs                    # SmtpReplier, ReplyHandler trait, MIME attachments
│   └── formatter.rs                # Reply body formatting (configurable)
│
├── agent/
│   ├── mod.rs                      # AgentRunner trait
│   ├── command_runner.rs           # Generic command subprocess agent
│   └── claude_code.rs              # Claude Code stream-json IPC + permissions
│
├── session/
│   └── mod.rs                      # Session, SessionManager, history, persistence
│
├── task/
│   ├── mod.rs
│   ├── model.rs                    # Task model (id, status, summary)
│   └── store.rs                    # SQLite task persistence
│
├── command/
│   ├── mod.rs                      # CommandRegistry, detect_and_parse_command
│   └── builtins.rs                 # Built-in command handlers (/new, /doctor, /help)
│
├── cron.rs                         # CronScheduler, CronJob, persistence
├── relay.rs                        # RelayManager, peer config
├── webhook.rs                      # WebhookServer, config
└── workspace.rs                    # WorkspaceRouter, sender/subject matching

tests/
└── integration.rs                  # Integration tests
```

---

## 24. Implementation Status

Legend: [x] done, [~] partial, [ ] todo

### v0.0.2 — Core (current)
- [x] Engine struct orchestrator
- [x] Gmail IMAP/SMTP (primary supported provider)
- [x] Claude Code stream-json agent (primary supported agent)
- [x] Permission approval via email
- [x] Attachment send-back (time-based file scan, multipart MIME)
- [x] Session management (per-sender sessions, history)
- [x] Task persistence (SQLite)
- [x] Security (sender allowlist, body validation)
- [x] Reply formatting (configurable sections)
- [x] `/new` — start new session
- [x] `/doctor` — diagnostics
- [x] `/help` — list commands
- [x] Homebrew tap + npm publish

### Future — Commands (WIP)
- [ ] `/stop` — abort running agent task
- [ ] `/status` — show task and session status
- [ ] `/history` — conversation history
- [ ] `/model` — switch model
- [ ] `/usage` — usage reporting
- [ ] `/cron` — scheduled tasks
- [ ] `/relay` — bot-to-bot relay
- [ ] `/compress` — context compression
- [ ] `/config` — hot-reload via email
- [ ] `/upgrade` — self-update
- [ ] Custom commands & aliases

### Future — Providers (WIP)
- [ ] Outlook
- [ ] Yahoo
- [ ] iCloud
- [ ] 163 Mail
- [ ] QQ Mail

### Future — Agents (WIP)
- [ ] Custom command agent
- [ ] OpenCode
- [ ] Codex

### Future — Advanced
- [ ] Rate limiting (config exists, not enforced)
- [ ] Admin authorization (config exists, not enforced)
- [ ] Session auto-reset on idle (config exists, not enforced)
- [ ] Heartbeat (config parsed, not fully wired)
- [ ] Webhook server (config + model, not fully wired)
- [ ] Daemon management CLI (systemd/launchd)
- [ ] Voice/attachment processing (inbound)
- [ ] i18n (en/zh)

### Recent Changes (2026-05)
- v0.0.1: Initial release — Gmail + Claude Code as primary supported stack
- v0.0.1: Homebrew tap and npm package publishing
- v0.0.1: Implemented attachment send-back via time-based file scanning
- v0.0.1: Added email provider compatibility guide
- v0.0.2: Trimmed to 3 commands only: /new, /doctor, /help — all other command dispatch removed from engine
- v0.0.2: Removed unused `usage` field from Engine struct
- v0.0.2: Cleaned up dead integration tests
