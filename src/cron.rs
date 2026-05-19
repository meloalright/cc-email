use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{CcEmailError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SessionMode {
    Reuse,
    NewPerRun,
}

impl std::fmt::Display for SessionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionMode::Reuse => write!(f, "reuse"),
            SessionMode::NewPerRun => write!(f, "new-per-run"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub cron_expr: String,
    pub prompt: Option<String>,
    pub exec: Option<String>,
    pub description: String,
    pub enabled: bool,
    pub mute: bool,
    pub session_mode: SessionMode,
    pub timeout_mins: Option<u64>,
    pub reply_to: String,
    pub created_at: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

pub struct CronScheduler {
    jobs: HashMap<String, CronJob>,
    store_path: PathBuf,
    counter: u64,
}

impl CronScheduler {
    pub fn new(data_dir: &Path) -> Self {
        let store_path = data_dir.join("crons.json");
        Self {
            jobs: HashMap::new(),
            store_path,
            counter: 0,
        }
    }

    pub fn load(data_dir: &Path) -> Result<Self> {
        let store_path = data_dir.join("crons.json");
        if !store_path.exists() {
            return Ok(Self {
                jobs: HashMap::new(),
                store_path,
                counter: 0,
            });
        }
        let content = std::fs::read_to_string(&store_path)
            .map_err(|e| CcEmailError::Config(format!("failed to load crons: {}", e)))?;
        let jobs: HashMap<String, CronJob> = serde_json::from_str(&content)
            .map_err(|e| CcEmailError::Config(format!("failed to parse crons: {}", e)))?;
        let counter = jobs
            .keys()
            .filter_map(|k| k.strip_prefix("cron-").and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        Ok(Self {
            jobs,
            store_path,
            counter,
        })
    }

    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.jobs)
            .map_err(|e| CcEmailError::Config(format!("failed to serialize crons: {}", e)))?;
        if let Some(parent) = self.store_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&self.store_path, json)
            .map_err(|e| CcEmailError::Config(format!("failed to write crons: {}", e)))?;
        Ok(())
    }

    pub fn add_job(
        &mut self,
        cron_expr: &str,
        prompt: Option<&str>,
        exec: Option<&str>,
        description: &str,
        reply_to: &str,
    ) -> Result<String> {
        if prompt.is_none() && exec.is_none() {
            return Err(CcEmailError::Config(
                "cron job must have either prompt or exec".into(),
            ));
        }

        self.counter += 1;
        let id = format!("cron-{}", self.counter);

        let job = CronJob {
            id: id.clone(),
            cron_expr: cron_expr.to_string(),
            prompt: prompt.map(|s| s.to_string()),
            exec: exec.map(|s| s.to_string()),
            description: description.to_string(),
            enabled: true,
            mute: false,
            session_mode: SessionMode::Reuse,
            timeout_mins: Some(30),
            reply_to: reply_to.to_string(),
            created_at: Utc::now(),
            last_run: None,
            last_error: None,
        };

        self.jobs.insert(id.clone(), job);
        self.save()?;
        Ok(id)
    }

    pub fn remove_job(&mut self, id: &str) -> Result<bool> {
        let removed = self.jobs.remove(id).is_some();
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    pub fn toggle_job(&mut self, id: &str) -> Result<bool> {
        if let Some(job) = self.jobs.get_mut(id) {
            job.enabled = !job.enabled;
            let enabled = job.enabled;
            self.save()?;
            Ok(enabled)
        } else {
            Err(CcEmailError::Config(format!("cron job '{}' not found", id)))
        }
    }

    pub fn mute_job(&mut self, id: &str) -> Result<bool> {
        if let Some(job) = self.jobs.get_mut(id) {
            job.mute = !job.mute;
            let muted = job.mute;
            self.save()?;
            Ok(muted)
        } else {
            Err(CcEmailError::Config(format!("cron job '{}' not found", id)))
        }
    }

    pub fn list_jobs(&self) -> Vec<&CronJob> {
        let mut jobs: Vec<_> = self.jobs.values().collect();
        jobs.sort_by_key(|j| &j.id);
        jobs
    }

    pub fn get_job(&self, id: &str) -> Option<&CronJob> {
        self.jobs.get(id)
    }

    pub fn record_run(&mut self, id: &str, error: Option<&str>) {
        if let Some(job) = self.jobs.get_mut(id) {
            job.last_run = Some(Utc::now());
            job.last_error = error.map(|s| s.to_string());
            self.save().ok();
        }
    }

    pub fn job_count(&self) -> usize {
        self.jobs.len()
    }

    pub fn should_run(&self, id: &str, now: &DateTime<Utc>) -> bool {
        if let Some(job) = self.jobs.get(id) {
            if !job.enabled {
                return false;
            }
            if let Some(ref last_run) = job.last_run {
                let elapsed = (*now - *last_run).num_seconds();
                elapsed >= 60
            } else {
                true
            }
        } else {
            false
        }
    }

    pub fn enabled_jobs(&self) -> Vec<&CronJob> {
        self.jobs.values().filter(|j| j.enabled).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_dir() -> PathBuf {
        let p = PathBuf::from(format!("/tmp/cc-email-cron-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn test_add_job() {
        let dir = tmp_dir();
        let mut sched = CronScheduler::new(&dir);
        let id = sched
            .add_job(
                "0 9 * * 1-5",
                Some("run tests"),
                None,
                "Weekday tests",
                "user@example.com",
            )
            .unwrap();
        assert_eq!(id, "cron-1");
        assert_eq!(sched.job_count(), 1);

        let job = sched.get_job(&id).unwrap();
        assert_eq!(job.cron_expr, "0 9 * * 1-5");
        assert!(job.enabled);
        assert!(!job.mute);
    }

    #[test]
    fn test_remove_job() {
        let dir = tmp_dir();
        let mut sched = CronScheduler::new(&dir);
        let id = sched
            .add_job("* * * * *", Some("test"), None, "test", "u@e.com")
            .unwrap();
        assert!(sched.remove_job(&id).unwrap());
        assert_eq!(sched.job_count(), 0);
        assert!(!sched.remove_job(&id).unwrap());
    }

    #[test]
    fn test_toggle_job() {
        let dir = tmp_dir();
        let mut sched = CronScheduler::new(&dir);
        let id = sched
            .add_job("* * * * *", Some("test"), None, "test", "u@e.com")
            .unwrap();
        assert!(sched.get_job(&id).unwrap().enabled);
        sched.toggle_job(&id).unwrap();
        assert!(!sched.get_job(&id).unwrap().enabled);
        sched.toggle_job(&id).unwrap();
        assert!(sched.get_job(&id).unwrap().enabled);
    }

    #[test]
    fn test_save_load() {
        let dir = tmp_dir();
        let mut sched = CronScheduler::new(&dir);
        sched
            .add_job("0 6 * * *", Some("morning"), None, "Morning", "u@e.com")
            .unwrap();
        sched
            .add_job("0 18 * * *", None, Some("df -h"), "Disk check", "u@e.com")
            .unwrap();

        let loaded = CronScheduler::load(&dir).unwrap();
        assert_eq!(loaded.job_count(), 2);
    }

    #[test]
    fn test_list_jobs() {
        let dir = tmp_dir();
        let mut sched = CronScheduler::new(&dir);
        sched
            .add_job("0 9 * * *", Some("a"), None, "A", "u@e.com")
            .unwrap();
        sched
            .add_job("0 18 * * *", Some("b"), None, "B", "u@e.com")
            .unwrap();
        let list = sched.list_jobs();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_record_run() {
        let dir = tmp_dir();
        let mut sched = CronScheduler::new(&dir);
        let id = sched
            .add_job("* * * * *", Some("test"), None, "test", "u@e.com")
            .unwrap();
        assert!(sched.get_job(&id).unwrap().last_run.is_none());
        sched.record_run(&id, None);
        assert!(sched.get_job(&id).unwrap().last_run.is_some());
    }

    #[test]
    fn test_must_have_prompt_or_exec() {
        let dir = tmp_dir();
        let mut sched = CronScheduler::new(&dir);
        let result = sched.add_job("* * * * *", None, None, "empty", "u@e.com");
        assert!(result.is_err());
    }
}
