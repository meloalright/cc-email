use chrono::Utc;

use crate::agent::claude_code::UsageReport;
use crate::command::CommandRegistry;
use crate::config::{HeartbeatConfig, ProviderConfig};
use crate::cron::CronScheduler;
use crate::mail::formatter::ReplyFormatter;
use crate::relay::RelayManager;
use crate::session::SessionManager;
use crate::webhook::WebhookServer;
use crate::workspace::WorkspaceRouter;

pub fn handle_help(registry: &CommandRegistry) -> String {
    let mut out = String::from("Available commands:\n\n");
    for cmd in registry.list_commands() {
        out.push_str(&format!("  /{:<12} {}\n", cmd.name, cmd.description));
    }
    out
}

pub fn handle_new(sessions: &mut SessionManager, sender: &str, args: &str) -> String {
    let name = if args.is_empty() { None } else { Some(args) };
    let id = sessions.new_session(sender, name);
    match name {
        Some(n) => format!("Created new session {} \"{}\" (now active)", id, n),
        None => format!("Created new session {} (now active)", id),
    }
}

pub fn handle_list(sessions: &SessionManager, sender: &str) -> String {
    let list = sessions.list_sessions(sender);
    if list.is_empty() {
        return "No sessions found.".to_string();
    }

    let active = sessions.get_active_session(sender);
    let active_id = active.map(|s| s.id.as_str());

    let mut out = String::from("Sessions:\n\n");
    for s in &list {
        let marker = if Some(s.id.as_str()) == active_id {
            " *"
        } else {
            ""
        };
        let name_part = s
            .name
            .as_ref()
            .map(|n| format!(" \"{}\"", n))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {}{} — {} ({}, {} turns){}\n",
            s.id,
            name_part,
            s.sender,
            s.status,
            s.history.len(),
            marker,
        ));
    }
    out
}

pub fn handle_switch(sessions: &mut SessionManager, sender: &str, args: &str) -> String {
    if args.is_empty() {
        return "Usage: /switch <name-or-id>".to_string();
    }
    match sessions.switch_session(sender, args) {
        Ok(()) => {
            let active = sessions.get_active_session(sender);
            match active {
                Some(s) => {
                    let name_part = s
                        .name
                        .as_ref()
                        .map(|n| format!(" \"{}\"", n))
                        .unwrap_or_default();
                    format!("Switched to session {}{}", s.id, name_part)
                }
                None => "Switched session.".to_string(),
            }
        }
        Err(e) => format!("Error: {}", e),
    }
}

pub fn handle_name(sessions: &mut SessionManager, sender: &str, args: &str) -> String {
    if args.is_empty() {
        return "Usage: /name <session-name>".to_string();
    }
    let active = sessions.get_active_session(sender);
    match active {
        Some(s) => {
            let id = s.id.clone();
            match sessions.name_session(&id, args) {
                Ok(()) => format!("Session {} named \"{}\"", id, args),
                Err(e) => format!("Error: {}", e),
            }
        }
        None => "No active session.".to_string(),
    }
}

pub fn handle_current(sessions: &SessionManager, sender: &str) -> String {
    match sessions.get_active_session(sender) {
        Some(s) => {
            let name_part = s
                .name
                .as_ref()
                .map(|n| format!("Name: \"{}\"\n", n))
                .unwrap_or_default();
            format!(
                "Session: {}\n\
                 {}\
                 Sender: {}\n\
                 Status: {}\n\
                 History: {} turns\n\
                 Created: {}\n\
                 Updated: {}",
                s.id,
                name_part,
                s.sender,
                s.status,
                s.history.len(),
                s.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
                s.updated_at.format("%Y-%m-%d %H:%M:%S UTC"),
            )
        }
        None => "No active session.".to_string(),
    }
}

pub fn handle_status(
    sessions: &SessionManager,
    agent_type: &str,
    started_at: chrono::DateTime<Utc>,
) -> String {
    let uptime = Utc::now() - started_at;
    let hours = uptime.num_hours();
    let mins = uptime.num_minutes() % 60;
    let secs = uptime.num_seconds() % 60;

    format!(
        "Engine Status:\n\n\
         Uptime: {}h {}m {}s\n\
         Agent: {}\n\
         Sessions: {}",
        hours,
        mins,
        secs,
        agent_type,
        sessions.session_count(),
    )
}

pub fn handle_stop() -> String {
    "No task is currently running.".to_string()
}

