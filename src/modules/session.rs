use async_trait::async_trait;
use tokio::io;

pub const SESSION_SCHEDULED_END: &'static str = "SESSION_SCHEDULED_END";

#[derive(Copy, Clone, Debug)]
pub enum EndSessionReason {
    None,
    SessionTimeout,
    SessionEnd,
    SessionUpdate,
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
    async fn get_errors(&mut self) -> String;
    async fn end(&mut self, reason: EndSessionReason);
}
