use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceConfig {
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub base_dir: Option<String>,
    #[serde(default)]
    pub routes: Vec<WorkspaceRoute>,
    #[serde(default)]
    pub idle_timeout_mins: Option<u64>,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            base_dir: None,
            routes: vec![],
            idle_timeout_mins: None,
        }
    }
}

fn default_mode() -> String {
    "single".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceRoute {
    #[serde(default)]
    pub match_sender: Option<String>,
    #[serde(default)]
    pub match_subject_prefix: Option<String>,
    pub work_dir: String,
}

pub struct WorkspaceRouter {
    config: WorkspaceConfig,
}

impl WorkspaceRouter {
    pub fn new(config: WorkspaceConfig) -> Self {
        Self { config }
    }

    pub fn resolve(&self, sender: &str, subject: &str) -> Option<PathBuf> {
        if self.config.mode == "single" {
            return None;
        }

        for route in &self.config.routes {
            if let Some(ref match_sender) = route.match_sender {
                if sender == match_sender {
                    return Some(PathBuf::from(&route.work_dir));
                }
            }
            if let Some(ref prefix) = route.match_subject_prefix {
                if subject.starts_with(prefix) {
                    return Some(PathBuf::from(&route.work_dir));
                }
            }
        }

        self.config.base_dir.as_ref().map(PathBuf::from)
    }

    pub fn is_multi(&self) -> bool {
        self.config.mode == "multi"
    }

    pub fn routes(&self) -> &[WorkspaceRoute] {
        &self.config.routes
    }

    pub fn idle_timeout_mins(&self) -> Option<u64> {
        self.config.idle_timeout_mins
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_mode() {
        let router = WorkspaceRouter::new(WorkspaceConfig::default());
        assert!(!router.is_multi());
        assert!(router.resolve("user@example.com", "test").is_none());
    }

    #[test]
    fn test_multi_mode_sender_match() {
        let config = WorkspaceConfig {
            mode: "multi".to_string(),
            base_dir: Some("/projects".to_string()),
            routes: vec![WorkspaceRoute {
                match_sender: Some("frontend@example.com".to_string()),
                match_subject_prefix: None,
                work_dir: "/projects/frontend".to_string(),
            }],
            idle_timeout_mins: None,
        };
        let router = WorkspaceRouter::new(config);
        assert!(router.is_multi());
        assert_eq!(
            router.resolve("frontend@example.com", "anything"),
            Some(PathBuf::from("/projects/frontend"))
        );
        assert_eq!(
            router.resolve("other@example.com", "anything"),
            Some(PathBuf::from("/projects"))
        );
    }

    #[test]
    fn test_subject_prefix_match() {
        let config = WorkspaceConfig {
            mode: "multi".to_string(),
            base_dir: None,
            routes: vec![WorkspaceRoute {
                match_sender: None,
                match_subject_prefix: Some("[api]".to_string()),
                work_dir: "/projects/api".to_string(),
            }],
            idle_timeout_mins: None,
        };
        let router = WorkspaceRouter::new(config);
        assert_eq!(
            router.resolve("anyone@example.com", "[api] fix endpoint"),
            Some(PathBuf::from("/projects/api"))
        );
        assert!(router
            .resolve("anyone@example.com", "fix frontend")
            .is_none());
    }
}