pub fn handle_history(sessions: &SessionManager, sender: &str) -> String {
    match sessions.get_active_session(sender) {
        Some(s) => {
            if s.history.is_empty() {
                return "No conversation history.".to_string();
            }

            let start = if s.history.len() > 10 {
                s.history.len() - 10
            } else {
                0
            };

            let mut out = format!("History for session {}:\n\n", s.id);
            for (i, entry) in s.history[start..].iter().enumerate() {
                let ts = entry.timestamp.format("%Y-%m-%d %H:%M");
                let preview: String = entry
                    .content
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect();
                out.push_str(&format!(
                    "[{}] {} — {}: {}\n",
                    start + i + 1,
                    ts,
                    entry.role,
                    preview,
                ));
            }
            out
        }
        None => "No active session.".to_string(),
    }
}

pub fn handle_model(current_model: Option<&str>, agent_type: &str, args: &str) -> String {
    if args.is_empty() || args == "show" {
        let model_display = current_model.unwrap_or("default");
        return format!("Agent: {}\nModel: {}", agent_type, model_display);
    }
    if args == "list" {
        return "Available models:\n\
                \n  claude-sonnet-4-20250514\
                \n  claude-opus-4-20250514\
                \n  claude-haiku-3-5-20241022\
                \n\nUse /model <name> to switch."
            .to_string();
    }
    format!("Model switched to: {}", args)
}

pub fn handle_provider(providers: &[ProviderConfig], args: &str) -> String {
    if args.is_empty() || args == "list" {
        if providers.is_empty() {
            return "No providers configured.\n\nAdd providers in config:\n\n\
                    [[providers]]\nname = \"anthropic\"\napi_key_env = \"ANTHROPIC_API_KEY\"\n\
                    model = \"claude-sonnet-4-20250514\""
                .to_string();
        }
        let mut out = String::from("Configured providers:\n\n");
        for p in providers {
            let model = p.model.as_deref().unwrap_or("default");
            out.push_str(&format!("  {} — model: {}\n", p.name, model));
        }
        out
    } else {
        format!("Provider command: {}", args)
    }
}

pub fn handle_quiet(formatter: &ReplyFormatter, args: &str) -> String {
    if args.is_empty() {
        return format!(
            "Display settings:\n\n\
             show_thinking: {}\n\
             show_tool_use: {}\n\
             reply_footer: {}\n\
             max_output_chars: {}\n\
             max_error_chars: {}\n\n\
             Use /quiet thinking on|off or /quiet tools on|off",
            formatter.show_thinking,
            formatter.show_tool_use,
            formatter.reply_footer,
            formatter.max_output_chars,
            formatter.max_error_chars,
        );
    }

    let parts: Vec<&str> = args.split_whitespace().collect();
    match parts.as_slice() {
        ["thinking", val] => format!("show_thinking set to: {}", val),
        ["tools", val] => format!("show_tool_use set to: {}", val),
        _ => format!(
            "Unknown quiet option: {}. Use: thinking on|off, tools on|off",
            args
        ),
    }
}

pub fn handle_usage(usage: &UsageReport) -> String {
    format!(
        "Usage Statistics:\n\n\
         Input tokens: {}\n\
         Output tokens: {}\n\
         Total tokens: {}",
        usage.input_tokens,
        usage.output_tokens,
        usage.input_tokens + usage.output_tokens,
    )
}

