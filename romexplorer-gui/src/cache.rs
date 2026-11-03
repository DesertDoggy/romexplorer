//! The offline cache seam. Every successful GET is remembered under its path; when the server
//! can't be reached, a GET answers from here instead (the caller can't tell, apart from the
//! toast the shell shows). This session-only memory store is the first implementation; a
//! phone build swaps in a persistent one (a small key -> JSON SQLite table plus thumbnails,
//! docs/plan/gui.md) behind the same trait, without touching the screens.

use std::cell::RefCell;
use std::collections::HashMap;

/// Bodies are kept as the text the server sent, not as a parsed `serde_json::Value`: a typed
/// caller (`api::get_as`) would otherwise pay for a tree it never reads, and a `Value` caller
/// parses either way.
pub trait ResponseCache {
    fn get(&self, key: &str) -> Option<String>;
    fn put(&self, key: &str, body: &str);
    /// Forgets everything: on signing in or out, so one account never sees another's pages.
    fn clear(&self);
}

/// Remembers responses for this run of the app, up to `MAX` entries (oldest out first).
#[derive(Default)]
pub struct MemoryCache {
    map: RefCell<HashMap<String, (u64, String)>>,
    tick: RefCell<u64>,
}

const MAX: usize = 400;

impl ResponseCache for MemoryCache {
    fn get(&self, key: &str) -> Option<String> {
        self.map.borrow().get(key).map(|(_, v)| v.clone())
    }

    fn put(&self, key: &str, body: &str) {
        let mut tick = self.tick.borrow_mut();
        *tick += 1;
        let mut map = self.map.borrow_mut();
        map.insert(key.to_string(), (*tick, body.to_string()));
        if map.len() > MAX {
            if let Some(oldest) = map.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| k.clone()) {
                map.remove(&oldest);
            }
        }
    }

    fn clear(&self) {
        self.map.borrow_mut().clear();
    }
}

thread_local! {
    static CACHE: Box<dyn ResponseCache> = Box::new(MemoryCache::default());
}

pub fn get(key: &str) -> Option<String> {
    CACHE.with(|c| c.get(key))
}

pub fn put(key: &str, body: &str) {
    CACHE.with(|c| c.put(key, body))
}

pub fn clear() {
    CACHE.with(|c| c.clear())
}
