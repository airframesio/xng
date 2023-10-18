use log::*;

use async_trait::async_trait;
use tokio::io::{self, AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdout};

use crate::modules::session::{EndSessionReason, Session};

pub struct DumpVDL2Session {
    process: Child,

    reader: BufReader<ChildStdout>,
    stderr: ChildStderr,

    bands: Vec<u64>,
}

#[async_trait]
impl Session for DumpVDL2Session {
    async fn read_message(&mut self, msg: &mut String) -> Result<usize, io::Error> {
        self.reader.read_line(msg).await
    }

    async fn on_timeout(&mut self) -> bool {
        false
    }

    async fn get_errors(&mut self) -> String {
        let mut errors = String::new();
        if let Err(e) = self.stderr.read_to_string(&mut errors).await {
            return format!("Failed to read STDERR: {}", e.to_string());
        }

        errors
    }

    fn get_listening_band(&self) -> &Vec<u64> {
        &self.bands
    }

    async fn end(&mut self, reason: EndSessionReason) {
        debug!("Terminating launched dumpvdl2 process...");

        #[allow(unused_must_use)]
        {
            self.process.kill().await;
        }

        debug!("AoA session terminated: reason={:?}", reason);
    }
}

impl DumpVDL2Session {
    pub fn new(
        process: Child,
        reader: BufReader<ChildStdout>,
        stderr: ChildStderr,
        bands: Vec<u64>,
    ) -> DumpVDL2Session {
        DumpVDL2Session {
            process,
            reader,
            stderr,
            bands,
        }
    }
}
