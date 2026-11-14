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

    /// A handle that can subscribe while the bus is open but does not keep
    /// it open. Outputs exit when the last [`MessageBus`] is dropped, so a
    /// long-lived observer must hold one of these rather than a bus clone.
    pub fn downgrade(&self) -> WeakBus {
        WeakBus { tx: self.tx.downgrade() }
    }
}

/// See [`MessageBus::downgrade`].
#[derive(Clone)]
pub struct WeakBus {
    tx: broadcast::WeakSender<Arc<Message>>,
}

impl WeakBus {
    /// Subscribe, or `None` once the bus has closed.
    pub fn subscribe(&self) -> Option<broadcast::Receiver<Arc<Message>>> {
        self.tx.upgrade().map(|tx| tx.subscribe())
    }
}

impl Default for MessageBus {
    fn default() -> Self {
        Self::new()
    }
}