pub fn handle_cron(scheduler: &mut CronScheduler, sender: &str, args: &str) -> String {
    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    let subcmd = parts.first().copied().unwrap_or("");
    let rest = if parts.len() > 1 { parts[1].trim() } else { "" };

    match subcmd {
        "" | "list" => {
            let jobs = scheduler.list_jobs();
            if jobs.is_empty() {
                return "No cron jobs configured.\n\nUsage:\n  /cron add --schedule \"0 9 * * *\" --prompt \"run tests\"".to_string();
            }
            let mut out = String::from("Cron Jobs:\n\n");
            for job in &jobs {
                let status = if job.enabled { "enabled" } else { "disabled" };
                let muted = if job.mute { " [muted]" } else { "" };
                let desc = &job.description;
                let last = job
                    .last_run
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "never".to_string());
                out.push_str(&format!(
                    "  {} — {} ({}{})\n    Schedule: {}\n    Last run: {}\n",
                    job.id, desc, status, muted, job.cron_expr, last,
                ));
            }
            out
        }
        "add" => {
            let mut schedule = None;
            let mut prompt = None;
            let mut exec = None;
            let mut desc = String::from("cron job");

            let tokens: Vec<&str> = rest.split_whitespace().collect();
            let mut i = 0;
            while i < tokens.len() {
                match tokens[i] {
                    "--schedule" if i + 1 < tokens.len() => {
                        let mut expr_parts = Vec::new();
                        i += 1;
                        let val = tokens[i].trim_matches('"');
                        expr_parts.push(val.to_string());
                        while i + 1 < tokens.len()
                            && !tokens[i + 1].starts_with("--")
                            && expr_parts.len() < 5
                        {
                            i += 1;
                            expr_parts.push(tokens[i].trim_matches('"').to_string());
                        }
                        schedule = Some(expr_parts.join(" "));
                    }
                    "--prompt" if i + 1 < tokens.len() => {
                        i += 1;
                        let val = tokens[i..].join(" ");
                        let val = if val.starts_with('"') && val.contains('"') {
                            val.trim_matches('"').to_string()
                        } else {
                            val
                        };
                        prompt = Some(val);
                        break;
                    }
                    "--exec" if i + 1 < tokens.len() => {
                        i += 1;
                        let val = tokens[i..].join(" ");
                        let val = if val.starts_with('"') && val.contains('"') {
                            val.trim_matches('"').to_string()
                        } else {
                            val
                        };
                        exec = Some(val);
                        break;
                    }
                    "--desc" if i + 1 < tokens.len() => {
                        i += 1;
                        desc = tokens[i].trim_matches('"').to_string();
                    }
                    _ => {}
                }
                i += 1;
            }

            if schedule.is_none() {
                return "Missing --schedule. Usage: /cron add --schedule \"0 9 * * *\" --prompt \"task\"".to_string();
            }

            match scheduler.add_job(
                &schedule.unwrap(),
                prompt.as_deref(),
                exec.as_deref(),
                &desc,
                sender,
            ) {
                Ok(id) => format!("Cron job created: {}", id),
                Err(e) => format!("Error: {}", e),
            }
        }
        "del" | "delete" | "rm" => {
            if rest.is_empty() {
                return "Usage: /cron del <job-id>".to_string();
            }
            match scheduler.remove_job(rest) {
                Ok(true) => format!("Cron job {} deleted.", rest),
                Ok(false) => format!("Cron job '{}' not found.", rest),
                Err(e) => format!("Error: {}", e),
            }
        }
        "toggle" => {
            if rest.is_empty() {
                return "Usage: /cron toggle <job-id>".to_string();
            }
            match scheduler.toggle_job(rest) {
                Ok(enabled) => {
                    let state = if enabled { "enabled" } else { "disabled" };
                    format!("Cron job {} is now {}.", rest, state)
                }
                Err(e) => format!("Error: {}", e),
            }
        }
        "mute" => {
            if rest.is_empty() {
                return "Usage: /cron mute <job-id>".to_string();
            }
            match scheduler.mute_job(rest) {
                Ok(muted) => {
                    let state = if muted { "muted" } else { "unmuted" };
                    format!("Cron job {} is now {}.", rest, state)
                }
                Err(e) => format!("Error: {}", e),
            }
        }
        _ => format!(
            "Unknown cron subcommand: {}. Use: list, add, del, toggle, mute",
            subcmd
        ),
    }
}

pub fn handle_heartbeat(config: &HeartbeatConfig, args: &str) -> String {
    match args.trim() {
        "" | "status" => {
            let status = if config.enabled {
                "enabled"
            } else {
                "disabled"
            };
            let schedule = &config.schedule;
            let prompt = config.prompt.as_deref().unwrap_or("(none)");
            let reply_to = config.reply_to.as_deref().unwrap_or("(none)");
            format!(
                "Heartbeat: {}\nSchedule: {}\nPrompt: {}\nReply to: {}",
                status, schedule, prompt, reply_to
            )
        }
        "pause" => "Heartbeat paused.".to_string(),
        "resume" => "Heartbeat resumed.".to_string(),
        "run" => "Heartbeat triggered manually.".to_string(),
        _ => format!(
            "Unknown heartbeat command: {}. Use: status, pause, resume, run",
            args
        ),
    }
}

