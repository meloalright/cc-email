use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "cc-email",
    version,
    about = "Local-first email listener for coding agents"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start the email listener daemon
    Listen {
        /// Path to config file
        #[arg(short, long, default_value = "cc-email.toml")]
        config: PathBuf,
    },

    /// Send a one-off email
    Send {
        /// Recipient email address
        #[arg(long)]
        to: String,
        /// Email subject
        #[arg(long, default_value = "cc-email")]
        subject: String,
        /// Email body
        #[arg(long)]
        body: String,
        /// Config file for SMTP settings
        #[arg(short, long, default_value = "cc-email.toml")]
        config: PathBuf,
    },

    /// Session management
    Sessions {
        #[command(subcommand)]
        action: SessionAction,
    },

    /// Cron job management
    Cron {
        #[command(subcommand)]
        action: CronAction,
    },

    /// Run health diagnostics
    Doctor {
        /// Config file
        #[arg(short, long, default_value = "cc-email.toml")]
        config: PathBuf,
    },

    /// Show version info
    Version,
}

#[derive(Subcommand)]
pub enum SessionAction {
    /// List all sessions
    List {
        /// Config file
        #[arg(short, long, default_value = "cc-email.toml")]
        config: PathBuf,
    },
    /// Delete a session
    Delete {
        /// Session ID
        id: String,
        /// Config file
        #[arg(short, long, default_value = "cc-email.toml")]
        config: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum CronAction {
    /// List cron jobs
    List {
        /// Config file
        #[arg(short, long, default_value = "cc-email.toml")]
        config: PathBuf,
    },
    /// Add a cron job
    Add {
        /// Cron schedule expression
        #[arg(long)]
        schedule: String,
        /// Agent prompt
        #[arg(long)]
        prompt: Option<String>,
        /// Shell command
        #[arg(long)]
        exec: Option<String>,
        /// Description
        #[arg(long, default_value = "cron job")]
        desc: String,
        /// Reply-to address
        #[arg(long)]
        reply_to: String,
        /// Config file
        #[arg(short, long, default_value = "cc-email.toml")]
        config: PathBuf,
    },
    /// Delete a cron job
    Del {
        /// Job ID
        id: String,
        /// Config file
        #[arg(short, long, default_value = "cc-email.toml")]
        config: PathBuf,
    },
}
