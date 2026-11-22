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

    /// True when this is the only handle left holding the bus open: everyone
    /// else (the session) has let go, so input has ended.
    pub fn is_last(&self) -> bool {
        self.tx.strong_count() == 1
    }
}

/// See [`MessageBus::downgrade`].
#[derive(Clone)]
pub struct WeakBus {
    tx: broadcast::WeakSender<Arc<Message>>,
}

impl WeakBus {
    /// A bus handle that keeps the bus open, or `None` once it has closed.
    pub fn upgrade(&self) -> Option<MessageBus> {
        self.tx.upgrade().map(|tx| MessageBus { tx })
    }

    /// Subscribe, or `None` once the bus has closed.
    pub fn subscribe(&self) -> Option<broadcast::Receiver<Arc<Message>>> {
        self.tx.upgrade().map(|tx| tx.subscribe())
    }

    /// Publish if the bus is still open; returns the number of subscribers.
    pub fn publish(&self, msg: Message) -> usize {
        self.tx.upgrade().map(|tx| tx.send(Arc::new(msg)).unwrap_or(0)).unwrap_or(0)
    }
}

impl Default for MessageBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgraded_handle_keeps_the_bus_open_and_knows_when_it_is_last() {
        let bus = MessageBus::new();
        let mut rx = bus.subscribe();
        let weak = bus.downgrade();
        let hold = weak.upgrade().expect("bus is open");
        assert!(!hold.is_last());
        drop(bus);
        // The session has let go: the holder is last, and the bus is still open.
        assert!(hold.is_last());
        assert!(weak.subscribe().is_some());
        drop(hold);
        assert!(weak.upgrade().is_none());
        assert!(matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Closed)));
    }
}