pub fn handle_relay(relay: &RelayManager, args: &str) -> String {
    match args.trim() {
        "" | "list" => {
            let peers = relay.list_peers();
            if peers.is_empty() {
                return "No relay peers configured.".to_string();
            }
            let mut out = String::from("Relay Peers:\n\n");
            for peer in &peers {
                out.push_str(&format!("  {} — {}\n", peer.name, peer.address));
            }
            out
        }
        _ => format!("Unknown relay command: {}. Use: list", args),
    }
}

pub fn handle_webhook(server: &WebhookServer) -> String {
    let status = if server.is_enabled() {
        "enabled"
    } else {
        "disabled"
    };
    format!(
        "Webhook: {}\nPort: {}\nPath: {}\nPending: {}",
        status,
        server.port(),
        server.path(),
        server.pending_count(),
    )
}

pub fn handle_search(sessions: &SessionManager, sender: &str, query: &str) -> String {
    if query.is_empty() {
        return "Usage: /search <query>".to_string();
    }

    let all_sessions = sessions.list_sessions(sender);
    let query_lower = query.to_lowercase();
    let mut matches = Vec::new();

    for s in &all_sessions {
        let name_match = s
            .name
            .as_ref()
            .map(|n| n.to_lowercase().contains(&query_lower))
            .unwrap_or(false);
        let content_match = s
            .history
            .iter()
            .any(|h| h.content.to_lowercase().contains(&query_lower));

        if name_match || content_match {
            matches.push(s);
        }
    }

    if matches.is_empty() {
        return format!("No sessions matching \"{}\".", query);
    }

    let mut out = format!("Search results for \"{}\":\n\n", query);
    for s in &matches {
        let name_part = s
            .name
            .as_ref()
            .map(|n| format!(" \"{}\"", n))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {}{} — {} turns\n",
            s.id,
            name_part,
            s.history.len()
        ));
    }
    out
}

pub fn handle_memory(args: &str) -> String {
    match args.trim() {
        "" | "show" => "Memory management:\n\n\
             /memory — show this help\n\
             /memory add <text> — append to project memory\n\
             /memory global — show global memory path\n\n\
             Memory files are managed by the agent (CLAUDE.md, etc)."
            .to_string(),
        "global" => "Global memory: ~/.claude/memory/".to_string(),
        text if text.starts_with("add ") => {
            let content = &text[4..];
            format!(
                "Memory note saved: \"{}\"",
                content.chars().take(50).collect::<String>()
            )
        }
        _ => format!("Unknown memory command: {}. Use: show, add, global", args),
    }
}

pub fn handle_workspace(router: &WorkspaceRouter) -> String {
    if !router.is_multi() {
        return "Workspace mode: single (default)\n\nTo enable multi-workspace, add [workspace] to config.".to_string();
    }

    let routes = router.routes();
    let mut out = String::from("Workspace mode: multi\n\nRoutes:\n");
    for route in routes {
        let matcher = if let Some(ref sender) = route.match_sender {
            format!("sender={}", sender)
        } else if let Some(ref prefix) = route.match_subject_prefix {
            format!("subject_prefix={}", prefix)
        } else {
            "default".to_string()
        };
        out.push_str(&format!("  {} -> {}\n", matcher, route.work_dir));
    }
    if let Some(timeout) = router.idle_timeout_mins() {
        out.push_str(&format!("\nIdle timeout: {} mins\n", timeout));
    }
    out
}

pub fn handle_delete(sessions: &mut SessionManager, _sender: &str, args: &str) -> String {
    if args.is_empty() {
        return "Usage: /delete <session-id>".to_string();
    }
    match sessions.delete_session(args) {
        Ok(()) => format!("Session {} deleted.", args),
        Err(e) => format!("Error: {}", e),
    }
}

pub fn handle_history_with_count(sessions: &SessionManager, sender: &str, args: &str) -> String {
    let count: usize = args.trim().parse().unwrap_or(10);
    match sessions.get_active_session(sender) {
        Some(s) => {
            if s.history.is_empty() {
                return "No conversation history.".to_string();
            }

            let start = if s.history.len() > count {
                s.history.len() - count
            } else {
                0
            };

            let mut out = format!("History for session {} (last {}):\n\n", s.id, count);
            for (i, entry) in s.history[start..].iter().enumerate() {
                let ts = entry.timestamp.format("%Y-%m-%d %H:%M");
                let preview: String = entry
                    .content
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect();
                out.push_str(&format!(
                    "[{}] {} — {}: {}\n",
                    start + i + 1,
                    ts,
                    entry.role,
                    preview,
                ));
            }
            out
        }
        None => "No active session.".to_string(),
    }
}

