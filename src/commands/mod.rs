pub mod atc;
pub mod atc_setup;
pub mod devices;
pub mod extern_cmd;
pub mod ingest;
pub mod iq_info;
pub mod scan;
pub mod selftest;
pub mod station;
pub mod survey;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Where a long-running command (`scan`, `survey`) reports progress, and how
/// it is asked to stop early. The CLI prints to stdout; other front ends
/// collect the lines and set the flag from their own controls.
#[derive(Clone)]
pub struct Reporter {
    sink: Arc<dyn Fn(&str) + Send + Sync>,
    cancel: Arc<AtomicBool>,
}

impl Reporter {
    /// Progress to stdout, never cancelled.
    pub fn stdout() -> Self {
        Self::new(|line| println!("{line}"), Arc::new(AtomicBool::new(false)))
    }

    /// Progress to `sink`; the command winds down once `cancel` is set and
    /// still returns what it has measured so far.
    pub fn new(sink: impl Fn(&str) + Send + Sync + 'static, cancel: Arc<AtomicBool>) -> Self {
        Self { sink: Arc::new(sink), cancel }
    }

    pub fn line(&self, text: &str) {
        (self.sink)(text)
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn cancel_flag(&self) -> &Arc<AtomicBool> {
        &self.cancel
    }
}

/// `println!` for a [`Reporter`].
macro_rules! say {
    ($rep:expr, $($arg:tt)*) => {
        $rep.line(&format!($($arg)*))
    };
}
pub(crate) use say;
