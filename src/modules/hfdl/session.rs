use async_trait::async_trait;
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
        match self.reader.read_line(msg).await {
            Ok(size) => Ok(size),
            Err(e) => Err(e),
        }
    }

    async fn get_errors(&self) -> String {
        todo!();
    }

    async fn end(&mut self, reason: EndSessionReason) {
        self.process.kill().await;
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
