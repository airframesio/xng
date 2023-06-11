use async_trait::async_trait;
use tokio::io;

#[derive(Debug)]
pub enum EndSessionReason {
    None,
    SessionTimeout,
    UserInterrupt,
    UserAPIControl,
    ReadError,
    ReadEOF,
    ProcessStartError,
}

#[async_trait]
pub trait Session {
    async fn read_message(&mut self, msg: &mut String) -> Result<usize, io::Error>;

    async fn on_timeout(&mut self) -> bool;
    async fn get_errors(&self) -> String;
    async fn end(&mut self, reason: EndSessionReason);
}
