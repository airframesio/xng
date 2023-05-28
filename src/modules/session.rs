use async_trait::async_trait;
use tokio::io;

pub enum EndSessionReason {
    None,
    SessionTimeout,
    UserInterrupt,
    UserAPIControl,
    ReadError,
    BadReadSize,
    ProcessStartError,
}

#[async_trait]
pub trait Session {
    async fn read_message(&mut self, msg: &mut String) -> Result<usize, io::Error>;

    async fn get_errors(&self) -> String;
    async fn end(&mut self, reason: EndSessionReason);
}
