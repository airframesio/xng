use std::io;

use async_trait::async_trait;

use crate::modules::session::{EndSessionReason, Session};

pub struct DumpHFDLSession {}

#[async_trait]
impl Session for DumpHFDLSession {
    async fn read_message(&self, msg: &mut String) -> io::Result<usize> {
        todo!();
    }

    fn end(&mut self, reason: EndSessionReason) {
        todo!();
    }
}

impl DumpHFDLSession {}
