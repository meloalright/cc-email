use std::path::{Path, PathBuf};
use std::time::SystemTime;

const SENSITIVE_PATTERNS: &[&str] = &[
    ".env",
    "credentials",
    ".key",
    ".pem",
    "id_rsa",
    "id_ed25519",
    ".secret",
    ".p12",
    ".pfx",
    ".keystore",
];

const DEFAULT_ALLOWED_EXTENSIONS: &[&str] = &[
    "txt", "md", "rs", "py", "ts", "json", "toml", "yaml", "yml", "csv", "diff", "patch", "png",
    "jpg", "jpeg", "svg", "pdf", "html", "css", "sql", "go", "java", "c", "h", "cpp", "hpp", "rb",
    "ex", "exs", "xml",
];

#[derive(Debug, Clone)]
pub struct AttachmentFile {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct GeneratedFile {
    pub path: PathBuf,
    pub tool_name: String,
}

pub struct AttachmentCollector {
    generated_files: Vec<GeneratedFile>,
    work_dir: PathBuf,
    max_file_size: u64,
    max_total_size: u64,
    allowed_extensions: Vec<String>,
    enabled: bool,
}

impl AttachmentCollector {
    pub fn new(work_dir: PathBuf, config: &AttachmentConfig) -> Self {
        let allowed_extensions = if config.allowed_extensions.is_empty() {
            DEFAULT_ALLOWED_EXTENSIONS
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            config.allowed_extensions.clone()
        };

        Self {
            generated_files: Vec::new(),
            work_dir,
            max_file_size: config.max_file_size_bytes,
            max_total_size: config.max_total_size_bytes,
            allowed_extensions,
            enabled: config.enabled,
        }
    }

    pub fn track_write(&mut self, input: &serde_json::Value) {
        if !self.enabled {
            return;
        }
        if let Some(path) = input.get("file_path").and_then(|v| v.as_str()) {
            self.generated_files.push(GeneratedFile {
                path: PathBuf::from(path),
                tool_name: "Write".to_string(),
            });
        }
    }

    pub fn track_tool_use(&mut self, tool_name: &str, input: &serde_json::Value) {
        if !self.enabled {
            return;
        }
        match tool_name {
            "Write" | "write" => self.track_write(input),
            "NotebookEdit" | "notebook_edit" => {
                if let Some(path) = input.get("notebook_path").and_then(|v| v.as_str()) {
                    self.generated_files.push(GeneratedFile {
                        path: PathBuf::from(path),
                        tool_name: "NotebookEdit".to_string(),
                    });
                }
            }
            _ => {}
        }
    }

    pub fn collect(&self) -> Vec<AttachmentFile> {
        if !self.enabled || self.generated_files.is_empty() {
            return Vec::new();
        }

        let mut attachments = Vec::new();
        let mut total_size: u64 = 0;

        for gen in &self.generated_files {
            let path = if gen.path.is_absolute() {
                gen.path.clone()
            } else {
                self.work_dir.join(&gen.path)
            };

            if !path.exists() {
                tracing::warn!(path = %path.display(), "generated file not found on disk, skipping");
                continue;
            }

            if !is_within_dir(&path, &self.work_dir) {
                tracing::debug!(path = %path.display(), "skipping file outside work dir");
                continue;
            }

            if is_sensitive(&path) {
                tracing::debug!(path = %path.display(), "skipping sensitive file");
                continue;
            }

            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !self.allowed_extensions.iter().any(|a| a == &ext) {
                tracing::debug!(path = %path.display(), ext = %ext, "skipping disallowed extension");
                continue;
            }

            let metadata = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            if metadata.len() > self.max_file_size {
                tracing::debug!(path = %path.display(), size = metadata.len(), "skipping oversized file");
                continue;
            }

            if total_size + metadata.len() > self.max_total_size {
                tracing::debug!("total attachment size limit reached, stopping");
                break;
            }

            let data = match std::fs::read(&path) {
                Ok(d) => d,
                Err(_) => continue,
            };

            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string();

            let content_type = mime_from_ext(&ext);

            total_size += data.len() as u64;
            attachments.push(AttachmentFile {
                filename,
                content_type,
                data,
            });
        }

        attachments
    }
}

pub fn scan_new_files(
    work_dir: &Path,
    since: SystemTime,
    config: &AttachmentConfig,
) -> Vec<AttachmentFile> {
    if !config.enabled {
        return Vec::new();
    }

    let allowed_extensions: Vec<String> = if config.allowed_extensions.is_empty() {
        DEFAULT_ALLOWED_EXTENSIONS
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        config.allowed_extensions.clone()
    };

    let mut candidates = Vec::new();
    scan_dir_recursive(work_dir, &mut candidates);

    let mut attachments = Vec::new();
    let mut total_size: u64 = 0;

    for path in candidates {
        let metadata = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };

        if !metadata.is_file() {
            continue;
        }

        let modified = match metadata.modified() {
            Ok(t) => t,
            Err(_) => continue,
        };

        if modified < since {
            continue;
        }

        if !is_within_dir(&path, work_dir) {
            continue;
        }

        if is_sensitive(&path) {
            tracing::debug!(path = %path.display(), "skipping sensitive file");
            continue;
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !allowed_extensions.iter().any(|a| a == &ext) {
            continue;
        }

        if metadata.len() > config.max_file_size_bytes {
            tracing::debug!(path = %path.display(), size = metadata.len(), "skipping oversized file");
            continue;
        }

        if total_size + metadata.len() > config.max_total_size_bytes {
            tracing::debug!("total attachment size limit reached");
            break;
        }

        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        let content_type = mime_from_ext(&ext);
        total_size += data.len() as u64;

        tracing::info!(path = %path.display(), "attaching new file");
        attachments.push(AttachmentFile {
            filename,
            content_type,
            data,
        });
    }

    attachments
}

const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".claude",
    "__pycache__",
    ".venv",
    "venv",
    "logs",
    "data",
];

