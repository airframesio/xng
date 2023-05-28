use async_trait::async_trait;
use tokio::io;

pub enum EndSessionReason {
    None,
    SessionTimeout,
    UserInterrupt,
    UserAPIControl,
    ReadError,
    BadReadSize,
}

#[async_trait]
pub trait Session {
    async fn read_message(&self, msg: &mut String) -> io::Result<usize>;

    fn end(&mut self, reason: EndSessionReason);
}
