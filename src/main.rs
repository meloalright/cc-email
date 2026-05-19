mod cli;

use std::path::PathBuf;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use cc_email::config::Config;
use cc_email::cron::CronScheduler;
use cc_email::engine::Engine;
use cc_email::session::SessionManager;
use cli::{Cli, Commands, CronAction, SessionAction};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Listen { config } => {
            let cfg = load_config(&config);

            tracing::info!(config = %config.display(), "starting cc-email");

            let mut engine = match Engine::new(cfg) {
                Ok(e) => e,
                Err(e) => {
                    tracing::error!(error = %e, "failed to initialize engine");
                    std::process::exit(1);
                }
            };

            if let Err(e) = engine.start().await {
                tracing::error!(error = %e, "engine error");
                std::process::exit(1);
            }
        }

        Commands::Send {
            to,
            subject,
            body,
            config,
        } => {
            let cfg = load_config(&config);
            println!("Sending email to {} ...", to);
            println!("Subject: {}", subject);
            println!("Body: {}", body);
            println!("SMTP: {}:{}", cfg.outbox.host, cfg.outbox.port);
            println!("(Use 'listen' mode for full agent integration)");
        }

        Commands::Sessions { action } => match action {
            SessionAction::List { config } => {
                let cfg = load_config(&config);
                let data_dir = PathBuf::from(&cfg.session.data_dir);
                let sessions_path = data_dir.join("sessions.json");
                match SessionManager::load(&sessions_path) {
                    Ok(mgr) => {
                        println!("Sessions ({} total):", mgr.session_count());
                        for sender in mgr.all_senders() {
                            for s in mgr.list_sessions(&sender) {
                                let name = s
                                    .name
                                    .as_ref()
                                    .map(|n| format!(" \"{}\"", n))
                                    .unwrap_or_default();
                                println!(
                                    "  {}{} — {} ({}, {} turns)",
                                    s.id,
                                    name,
                                    s.sender,
                                    s.status,
                                    s.history.len()
                                );
                            }
                        }
                    }
                    Err(_) => println!("No sessions found."),
                }
            }
            SessionAction::Delete { id, config } => {
                let cfg = load_config(&config);
                let data_dir = PathBuf::from(&cfg.session.data_dir);
                let sessions_path = data_dir.join("sessions.json");
                match SessionManager::load(&sessions_path) {
                    Ok(mut mgr) => match mgr.delete_session(&id) {
                        Ok(()) => {
                            mgr.save().ok();
                            println!("Session {} deleted.", id);
                        }
                        Err(e) => println!("Error: {}", e),
                    },
                    Err(_) => println!("No sessions found."),
                }
            }
        },

        Commands::Cron { action } => match action {
            CronAction::List { config } => {
                let cfg = load_config(&config);
                let data_dir = PathBuf::from(&cfg.session.data_dir);
                match CronScheduler::load(&data_dir) {
                    Ok(sched) => {
                        let jobs = sched.list_jobs();
                        if jobs.is_empty() {
                            println!("No cron jobs.");
                        } else {
                            for job in jobs {
                                let status = if job.enabled { "enabled" } else { "disabled" };
                                println!(
                                    "  {} — {} ({}) [{}]",
                                    job.id, job.description, job.cron_expr, status
                                );
                            }
                        }
                    }
                    Err(_) => println!("No cron jobs found."),
                }
            }
            CronAction::Add {
                schedule,
                prompt,
                exec,
                desc,
                reply_to,
                config,
            } => {
                let cfg = load_config(&config);
                let data_dir = PathBuf::from(&cfg.session.data_dir);
                let mut sched = CronScheduler::load(&data_dir)
                    .unwrap_or_else(|_| CronScheduler::new(&data_dir));
                match sched.add_job(
                    &schedule,
                    prompt.as_deref(),
                    exec.as_deref(),
                    &desc,
                    &reply_to,
                ) {
                    Ok(id) => println!("Cron job created: {}", id),
                    Err(e) => println!("Error: {}", e),
                }
            }
            CronAction::Del { id, config } => {
                let cfg = load_config(&config);
                let data_dir = PathBuf::from(&cfg.session.data_dir);
                let mut sched = CronScheduler::load(&data_dir)
                    .unwrap_or_else(|_| CronScheduler::new(&data_dir));
                match sched.remove_job(&id) {
                    Ok(true) => println!("Cron job {} deleted.", id),
                    Ok(false) => println!("Cron job '{}' not found.", id),
                    Err(e) => println!("Error: {}", e),
                }
            }
        },

        Commands::Doctor { config } => {
            let cfg = load_config(&config);
            let data_dir = PathBuf::from(&cfg.session.data_dir);
            let sessions_path = data_dir.join("sessions.json");
            let sessions = SessionManager::load(&sessions_path)
                .unwrap_or_else(|_| SessionManager::new(sessions_path));
            let cron =
                CronScheduler::load(&data_dir).unwrap_or_else(|_| CronScheduler::new(&data_dir));

            let report =
                cc_email::diagnostics::run_doctor(&cfg, &sessions, &cron, chrono::Utc::now());
            print!("{}", cc_email::diagnostics::format_report(&report));
        }

        Commands::Version => {
            println!(
                "cc-email {} ({})",
                env!("CARGO_PKG_VERSION"),
                env!("CARGO_PKG_DESCRIPTION")
            );
        }
    }
}

fn load_config(path: &std::path::Path) -> Config {
    match Config::load(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: failed to load config: {}", e);
            std::process::exit(1);
        }
    }
}
