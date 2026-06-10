//! In-process message bus: decode cores publish normalized messages, outputs
//! subscribe. Backed by a tokio broadcast channel; slow consumers lag (drop
//! oldest) rather than back-pressure the decode path.

use std::sync::Arc;
use tokio::sync::broadcast;
use xng_types::Message;

const BUS_CAPACITY: usize = 4096;

#[derive(Clone)]
pub struct MessageBus {
    tx: broadcast::Sender<Arc<Message>>,
}

impl MessageBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BUS_CAPACITY);
        Self { tx }
    }

    /// Publish a message. Returns the number of current subscribers.
    pub fn publish(&self, msg: Message) -> usize {
        // send() only errors when there are no subscribers; that's fine.
        self.tx.send(Arc::new(msg)).unwrap_or(0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Message>> {
        self.tx.subscribe()
    }
}

impl Default for MessageBus {
    fn default() -> Self {
        Self::new()
    }
}
