use anyhow::Result;
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct Journal {
    file: Arc<Mutex<JournalFile>>,
    observer: Option<Arc<Observer>>,
    best_effort: bool,
}
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;
struct JournalFile {
    file: File,
    path: PathBuf,
    bytes: u64,
    limit: u64,
}
type Observer = dyn Fn(&str, &Value) + Send + Sync;
impl Journal {
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let bytes = file.metadata()?.len();
        Ok(Self {
            file: Arc::new(Mutex::new(JournalFile {
                file,
                path: path.to_owned(),
                bytes,
                limit: MAX_LOG_BYTES,
            })),
            observer: None,
            best_effort: false,
        })
    }
    pub fn with_observer(
        mut self,
        observer: impl Fn(&str, &Value) + Send + Sync + 'static,
    ) -> Self {
        self.observer = Some(Arc::new(observer));
        self
    }
    /// A service must keep controlling power if a full disk prevents logging.
    /// Opening the log still fails explicitly when the directory is unusable.
    pub fn best_effort(mut self) -> Self {
        self.best_effort = true;
        self
    }
    // Only structured operational evidence goes here. Never raw protocol frames or keys.
    pub fn record(&mut self, event: &str, details: Value) -> Result<()> {
        let result = self.write(event, &details);
        if (self.best_effort || result.is_ok())
            && let Some(observer) = &self.observer
        {
            observer(event, &details);
        }
        if self.best_effort { Ok(()) } else { result }
    }

    fn write(&self, event: &str, details: &Value) -> Result<()> {
        let timestamp_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let mut entry = serde_json::to_vec(
            &json!({"timestamp_ms": timestamp_ms, "event": event, "details": details}),
        )?;
        entry.push(b'\n');
        let mut log = self
            .file
            .lock()
            .map_err(|_| anyhow::anyhow!("Journal lock poisoned"))?;
        anyhow::ensure!(
            entry.len() as u64 <= log.limit,
            "Journal entry exceeds log size limit"
        );
        if log.bytes.saturating_add(entry.len() as u64) > log.limit {
            // Clones share this lock and handle. Keep one bounded previous log,
            // and prepare the replacement before moving the current evidence.
            let parent = log
                .path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let mut next = tempfile::NamedTempFile::new_in(parent)?;
            next.write_all(&entry)?;
            next.as_file().sync_all()?;
            let previous = log.path.with_extension("previous.jsonl");
            std::fs::rename(&log.path, &previous)?;
            match next.persist(&log.path) {
                Ok(file) => log.file = file,
                Err(error) => {
                    let _ = std::fs::rename(&previous, &log.path);
                    return Err(error.into());
                }
            }
            log.bytes = entry.len() as u64;
        } else {
            log.file.write_all(&entry)?;
            log.bytes += entry.len() as u64;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_writers_rotate_bounded_complete_json_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut first = Journal::open(&path).unwrap();
        first.file.lock().unwrap().limit = 512;
        let mut second = first.clone();
        for sequence in 0..100 {
            let writer = if sequence % 2 == 0 {
                &mut first
            } else {
                &mut second
            };
            writer.record("test", json!({"sequence":sequence})).unwrap();
        }
        for log in [&path, &path.with_extension("previous.jsonl")] {
            assert!(std::fs::metadata(log).unwrap().len() <= 512);
            for line in std::fs::read_to_string(log).unwrap().lines() {
                serde_json::from_str::<Value>(line).unwrap();
            }
        }
        assert!(std::fs::read_to_string(path).unwrap().contains("99"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn failed_rotation_preserves_log_and_best_effort_observer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut journal = Journal::open(&path).unwrap();
        journal.record("before", json!({})).unwrap();
        journal.file.lock().unwrap().limit = 128;
        std::fs::create_dir(path.with_extension("previous.jsonl")).unwrap();
        assert!(
            journal
                .record("failed", json!({"value":"x".repeat(40)}))
                .is_err()
        );
        assert!(std::fs::read_to_string(path).unwrap().contains("before"));
        let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = observed.clone();
        journal = journal.best_effort().with_observer(move |_, _| {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        journal
            .record("failed", json!({"value":"x".repeat(40)}))
            .unwrap();
        assert!(observed.load(std::sync::atomic::Ordering::Relaxed));
    }
}
