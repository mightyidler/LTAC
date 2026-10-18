use anyhow::Result;
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct Journal {
    file: Arc<Mutex<File>>,
    observer: Option<Arc<Observer>>,
    best_effort: bool,
}
type Observer = dyn Fn(&str, &Value) + Send + Sync;
impl Journal {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            file: Arc::new(Mutex::new(
                OpenOptions::new().create(true).append(true).open(path)?,
            )),
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
        let mut file = self
            .file
            .lock()
            .map_err(|_| anyhow::anyhow!("Journal lock poisoned"))?;
        serde_json::to_writer(
            &mut *file,
            &json!({"timestamp_ms": timestamp_ms, "event": event, "details": details}),
        )?;
        writeln!(file)?;
        file.flush()?;
        drop(file);
        Ok(())
    }
}
