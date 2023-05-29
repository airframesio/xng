use async_trait::async_trait;
use log::*;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdout};

use crate::modules::session::{EndSessionReason, Session};

pub struct DumpHFDLSession {
    process: Child,

    reader: BufReader<ChildStdout>,
    stderr: ChildStderr,
}

#[async_trait]
impl Session for DumpHFDLSession {
    async fn read_message(&mut self, msg: &mut String) -> Result<usize, io::Error> {
        self.reader.read_line(msg).await
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
    ) -> DumpHFDLSession {
        DumpHFDLSession {
            process,
            reader,
            stderr,
        }
    }
}
