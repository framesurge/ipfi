use crate::{IpfiAtomicInteger, IpfiInteger};
use crossbeam_queue::SegQueue;
use std::sync::atomic::Ordering;

/// A reuse-or-increment queue for unique identifiers. When polled for a new identifier, this will fist attempt to reuse any
/// relinquished identifiers, before incrementing to create a new identifier if no other could be found. This is lock-free, and
/// optimised for highly concurrent use-cases.
///
/// This exists to avoid using `RwLock<Vec<_>>` and the like in situations where extreme concurrency is warranted, such as if
/// IPFI is used as a network protocol for handling high volumes of requests.
pub(crate) struct RoiQueue {
    /// A counter that represents one above the highest active identifier. In other words, the value of this counter will always
    /// be an identifier that is not yet in circulation.
    counter: IpfiAtomicInteger,
    /// A lock-free queue containing all the identifiers that have been 'relinquished' (i.e. that are no longer in use). These
    /// will be recirculated automatically when a new identifier is requested.
    relinquished: SegQueue<IpfiInteger>,
}
impl Default for RoiQueue {
    fn default() -> Self {
        Self {
            counter: IpfiAtomicInteger::new(0),
            relinquished: SegQueue::new(),
        }
    }
}
impl RoiQueue {
    /// Initialises a new reuse-or-increment queue.
    pub fn new() -> Self {
        Self::default()
    }
    /// Gets a new unique identifier from this queue. This method guarantees that the returned identifier will be unique among
    /// those returned from *this* queue.
    pub fn get(&self) -> IpfiInteger {
        // Attempt to acquire a relinquished ID from the queue
        if let Some(id) = self.relinquished.pop() {
            id
        } else {
            // There were no relinquished IDs, this will return the current value and add one to the stored version
            self.counter.fetch_add(1, Ordering::Relaxed)
        }
    }
    /// Marks the given identifier as relinquished. After this method is called, the given identifier will almost certainly
    /// point to something else, and as such it should no longer be considered valid.
    pub fn relinquish(&self, id: IpfiInteger) {
        self.relinquished.push(id);
    }
}
