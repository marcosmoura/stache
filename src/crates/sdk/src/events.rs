//! Typed, pull-based event delivery.
//!
//! Publishing never calls subscriber code and never blocks: each subscription
//! owns a bounded `std::sync::mpsc` queue, and a publish only performs a
//! non-blocking `try_send`. A full queue drops the newest event and increments
//! that subscription's dropped counter, so a slow or absent consumer can never
//! stall a native callback that publishes an event.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::lock;

/// A typed event bus owned by one registry.
///
/// There is no global bus: two registries (for example two test hosts) have
/// independent subscriptions and dropped counters.
#[derive(Default)]
pub struct EventBus {
    inner: Arc<EventBusInner>,
}

#[derive(Default)]
struct EventBusInner {
    subscribers: Mutex<Subscribers>,
}

#[derive(Default)]
struct Subscribers {
    next_id: u64,
    by_type: HashMap<TypeId, Vec<Registration>>,
}

struct Registration {
    id: u64,
    subscriber: Arc<dyn ErasedSubscriber>,
}

/// A type-erased delivery target. The bus only asks a subscriber to deliver
/// events it was registered for, so the downcast inside an implementation is an
/// internal invariant rather than a fallible user operation.
trait ErasedSubscriber: Send + Sync {
    fn deliver(&self, event: &dyn Any);
}

struct ChannelSubscriber<E> {
    sender: SyncSender<E>,
    dropped: Arc<AtomicU64>,
}

