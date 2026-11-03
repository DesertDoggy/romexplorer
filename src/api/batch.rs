//! Request coalescing for providers whose API takes a list per request (IGDB `where id =
//! (...)`, ...): individual lookups queue up and go out as one list request, split into
//! chunks of the provider's maximum. The wait for more entries is bounded by one
//! rate-limit slot of that provider (`for_rate`: 4/s -> 250 ms) -- a lone lookup never
//! waits longer than the provider would have made it wait anyway.

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::oneshot;

use super::{Error, Result};

pub type FetchFuture<K, V> = Pin<Box<dyn Future<Output = Result<HashMap<K, V>>> + Send + 'static>>;
pub type FetchFn<K, V> = Arc<dyn Fn(Vec<K>) -> FetchFuture<K, V> + Send + Sync + 'static>;

struct Pending<K, V> {
    order: Vec<K>,
    waiters: HashMap<K, Vec<oneshot::Sender<Result<Option<V>>>>>,
    timer_armed: bool,
}

struct Inner<K, V> {
    max_batch: usize,
    window: Duration,
    fetch: FetchFn<K, V>,
    pending: Mutex<Pending<K, V>>,
}

pub struct Coalescer<K, V> {
    inner: Arc<Inner<K, V>>,
}

impl<K, V> Clone for Coalescer<K, V> {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner) }
    }
}

impl<K, V> Coalescer<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// `max_batch` keys per request; a flush happens when that many are waiting, or
    /// `window` after the first one arrived, whichever comes first.
    pub fn new(max_batch: usize, window: Duration, fetch: FetchFn<K, V>) -> Self {
        Self { inner: Arc::new(Inner { max_batch: max_batch.max(1), window, fetch, pending: Mutex::new(Pending { order: Vec::new(), waiters: HashMap::new(), timer_armed: false }) }) }
    }

    /// Window = one slot of a `per_second` rate limit.
    pub fn for_rate(max_batch: usize, per_second: u32, fetch: FetchFn<K, V>) -> Self {
        Self::new(max_batch, Duration::from_millis(1000 / u64::from(per_second.max(1))), fetch)
    }

    /// One key; `Ok(None)` when the provider returned nothing for it.
    pub async fn lookup(&self, key: K) -> Result<Option<V>> {
        let rx = self.enqueue(key);
        rx.await.unwrap_or_else(|_| Err(Error::internal("coalescer dropped a lookup")))
    }

    /// Many keys at once (chunked into requests of `max_batch`); keys the provider didn't
    /// return are simply absent from the map.
    pub async fn lookup_many(&self, keys: Vec<K>) -> Result<HashMap<K, V>> {
        let receivers: Vec<(K, oneshot::Receiver<Result<Option<V>>>)> = keys.into_iter().map(|k| (k.clone(), self.enqueue(k))).collect();
        let mut out = HashMap::new();
        for (key, rx) in receivers {
            if let Some(v) = rx.await.unwrap_or_else(|_| Err(Error::internal("coalescer dropped a lookup")))? {
                out.insert(key, v);
            }
        }
        Ok(out)
    }

    fn enqueue(&self, key: K) -> oneshot::Receiver<Result<Option<V>>> {
        let (tx, rx) = oneshot::channel();
        let (flush_now, arm_timer) = {
            let mut p = self.inner.pending.lock().expect("coalescer lock");
            if !p.waiters.contains_key(&key) {
                p.order.push(key.clone());
            }
            p.waiters.entry(key).or_default().push(tx);
            let full = p.order.len() >= self.inner.max_batch;
            let arm = !p.timer_armed && !full;
            if arm {
                p.timer_armed = true;
            }
            (full, arm)
        };
        let me = self.clone();
        if flush_now {
            tokio::spawn(async move { me.flush_all().await });
        } else if arm_timer {
            let window = self.inner.window;
            tokio::spawn(async move {
                tokio::time::sleep(window).await;
                me.inner.pending.lock().expect("coalescer lock").timer_armed = false;
                me.flush_all().await;
            });
        }
        rx
    }

    /// Flushes in `max_batch` chunks until nothing is pending.
    async fn flush_all(&self) {
        loop {
            let (keys, waiters) = {
                let mut p = self.inner.pending.lock().expect("coalescer lock");
                if p.order.is_empty() {
                    return;
                }
                let n = p.order.len().min(self.inner.max_batch);
                let keys: Vec<K> = p.order.drain(..n).collect();
                let waiters: Vec<(K, Vec<oneshot::Sender<Result<Option<V>>>>)> = keys.iter().map(|k| (k.clone(), p.waiters.remove(k).unwrap_or_default())).collect();
                (keys, waiters)
            };
            let result = (self.inner.fetch)(keys).await;
            for (key, senders) in waiters {
                for tx in senders {
                    let _ = tx.send(match &result {
                        Ok(map) => Ok(map.get(&key).cloned()),
                        Err(e) => Err(e.clone()),
                    });
                }
            }
        }
    }
}

