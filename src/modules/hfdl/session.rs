use async_trait::async_trait;
use chrono::{DateTime, Local};
use log::*;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdout};
use tokio::select;
use tokio::time::{sleep_until, Duration, Instant};

use crate::modules::session::{EndSessionReason, Session};

pub struct DumpHFDLSession {
    process: Child,

    reader: BufReader<ChildStdout>,
    stderr: ChildStderr,

    session_start: Instant,
    session_end: Option<Duration>,
    end_session_on_timeout: bool,
}

#[async_trait]
impl Session for DumpHFDLSession {
    async fn read_message(&mut self, msg: &mut String) -> Result<usize, io::Error> {
        if let Some(session_end) = self.session_end {
            select! {
                _ = sleep_until(self.session_start + session_end) => {
                    return Err(
                        io::Error::new(io::ErrorKind::ConnectionReset, "Session ended by schedule")
                    )
                }
                result = self.reader.read_line(msg) => result
            }
        } else {
            self.reader.read_line(msg).await
        }
    }

    async fn on_timeout(&mut self) -> bool {
        self.end_session_on_timeout
    }

    async fn get_errors(&self) -> String {
        String::from("")
    }

    async fn end(&mut self, reason: EndSessionReason) {
        debug!("Terminating launched dumphfdl process...");

        #[allow(unused_must_use)]
        {
            self.process.kill().await;
        }

        debug!("HFDL session terminated: reason={:?}", reason);
    }
}

impl DumpHFDLSession {
    pub fn new(
        process: Child,
        reader: BufReader<ChildStdout>,
        stderr: ChildStderr,
        session_end_datetime: Option<DateTime<Local>>,
        end_session_on_timeout: bool,
    ) -> DumpHFDLSession {
        let mut session_end: Option<Duration> = None;
        if let Some(dt) = session_end_datetime {
            match (dt - Local::now()).to_std() {
                Ok(x) => session_end = Some(x),
                Err(e) => warn!(
                    "New session failed to set session end time: {}",
                    e.to_string()
                ),
            }
        }

        DumpHFDLSession {
            process,
            reader,
            stderr,
            end_session_on_timeout,
            session_start: Instant::now(),
            session_end,
        }
    }
}