// Watchdog for long-running decodes.
//
// A thumbnail that never finishes cannot be interrupted - Rust has no way to
// stop a thread stuck in a loop. What we CAN do is name the file: every
// decode registers itself here, and a background thread writes any that has
// been running for too long to a log file in the cache directory. The next
// time "no more thumbnails" happens, the culprit is in that log.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A decode running longer than this is reported.
const STUCK_AFTER: Duration = Duration::from_secs(30);
/// The log is cut in half beyond this size.
const LOG_MAX: u64 = 1 << 20;

struct State {
    running: HashMap<u64, (String, Instant, bool)>,
    log: Option<PathBuf>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| {
        std::thread::Builder::new()
            .name("thumb-watchdog".into())
            .spawn(|| loop {
                std::thread::sleep(Duration::from_secs(5));
                let mut late = Vec::new();
                if let Ok(mut st) = state().lock() {
                    for (path, since, told) in st.running.values_mut() {
                        if !*told && since.elapsed() > STUCK_AFTER {
                            *told = true;
                            late.push(format!(
                                "decode still running after {}s: {path}",
                                since.elapsed().as_secs()
                            ));
                        }
                    }
                }
                for line in late {
                    log(&line);
                }
            })
            .ok();
        Mutex::new(State { running: HashMap::new(), log: None })
    })
}

/// Where the log goes; set once by the application at start-up.
pub fn set_log_file(path: PathBuf) {
    if let Ok(mut st) = state().lock() {
        st.log = Some(path);
    }
}

/// Append one timestamped line to the log, if one is configured.
pub fn log(line: &str) {
    let Some(path) = state().lock().ok().and_then(|st| st.log.clone()) else { return };
    // Keep it small: past the limit, drop the older half.
    if std::fs::metadata(&path).map(|m| m.len() > LOG_MAX).unwrap_or(false) {
        if let Ok(old) = std::fs::read(&path) {
            let _ = std::fs::write(&path, &old[old.len() / 2..]);
        }
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{secs}] {line}");
    }
}

/// Registration of one running decode; dropping it unregisters.
pub struct Watch(u64);

impl Watch {
    pub fn start(path: &str) -> Watch {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut st) = state().lock() {
            st.running.insert(id, (path.to_string(), Instant::now(), false));
        }
        Watch(id)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Ok(mut st) = state().lock() {
            st.running.remove(&self.0);
        }
    }
}