fn scan_dir_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            scan_dir_recursive(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn is_within_dir(path: &Path, dir: &Path) -> bool {
    match (path.canonicalize(), dir.canonicalize()) {
        (Ok(p), Ok(d)) => p.starts_with(d),
        _ => false,
    }
}

fn is_sensitive(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    SENSITIVE_PATTERNS.iter().any(|p| name.contains(p))
}

fn mime_from_ext(ext: &str) -> String {
    match ext {
        "txt" | "log" | "csv" => "text/plain".to_string(),
        "md" => "text/markdown".to_string(),
        "html" => "text/html".to_string(),
        "css" => "text/css".to_string(),
        "json" => "application/json".to_string(),
        "xml" => "application/xml".to_string(),
        "pdf" => "application/pdf".to_string(),
        "png" => "image/png".to_string(),
        "jpg" | "jpeg" => "image/jpeg".to_string(),
        "svg" => "image/svg+xml".to_string(),
        "rs" | "py" | "js" | "ts" | "go" | "java" | "c" | "h" | "cpp" | "hpp" | "rb" | "ex"
        | "exs" | "sh" | "sql" | "toml" | "yaml" | "yml" | "diff" | "patch" => {
            "text/plain".to_string()
        }
        _ => "application/octet-stream".to_string(),
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AttachmentConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_max_file_size")]
    pub max_file_size_bytes: u64,
    #[serde(default = "default_max_total_size")]
    pub max_total_size_bytes: u64,
    #[serde(default)]
    pub allowed_extensions: Vec<String>,
}

impl Default for AttachmentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_file_size_bytes: default_max_file_size(),
            max_total_size_bytes: default_max_total_size(),
            allowed_extensions: Vec::new(),
        }
    }
}

fn default_enabled() -> bool {
    true
}

fn default_max_file_size() -> u64 {
    10_000_000
}

fn default_max_total_size() -> u64 {
    25_000_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_config() -> AttachmentConfig {
        AttachmentConfig {
            enabled: true,
            max_file_size_bytes: 1_000_000,
            max_total_size_bytes: 5_000_000,
            allowed_extensions: vec![],
        }
    }

    #[test]
    fn test_collect_written_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("output.txt");
        fs::write(&file_path, "hello world").unwrap();

        let mut collector = AttachmentCollector::new(dir.path().to_path_buf(), &test_config());
        collector.track_tool_use(
            "Write",
            &serde_json::json!({"file_path": file_path.to_str().unwrap()}),
        );

        let attachments = collector.collect();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].filename, "output.txt");
        assert_eq!(attachments[0].data, b"hello world");
        assert_eq!(attachments[0].content_type, "text/plain");
    }

    #[test]
    fn test_skip_sensitive_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join(".env");
        fs::write(&file_path, "SECRET=xyz").unwrap();

        let mut collector = AttachmentCollector::new(dir.path().to_path_buf(), &test_config());
        collector.track_tool_use(
            "Write",
            &serde_json::json!({"file_path": file_path.to_str().unwrap()}),
        );

        let attachments = collector.collect();
        assert_eq!(attachments.len(), 0);
    }

    #[test]
    fn test_skip_disallowed_extension() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("malware.exe");
        fs::write(&file_path, "bad stuff").unwrap();

        let mut collector = AttachmentCollector::new(dir.path().to_path_buf(), &test_config());
        collector.track_tool_use(
            "Write",
            &serde_json::json!({"file_path": file_path.to_str().unwrap()}),
        );

        let attachments = collector.collect();
        assert_eq!(attachments.len(), 0);
    }

    #[test]
    fn test_skip_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("big.txt");
        let big_data = vec![b'x'; 2_000_000];
        fs::write(&file_path, &big_data).unwrap();

        let mut collector = AttachmentCollector::new(dir.path().to_path_buf(), &test_config());
        collector.track_tool_use(
            "Write",
            &serde_json::json!({"file_path": file_path.to_str().unwrap()}),
        );

        let attachments = collector.collect();
        assert_eq!(attachments.len(), 0);
    }

    #[test]
    fn test_skip_deleted_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("deleted.txt");

        let mut collector = AttachmentCollector::new(dir.path().to_path_buf(), &test_config());
        collector.track_tool_use(
            "Write",
            &serde_json::json!({"file_path": file_path.to_str().unwrap()}),
        );

        let attachments = collector.collect();
        assert_eq!(attachments.len(), 0);
    }

    #[test]
    fn test_disabled_collector() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("output.txt");
        fs::write(&file_path, "hello").unwrap();

        let config = AttachmentConfig {
            enabled: false,
            ..test_config()
        };
        let mut collector = AttachmentCollector::new(dir.path().to_path_buf(), &config);
        collector.track_tool_use(
            "Write",
            &serde_json::json!({"file_path": file_path.to_str().unwrap()}),
        );

        let attachments = collector.collect();
        assert_eq!(attachments.len(), 0);
    }

    #[test]
    fn test_mime_types() {
        assert_eq!(mime_from_ext("png"), "image/png");
        assert_eq!(mime_from_ext("rs"), "text/plain");
        assert_eq!(mime_from_ext("json"), "application/json");
        assert_eq!(mime_from_ext("pdf"), "application/pdf");
        assert_eq!(mime_from_ext("bin"), "application/octet-stream");
    }
}
