use crate::config::InboxConfig;
use crate::error::{CcEmailError, Result};

pub type FetchFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Vec<(String, Vec<u8>)>>> + Send + 'a>,
>;

pub struct ImapPoller {
    config: InboxConfig,
}

pub trait InboxAdapter: Send + Sync {
    fn fetch_unseen(&self) -> FetchFuture<'_>;
}

impl ImapPoller {
    pub fn new(config: InboxConfig) -> Self {
        Self { config }
    }
}

impl InboxAdapter for ImapPoller {
    fn fetch_unseen(&self) -> FetchFuture<'_> {
        let config = self.config.clone();
        Box::pin(async move {
            tracing::debug!("starting imap fetch");
            let (tx, rx) = tokio::sync::oneshot::channel();

            std::thread::spawn(move || {
                let result = imap_fetch_sync(config);
                let _ = tx.send(result);
            });

            let result = rx
                .await
                .map_err(|_| CcEmailError::Imap("imap worker thread died".to_string()))?;
            tracing::debug!("imap fetch complete");
            result
        })
    }
}

fn imap_fetch_sync(config: InboxConfig) -> Result<Vec<(String, Vec<u8>)>> {
    let password = config.resolve_password()?;
    let addr = format!("{}:{}", config.host, config.port);

    let tcp = std::net::TcpStream::connect(&addr)
        .map_err(|e| CcEmailError::Imap(format!("tcp connect failed: {}", e)))?;

    let connector = native_tls::TlsConnector::new()
        .map_err(|e| CcEmailError::Imap(format!("tls connector failed: {}", e)))?;
    let tls_stream = connector
        .connect(&config.host, tcp)
        .map_err(|e| CcEmailError::Imap(format!("tls connect failed: {}", e)))?;

    let client = imap::Client::new(tls_stream);
    let mut session = client
        .login(&config.username, &password)
        .map_err(|e| CcEmailError::Imap(format!("login failed: {}", e.0)))?;

    session
        .select(&config.folder)
        .map_err(|e| CcEmailError::Imap(format!("select folder failed: {}", e)))?;

    let query = if !config.search_from.is_empty() {
        if config.search_from.len() == 1 {
            format!("UNSEEN FROM \"{}\"", config.search_from[0])
        } else {
            // IMAP prefix OR: "UNSEEN OR FROM a OR FROM b FROM c"
            let mut q = String::from("UNSEEN ");
            for _ in 0..config.search_from.len() - 1 {
                q.push_str("OR ");
            }
            for addr in &config.search_from {
                q.push_str(&format!("FROM \"{}\" ", addr));
            }
            q.trim().to_string()
        }
    } else if let Some(ref to_addr) = config.search_to {
        format!("UNSEEN TO \"{}\"", to_addr)
    } else {
        "UNSEEN".to_string()
    };

    let uids = session
        .uid_search(&query)
        .map_err(|e| CcEmailError::Imap(format!("search failed: {}", e)))?;

    if uids.is_empty() {
        session.logout().ok();
        return Ok(Vec::new());
    }

    let uid_list: Vec<String> = uids.iter().map(|u| u.to_string()).collect();
    let uid_set = uid_list.join(",");

    tracing::info!(count = uid_list.len(), "found unseen emails");

    let mut messages = Vec::new();

    let fetched = session
        .uid_fetch(&uid_set, "(UID RFC822)")
        .map_err(|e| CcEmailError::Imap(format!("fetch failed: {}", e)))?;

    for fetch in fetched.iter() {
        let uid = fetch.uid.unwrap_or(0).to_string();
        if let Some(body) = fetch.body() {
            messages.push((uid, body.to_vec()));
        }
    }

    if !uid_set.is_empty() {
        session
            .uid_store(&uid_set, "+FLAGS (\\Seen)")
            .map_err(|e| CcEmailError::Imap(format!("store flags failed: {}", e)))?;
    }

    session.logout().ok();
    Ok(messages)
}