/// Splits `keys` into request-sized chunks -- for callers that don't need the queue, just
/// the "don't exceed the provider's list maximum" rule.
pub fn chunks<K: Clone>(keys: &[K], max_batch: usize) -> Vec<Vec<K>> {
    keys.chunks(max_batch.max(1)).map(|c| c.to_vec()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    fn counting_fetch(calls: Arc<AtomicUsize>, sizes: Arc<Mutex<Vec<usize>>>) -> FetchFn<u32, String> {
        Arc::new(move |keys: Vec<u32>| {
            calls.fetch_add(1, Ordering::SeqCst);
            sizes.lock().unwrap().push(keys.len());
            Box::pin(async move { Ok(keys.into_iter().filter(|k| k % 7 != 0).map(|k| (k, format!("v{k}"))).collect()) })
        })
    }

    #[tokio::test]
    async fn many_keys_are_split_into_provider_sized_requests() {
        let calls = Arc::new(AtomicUsize::new(0));
        let sizes = Arc::new(Mutex::new(Vec::new()));
        let c = Coalescer::new(4, Duration::from_millis(50), counting_fetch(calls.clone(), sizes.clone()));
        let out = c.lookup_many((1..=10).collect()).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3, "10 keys at 4 per request = 3 requests");
        assert_eq!(sizes.lock().unwrap().iter().sum::<usize>(), 10);
        assert_eq!(out.get(&3).map(String::as_str), Some("v3"));
        assert!(!out.contains_key(&7), "keys the provider didn't return are absent");
    }

    #[tokio::test]
    async fn concurrent_lookups_within_the_window_share_one_request() {
        let calls = Arc::new(AtomicUsize::new(0));
        let sizes = Arc::new(Mutex::new(Vec::new()));
        let c = Coalescer::for_rate(100, 4, counting_fetch(calls.clone(), sizes.clone()));
        let (a, b, d) = tokio::join!(c.lookup(1), c.lookup(2), c.lookup(3));
        assert_eq!((a.unwrap(), b.unwrap(), d.unwrap()), (Some("v1".into()), Some("v2".into()), Some("v3".into())));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(sizes.lock().unwrap()[0], 3);
    }

    #[tokio::test]
    async fn a_lone_lookup_waits_at_most_one_rate_slot() {
        let c = Coalescer::for_rate(100, 4, counting_fetch(Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(Vec::new()))));
        let started = Instant::now();
        assert_eq!(c.lookup(5).await.unwrap(), Some("v5".into()));
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(240) && waited < Duration::from_millis(700), "waited {waited:?}");
        assert_eq!(c.lookup(7).await.unwrap(), None);
    }

    #[tokio::test]
    async fn fetch_errors_reach_every_waiter() {
        let fetch: FetchFn<u32, String> = Arc::new(|_keys| Box::pin(async { Err(Error::unavailable("provider down")) }));
        let c = Coalescer::new(10, Duration::from_millis(10), fetch);
        let (a, b) = tokio::join!(c.lookup(1), c.lookup(2));
        assert!(a.is_err() && b.is_err());
        assert_eq!(chunks(&[1, 2, 3, 4, 5], 2), vec![vec![1, 2], vec![3, 4], vec![5]]);
    }
}
