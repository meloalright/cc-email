use std::path::Path;

use chrono::{DateTime, Utc};

use crate::config::Config;
use crate::cron::CronScheduler;
use crate::session::SessionManager;

pub struct DiagnosticReport {
    pub checks: Vec<DiagnosticCheck>,
}

pub struct DiagnosticCheck {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
}

pub enum CheckStatus {
    Ok,
    Warning,
    Error,
}

impl std::fmt::Display for CheckStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckStatus::Ok => write!(f, "OK"),
            CheckStatus::Warning => write!(f, "WARN"),
            CheckStatus::Error => write!(f, "ERROR"),
        }
    }
}

pub fn run_doctor(
    config: &Config,
    sessions: &SessionManager,
    cron: &CronScheduler,
    started_at: DateTime<Utc>,
) -> DiagnosticReport {
    let checks = vec![
        check_agent_binary(config),
        check_data_dir(&config.session.data_dir),
        check_uptime(started_at),
        check_sessions(sessions),
        check_cron(cron),
        check_inbox_config(config),
        check_outbox_config(config),
    ];

    DiagnosticReport { checks }
}

fn check_agent_binary(config: &Config) -> DiagnosticCheck {
    let binary = if config.agent.agent_type == "claude-code" {
        if config.agent.command.is_empty() {
            "claude"
        } else {
            &config.agent.command
        }
    } else {
        &config.agent.command
    };

    let found = which_exists(binary);
    DiagnosticCheck {
        name: "Agent binary".to_string(),
        status: if found {
            CheckStatus::Ok
        } else {
            CheckStatus::Error
        },
        message: if found {
            format!("{} found", binary)
        } else {
            format!("{} not found in PATH", binary)
        },
    }
}

fn which_exists(binary: &str) -> bool {
    std::process::Command::new("which")
        .arg(binary)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn check_data_dir(data_dir: &str) -> DiagnosticCheck {
    let path = Path::new(data_dir);
    let exists = path.exists();
    DiagnosticCheck {
        name: "Data directory".to_string(),
        status: if exists {
            CheckStatus::Ok
        } else {
            CheckStatus::Warning
        },
        message: if exists {
            format!("{} exists", data_dir)
        } else {
            format!("{} does not exist (will be created)", data_dir)
        },
    }
}

fn check_uptime(started_at: DateTime<Utc>) -> DiagnosticCheck {
    let uptime = Utc::now() - started_at;
    DiagnosticCheck {
        name: "Uptime".to_string(),
        status: CheckStatus::Ok,
        message: format!(
            "{}h {}m {}s",
            uptime.num_hours(),
            uptime.num_minutes() % 60,
            uptime.num_seconds() % 60
        ),
    }
}

fn check_sessions(sessions: &SessionManager) -> DiagnosticCheck {
    DiagnosticCheck {
        name: "Sessions".to_string(),
        status: CheckStatus::Ok,
        message: format!("{} sessions loaded", sessions.session_count()),
    }
}

fn check_cron(cron: &CronScheduler) -> DiagnosticCheck {
    let total = cron.job_count();
    let enabled = cron.enabled_jobs().len();
    DiagnosticCheck {
        name: "Cron jobs".to_string(),
        status: CheckStatus::Ok,
        message: format!("{} total, {} enabled", total, enabled),
    }
}

fn check_inbox_config(config: &Config) -> DiagnosticCheck {
    let has_pw = config.inbox.password.is_some() || config.inbox.password_env.is_some();
    DiagnosticCheck {
        name: "Inbox config".to_string(),
        status: if has_pw {
            CheckStatus::Ok
        } else {
            CheckStatus::Error
        },
        message: if has_pw {
            format!(
                "{}:{} ({})",
                config.inbox.host, config.inbox.port, config.inbox.inbox_type
            )
        } else {
            "No password configured".to_string()
        },
    }
}

fn check_outbox_config(config: &Config) -> DiagnosticCheck {
    let has_pw = config.outbox.password.is_some() || config.outbox.password_env.is_some();
    DiagnosticCheck {
        name: "Outbox config".to_string(),
        status: if has_pw {
            CheckStatus::Ok
        } else {
            CheckStatus::Error
        },
        message: if has_pw {
            format!(
                "{}:{} ({})",
                config.outbox.host, config.outbox.port, config.outbox.outbox_type
            )
        } else {
            "No password configured".to_string()
        },
    }
}

pub fn format_report(report: &DiagnosticReport) -> String {
    let mut out = String::from("Diagnostic Report:\n\n");
    for check in &report.checks {
        out.push_str(&format!(
            "  [{}] {} — {}\n",
            check.status, check.name, check.message
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_report() {
        let report = DiagnosticReport {
            checks: vec![
                DiagnosticCheck {
                    name: "Test".to_string(),
                    status: CheckStatus::Ok,
                    message: "all good".to_string(),
                },
                DiagnosticCheck {
                    name: "Warning".to_string(),
                    status: CheckStatus::Warning,
                    message: "minor issue".to_string(),
                },
            ],
        };
        let formatted = format_report(&report);
        assert!(formatted.contains("[OK] Test"));
        assert!(formatted.contains("[WARN] Warning"));
    }
}