pub fn handle_whoami(sessions: &SessionManager, sender: &str) -> String {
    match sessions.get_active_session(sender) {
        Some(s) => {
            let name_part = s
                .name
                .as_ref()
                .map(|n| format!(" \"{}\"", n))
                .unwrap_or_default();
            format!(
                "Sender: {}\n\
                 Session: {}{}\n\
                 Status: {}",
                sender, s.id, name_part, s.status,
            )
        }
        None => format!("Sender: {}\nNo active session.", sender),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_manager() -> SessionManager {
        SessionManager::new(PathBuf::from("/tmp/test-builtins.json"))
    }

    #[test]
    fn test_help_lists_commands() {
        let registry = CommandRegistry::new();
        let result = handle_help(&registry);
        assert!(result.contains("/help"));
        assert!(result.contains("/new"));
        assert!(result.contains("/doctor"));
    }

    #[test]
    fn test_new_session() {
        let mut mgr = test_manager();
        let result = handle_new(&mut mgr, "user@example.com", "my-project");
        assert!(result.contains("Created new session"));
        assert!(result.contains("my-project"));
    }

    #[test]
    fn test_list_empty() {
        let mgr = test_manager();
        let result = handle_list(&mgr, "user@example.com");
        assert!(result.contains("No sessions found"));
    }

    #[test]
    fn test_whoami() {
        let mut mgr = test_manager();
        mgr.new_session("user@example.com", Some("test"));
        let result = handle_whoami(&mgr, "user@example.com");
        assert!(result.contains("user@example.com"));
        assert!(result.contains("test"));
    }

    #[test]
    fn test_current_no_session() {
        let mgr = test_manager();
        let result = handle_current(&mgr, "user@example.com");
        assert!(result.contains("No active session"));
    }

    #[test]
    fn test_status() {
        let mgr = test_manager();
        let result = handle_status(&mgr, "command", Utc::now());
        assert!(result.contains("Engine Status"));
        assert!(result.contains("command"));
    }

    #[test]
    fn test_history_empty() {
        let mut mgr = test_manager();
        mgr.new_session("user@example.com", None);
        let result = handle_history(&mgr, "user@example.com");
        assert!(result.contains("No conversation history"));
    }

    #[test]
    fn test_stop() {
        let result = handle_stop();
        assert!(result.contains("No task"));
    }

    #[test]
    fn test_model_show() {
        let result = handle_model(Some("claude-sonnet-4-20250514"), "claude-code", "");
        assert!(result.contains("claude-sonnet-4-20250514"));
        assert!(result.contains("claude-code"));
    }

    #[test]
    fn test_model_list() {
        let result = handle_model(None, "command", "list");
        assert!(result.contains("claude-sonnet"));
        assert!(result.contains("claude-opus"));
    }

    #[test]
    fn test_model_switch() {
        let result = handle_model(None, "command", "claude-opus-4-20250514");
        assert!(result.contains("Model switched to: claude-opus-4-20250514"));
    }

    #[test]
    fn test_provider_empty() {
        let result = handle_provider(&[], "");
        assert!(result.contains("No providers configured"));
    }

    #[test]
    fn test_provider_list() {
        let providers = vec![ProviderConfig {
            name: "anthropic".to_string(),
            api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
            base_url: None,
            model: Some("claude-sonnet-4-20250514".to_string()),
        }];
        let result = handle_provider(&providers, "list");
        assert!(result.contains("anthropic"));
        assert!(result.contains("claude-sonnet-4-20250514"));
    }

    #[test]
    fn test_quiet_show() {
        let fmt = ReplyFormatter::default();
        let result = handle_quiet(&fmt, "");
        assert!(result.contains("show_thinking: false"));
        assert!(result.contains("show_tool_use: true"));
    }

    #[test]
    fn test_quiet_toggle() {
        let fmt = ReplyFormatter::default();
        let result = handle_quiet(&fmt, "thinking on");
        assert!(result.contains("show_thinking set to: on"));
    }

    #[test]
    fn test_usage() {
        let usage = UsageReport {
            input_tokens: 1000,
            output_tokens: 500,
            total_cost_usd: None,
        };
        let result = handle_usage(&usage);
        assert!(result.contains("Input tokens: 1000"));
        assert!(result.contains("Output tokens: 500"));
        assert!(result.contains("Total tokens: 1500"));
    }
}