impl<E: Clone + Send + 'static> ErasedSubscriber for ChannelSubscriber<E> {
    fn deliver(&self, event: &dyn Any) {
        let Some(value) = event.downcast_ref::<E>() else { return };
        match self.sender.try_send(value.clone()) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl EventBus {
    /// Creates an event bus with no subscriptions.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Publishes an event to every current subscriber of type `E`.
    ///
    /// The call is non-blocking and never invokes subscriber code. With no
    /// subscribers the event is dropped without cloning it.
    #[allow(clippy::needless_pass_by_value)] // The event is moved in and cloned per subscription.
    pub fn publish<E: Clone + Send + 'static>(&self, event: E) {
        let subscribers = {
            let subscribers = lock(&self.inner.subscribers);
            subscribers
                .by_type
                .get(&TypeId::of::<E>())
                .map(|registrations| {
                    registrations
                        .iter()
                        .map(|registration| Arc::clone(&registration.subscriber))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        for subscriber in subscribers {
            subscriber.deliver(&event);
        }
    }

    /// Creates a bounded, typed subscription.
    ///
    /// `capacity` is clamped to at least one: a zero-capacity queue could never
    /// accept a pull-based event. When the queue is full, the newest event is
    /// dropped and that subscription's `dropped()` counter increments; older
    /// queued events stay readable.
    #[must_use]
    pub fn subscribe<E: Clone + Send + 'static>(&self, capacity: usize) -> Subscription<E> {
        let (sender, receiver) = mpsc::sync_channel(capacity.max(1));
        let dropped = Arc::new(AtomicU64::new(0));
        let id = {
            let mut subscribers = lock(&self.inner.subscribers);
            let id = subscribers.next_id;
            subscribers.next_id += 1;
            subscribers.by_type.entry(TypeId::of::<E>()).or_default().push(Registration {
                id,
                subscriber: Arc::new(ChannelSubscriber {
                    sender,
                    dropped: Arc::clone(&dropped),
                }),
            });
            id
        };
        Subscription {
            inner: Arc::clone(&self.inner),
            type_id: TypeId::of::<E>(),
            id,
            receiver,
            dropped,
        }
    }
}

/// A bounded receiver that unsubscribes when dropped.
///
/// Consumers pull with [`Subscription::try_recv`] or
/// [`Subscription::recv_timeout`]; the bus never calls into the consumer.
pub struct Subscription<E> {
    inner: Arc<EventBusInner>,
    type_id: TypeId,
    id: u64,
    receiver: Receiver<E>,
    dropped: Arc<AtomicU64>,
}

impl<E> Subscription<E> {
    /// Receives one queued event without waiting.
    ///
    /// # Errors
    /// Returns the standard channel error when the queue is empty or the
    /// sender was dropped.
    pub fn try_recv(&self) -> Result<E, TryRecvError> { self.receiver.try_recv() }

    /// Receives one queued event, waiting at most `timeout`.
    ///
    /// # Errors
    /// Returns the standard timeout or disconnection error.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<E, RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }

    /// How many events this subscription has dropped because its queue was
    /// full.
    #[must_use]
    pub fn dropped(&self) -> u64 { self.dropped.load(Ordering::Relaxed) }
}

impl<E> Drop for Subscription<E> {
    fn drop(&mut self) {
        let mut subscribers = lock(&self.inner.subscribers);
        let empty = subscribers.by_type.get_mut(&self.type_id).is_some_and(|registrations| {
            registrations.retain(|registration| registration.id != self.id);
            registrations.is_empty()
        });
        if empty {
            subscribers.by_type.remove(&self.type_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::TryRecvError;
    use std::time::Instant;

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Tick(u32);

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Other;

    #[test]
    fn publish_delivers_to_matching_subscriptions() {
        let bus = EventBus::new();
        let ticks = bus.subscribe::<Tick>(4);
        bus.publish(Tick(1));
        bus.publish(Tick(2));
        assert_eq!(ticks.try_recv(), Ok(Tick(1)));
        assert_eq!(ticks.try_recv(), Ok(Tick(2)));
        assert_eq!(ticks.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn unrelated_event_types_are_not_delivered() {
        let bus = EventBus::new();
        let ticks = bus.subscribe::<Tick>(2);
        bus.publish(Other);
        bus.publish(Tick(7));
        assert_eq!(ticks.try_recv(), Ok(Tick(7)));
        assert_eq!(ticks.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn overflow_drops_the_newest_event_and_counts_it() {
        let bus = EventBus::new();
        let ticks = bus.subscribe::<Tick>(2);
        for value in 0..5 {
            bus.publish(Tick(value));
        }
        assert_eq!(ticks.dropped(), 3);
        assert_eq!(ticks.try_recv(), Ok(Tick(0)));
        assert_eq!(ticks.try_recv(), Ok(Tick(1)));
        assert_eq!(ticks.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn zero_capacity_still_accepts_one_event() {
        let bus = EventBus::new();
        let ticks = bus.subscribe::<Tick>(0);
        bus.publish(Tick(1));
        assert_eq!(ticks.try_recv(), Ok(Tick(1)));
    }

    #[test]
    fn dropping_a_subscription_unregisters_it() {
        let bus = EventBus::new();
        let first = bus.subscribe::<Tick>(2);
        let second = bus.subscribe::<Tick>(2);
        drop(first);
        bus.publish(Tick(9));
        assert_eq!(second.try_recv(), Ok(Tick(9)));
        let replacement = bus.subscribe::<Tick>(2);
        bus.publish(Tick(10));
        assert_eq!(second.try_recv(), Ok(Tick(10)));
        assert_eq!(replacement.try_recv(), Ok(Tick(10)));
    }

    #[test]
    fn an_absent_subscriber_never_blocks_publish() {
        let bus = EventBus::new();
        let slow = bus.subscribe::<Tick>(1);
        let started = Instant::now();
        for value in 0..1_000 {
            bus.publish(Tick(value));
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "publish blocked on a full queue"
        );
        assert_eq!(slow.dropped(), 999);
    }

    #[test]
    fn two_buses_keep_independent_subscriptions() {
        let first = EventBus::new();
        let second = EventBus::new();
        let first_ticks = first.subscribe::<Tick>(2);
        let second_ticks = second.subscribe::<Tick>(2);
        first.publish(Tick(1));
        assert_eq!(first_ticks.try_recv(), Ok(Tick(1)));
        assert_eq!(second_ticks.try_recv(), Err(TryRecvError::Empty));
    }
}
