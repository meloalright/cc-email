use thiserror::Error;

#[derive(Error, Debug)]
pub enum CcEmailError {
    #[error("config error: {0}")]
    Config(String),

    #[error("imap error: {0}")]
    Imap(String),

    #[error("smtp error: {0}")]
    Smtp(String),

    #[error("mail parse error: {0}")]
    MailParse(String),

    #[error("agent error: {0}")]
    Agent(String),

    #[error("security violation: {0}")]
    Security(String),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, CcEmailError>;
