use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RelayPeer {
    pub name: String,
    pub address: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RelayConfig {
    #[serde(default)]
    pub peers: Vec<RelayPeer>,
}

pub struct RelayManager {
    peers: HashMap<String, RelayPeer>,
}

impl RelayManager {
    pub fn new(config: &RelayConfig) -> Self {
        let peers = config
            .peers
            .iter()
            .map(|p| (p.name.clone(), p.clone()))
            .collect();
        Self { peers }
    }

    pub fn list_peers(&self) -> Vec<&RelayPeer> {
        self.peers.values().collect()
    }

    pub fn get_peer(&self, name: &str) -> Option<&RelayPeer> {
        self.peers.get(name)
    }

    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_relay() {
        let mgr = RelayManager::new(&RelayConfig::default());
        assert_eq!(mgr.peer_count(), 0);
        assert!(mgr.list_peers().is_empty());
    }

    #[test]
    fn test_relay_with_peers() {
        let config = RelayConfig {
            peers: vec![
                RelayPeer {
                    name: "reviewer".to_string(),
                    address: "reviewer@example.com".to_string(),
                },
                RelayPeer {
                    name: "tester".to_string(),
                    address: "tester@example.com".to_string(),
                },
            ],
        };
        let mgr = RelayManager::new(&config);
        assert_eq!(mgr.peer_count(), 2);
        assert!(mgr.get_peer("reviewer").is_some());
        assert_eq!(
            mgr.get_peer("tester").unwrap().address,
            "tester@example.com"
        );
        assert!(mgr.get_peer("unknown").is_none());
    }
}
