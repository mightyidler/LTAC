//! A process-wide terminal gate. Confirmed logoff/shutdown must win over ON,
//! including a manual ON that temporarily runs without the automatic watcher.
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

static ENDING: AtomicBool = AtomicBool::new(false);
static CHANGED: Notify = Notify::const_new();

pub fn is_ending() -> bool {
    ENDING.load(Ordering::Acquire)
}

pub fn begin_ending() {
    ENDING.store(true, Ordering::Release);
    CHANGED.notify_waiters();
}

pub async fn ending() {
    loop {
        // Register before checking the flag, avoiding a lost notification.
        let notified = CHANGED.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if is_ending() {
            return;
        }
        notified.await;
    }
}
