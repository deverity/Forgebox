//! Bounded drop-oldest queue for stream items (docs/03 §7.3: a slow subscriber never
//! holds the producer). Used per subscription by the server outbox and by the client.
//!
//! When full, pushing drops the **oldest** item and counts it. Lag reported by the
//! producer itself (e.g. the event bus dropped events for this subscriber) is kept as an
//! ordered marker. [`DropOldestQueue::pop`] yields a [`Popped::Lagged`] exactly where
//! items went missing, so the consumer sees `lagged{n}` right before the first item
//! after the gap. The queue has no lock of its own: the owner wraps it.

use std::collections::VecDeque;

#[derive(Debug)]
enum Entry<T> {
    Item(T),
    Lagged(u64),
}

/// What [`DropOldestQueue::pop`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popped<T> {
    Item(T),
    /// `n` items were lost at this point of the stream.
    Lagged(u64),
}

/// Bounded queue that drops the oldest item when full.
#[derive(Debug)]
pub struct DropOldestQueue<T> {
    entries: VecDeque<Entry<T>>,
    items: usize,
    capacity: usize,
    /// Lost at the head of the queue: reported before the next entry.
    head_missed: u64,
    total_missed: u64,
}

impl<T> DropOldestQueue<T> {
    /// Queue holding at most `capacity` items (at least 1).
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            items: 0,
            capacity: capacity.max(1),
            head_missed: 0,
            total_missed: 0,
        }
    }

    /// Appends `item`, dropping the oldest item if the queue is full. Returns whether an
    /// item was dropped.
    pub fn push(&mut self, item: T) -> bool {
        let mut dropped = false;
        if self.items >= self.capacity {
            while let Some(entry) = self.entries.pop_front() {
                match entry {
                    Entry::Lagged(n) => self.head_missed += n,
                    Entry::Item(_) => {
                        self.items -= 1;
                        self.head_missed += 1;
                        self.total_missed += 1;
                        dropped = true;
                        break;
                    }
                }
            }
        }
        self.entries.push_back(Entry::Item(item));
        self.items += 1;
        dropped
    }

    /// Records that `n` items were lost upstream at this point of the stream.
    pub fn push_lagged(&mut self, n: u64) {
        if n == 0 {
            return;
        }
        self.total_missed += n;
        match self.entries.back_mut() {
            Some(Entry::Lagged(m)) => *m += n,
            Some(Entry::Item(_)) => self.entries.push_back(Entry::Lagged(n)),
            None => self.head_missed += n,
        }
    }

    /// Next entry in stream order.
    pub fn pop(&mut self) -> Option<Popped<T>> {
        if self.head_missed > 0 {
            let mut n = std::mem::take(&mut self.head_missed);
            if let Some(Entry::Lagged(m)) = self.entries.front() {
                n += m;
                self.entries.pop_front();
            }
            return Some(Popped::Lagged(n));
        }
        match self.entries.pop_front()? {
            Entry::Item(item) => {
                self.items -= 1;
                Some(Popped::Item(item))
            }
            Entry::Lagged(n) => Some(Popped::Lagged(n)),
        }
    }

    /// Items waiting (lag markers excluded).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.items
    }

    /// Nothing to pop.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.head_missed == 0
    }

    /// The queue is at capacity: the next push drops an item.
    pub fn is_full(&self) -> bool {
        self.items >= self.capacity
    }

    /// Items lost so far (dropped here or reported upstream).
    #[cfg(test)]
    pub fn total_missed(&self) -> u64 {
        self.total_missed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain<T>(q: &mut DropOldestQueue<T>) -> Vec<Popped<T>> {
        std::iter::from_fn(|| q.pop()).collect()
    }

    #[test]
    fn keeps_order_under_capacity() {
        let mut q = DropOldestQueue::new(4);
        for i in 0..3 {
            assert!(!q.push(i));
        }
        assert_eq!(q.len(), 3);
        assert_eq!(
            drain(&mut q),
            vec![Popped::Item(0), Popped::Item(1), Popped::Item(2)]
        );
        assert!(q.is_empty());
    }

    #[test]
    fn drops_oldest_and_reports_before_survivors() {
        let mut q = DropOldestQueue::new(3);
        for i in 0..10 {
            q.push(i);
        }
        assert_eq!(q.total_missed(), 7);
        assert_eq!(
            drain(&mut q),
            vec![
                Popped::Lagged(7),
                Popped::Item(7),
                Popped::Item(8),
                Popped::Item(9)
            ]
        );
    }

    #[test]
    fn upstream_lag_is_reported_in_place() {
        let mut q = DropOldestQueue::new(10);
        q.push(1);
        q.push(2);
        q.push_lagged(5);
        q.push_lagged(1);
        q.push(9);
        assert_eq!(
            drain(&mut q),
            vec![
                Popped::Item(1),
                Popped::Item(2),
                Popped::Lagged(6),
                Popped::Item(9)
            ]
        );
        q.push_lagged(3);
        assert_eq!(drain(&mut q), vec![Popped::Lagged(3)]);
        assert_eq!(q.total_missed(), 9);
    }

    #[test]
    fn dropping_through_a_marker_merges_counts() {
        let mut q = DropOldestQueue::new(2);
        q.push(1);
        q.push_lagged(4);
        q.push(6);
        q.push(7); // drops 1
        q.push(8); // drops the marker (4) and 6
        assert_eq!(
            drain(&mut q),
            vec![Popped::Lagged(6), Popped::Item(7), Popped::Item(8)]
        );
    }
}
