use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) struct TtlCache<K, V>
where
    K: Eq + Hash,
{
    state: Arc<Mutex<State<K, V>>>,
    capacity: usize,
    ttl: Duration,
}

struct State<K, V>
where
    K: Eq + Hash,
{
    entries: HashMap<K, Entry<V>>,
    order: VecDeque<K>,
}

struct Entry<V> {
    value: V,
    expires_at: Instant,
}

impl<K, V> Clone for TtlCache<K, V>
where
    K: Eq + Hash,
{
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
            capacity: self.capacity,
            ttl: self.ttl,
        }
    }
}

impl<K, V> TtlCache<K, V>
where
    K: Eq + Hash + Clone,
{
    pub(crate) fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                entries: HashMap::new(),
                order: VecDeque::new(),
            })),
            capacity,
            ttl,
        }
    }

    pub(crate) fn get(&self, key: &K) -> Option<V>
    where
        V: Clone,
    {
        let mut state = self.lock_state();
        let now = Instant::now();
        self.remove_expired(&mut state, now);
        let value = state.entries.get(key).map(|entry| entry.value.clone());
        if value.is_some() {
            move_to_back(&mut state.order, key);
        }
        value
    }

    pub(crate) fn insert(&self, key: K, value: V) {
        if self.capacity == 0 || self.ttl.is_zero() {
            return;
        }

        let mut state = self.lock_state();
        let now = Instant::now();
        self.remove_expired(&mut state, now);
        let expires_at = now.checked_add(self.ttl).unwrap_or(now);

        if state.entries.contains_key(&key) {
            state
                .entries
                .insert(key.clone(), Entry { value, expires_at });
            move_to_back(&mut state.order, &key);
            return;
        }

        while state.entries.len() >= self.capacity {
            let Some(oldest) = state.order.pop_front() else {
                break;
            };
            state.entries.remove(&oldest);
        }

        state
            .entries
            .insert(key.clone(), Entry { value, expires_at });
        state.order.push_back(key);
    }

    pub(crate) fn len(&self) -> usize {
        let mut state = self.lock_state();
        self.remove_expired(&mut state, Instant::now());
        state.entries.len()
    }

    pub(crate) fn clear(&self) {
        let mut state = self.lock_state();
        state.entries.clear();
        state.order.clear();
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, State<K, V>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn remove_expired(&self, state: &mut State<K, V>, now: Instant) {
        let expired = state
            .order
            .iter()
            .filter(|key| {
                state
                    .entries
                    .get(*key)
                    .is_none_or(|entry| entry.expires_at <= now)
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in expired {
            state.entries.remove(&key);
            remove_from_order(&mut state.order, &key);
        }
    }
}

fn move_to_back<K>(order: &mut VecDeque<K>, key: &K)
where
    K: Eq + Clone,
{
    remove_from_order(order, key);
    order.push_back(key.clone());
}

fn remove_from_order<K>(order: &mut VecDeque<K>, key: &K)
where
    K: Eq,
{
    if let Some(index) = order.iter().position(|current| current == key) {
        order.remove(index);
    }
}

#[cfg(test)]
mod tests {
    use super::TtlCache;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn clones_share_entries() {
        let cache = TtlCache::new(2, Duration::from_secs(1));
        cache.insert("query", "result");
        let clone = cache.clone();

        assert_eq!(clone.get(&"query"), Some("result"));
        clone.clear();
        assert_eq!(cache.get(&"query"), None);
    }

    #[test]
    fn expires_entries() {
        let cache = TtlCache::new(1, Duration::from_millis(10));
        cache.insert("query", "result");
        assert_eq!(cache.get(&"query"), Some("result"));
        thread::sleep(Duration::from_millis(20));
        assert_eq!(cache.get(&"query"), None);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn evicts_least_recently_used_entry_at_capacity() {
        let cache = TtlCache::new(2, Duration::from_secs(1));
        cache.insert("first", 1);
        cache.insert("second", 2);
        assert_eq!(cache.get(&"first"), Some(1));
        cache.insert("third", 3);

        assert_eq!(cache.get(&"first"), Some(1));
        assert_eq!(cache.get(&"second"), None);
        assert_eq!(cache.get(&"third"), Some(3));
    }

    #[test]
    fn replacing_entry_refreshes_ttl_and_recency() {
        let cache = TtlCache::new(2, Duration::from_secs(1));
        cache.insert("first", 1);
        cache.insert("second", 2);
        cache.insert("first", 10);
        cache.insert("third", 3);

        assert_eq!(cache.get(&"first"), Some(10));
        assert_eq!(cache.get(&"second"), None);
        assert_eq!(cache.get(&"third"), Some(3));
    }

    #[test]
    fn zero_capacity_or_ttl_does_not_store_entries() {
        let no_capacity = TtlCache::new(0, Duration::from_secs(1));
        no_capacity.insert("query", "result");
        assert_eq!(no_capacity.len(), 0);

        let no_ttl = TtlCache::new(1, Duration::ZERO);
        no_ttl.insert("query", "result");
        assert_eq!(no_ttl.len(), 0);
    }

    #[test]
    fn concurrent_clones_remain_bounded() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TtlCache<String, usize>>();

        let cache = Arc::new(TtlCache::new(16, Duration::from_secs(1)));
        let handles = (0..4)
            .map(|worker| {
                let cache = Arc::clone(&cache);
                thread::spawn(move || {
                    for index in 0..32 {
                        cache.insert(format!("{worker}:{index}"), index);
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().expect("cache worker should finish");
        }

        assert_eq!(cache.len(), 16);
    }

    #[test]
    fn expired_entries_are_reclaimed_before_eviction() {
        let cache = TtlCache::new(1, Duration::from_millis(10));
        cache.insert("old", 1);
        thread::sleep(Duration::from_millis(20));
        cache.insert("new", 2);

        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(&"old"), None);
        assert_eq!(cache.get(&"new"), Some(2));
    }
}
