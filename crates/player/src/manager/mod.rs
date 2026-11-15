//! Registry shared by hosts that keep one live player per external identifier.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, MutexGuard};

/// A thread-safe map of live players keyed by an external identifier.
///
/// The registry only stores handles. Creating the player and wiring its transport stays with the
/// host, so this type works for guilds, channels, or any other stable key.
pub struct PlayerRegistry<K, V> {
    entries: Mutex<HashMap<K, Arc<V>>>,
}

impl<K, V> Default for PlayerRegistry<K, V> {
    fn default() -> Self {
        Self { entries: Mutex::new(HashMap::new()) }
    }
}

impl<K: Eq + Hash + Copy, V> PlayerRegistry<K, V> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn get(&self, key: K) -> Option<Arc<V>> {
        self.lock().get(&key).cloned()
    }

    #[must_use]
    pub fn list(&self) -> Vec<Arc<V>> {
        self.lock().values().cloned().collect()
    }

    /// Return the existing player for `key`, or create and store one.
    pub fn get_or_try_insert_with<E>(
        &self,
        key: K,
        create: impl FnOnce() -> Result<Arc<V>, E>,
    ) -> Result<Arc<V>, E> {
        let mut entries = self.lock();
        if let Some(player) = entries.get(&key) {
            return Ok(Arc::clone(player));
        }
        let player = create()?;
        entries.insert(key, Arc::clone(&player));
        Ok(player)
    }

    pub fn remove(&self, key: K) -> Option<Arc<V>> {
        self.lock().remove(&key)
    }

    pub fn drain(&self) -> Vec<Arc<V>> {
        self.lock().drain().map(|(_, player)| player).collect()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<K, Arc<V>>> {
        self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
