//! Track position markers and lifecycle tracking.
//!
//! Track markers allow callers to place callbacks at specific playback timeline positions.
//! Handlers are guaranteed to be called at most once, and will receive one of the 7
//! [`MarkerState`] transitions depending on whether the marker was reached normally,
//! bypassed by a seek, overtaken before registration (`Late`), removed, overwritten,
//! or cut short by track stop or natural end.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::decode::Millis;

/// Monotonically increasing identifier for registered track markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MarkerId(pub u64);

fn next_marker_id() -> MarkerId {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    MarkerId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

/// Lifecycle state delivered to a marker handler when triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkerState {
    /// The marked position was reached during normal playback.
    Reached,
    /// The marker was removed explicitly or cleared.
    Removed,
    /// The marker was overwritten by a new non-null marker via `set_marker`.
    Overwritten,
    /// A seek jumped over the marked position.
    Bypassed,
    /// Playback was stopped before reaching the marker.
    Stopped,
    /// The playback position had already passed the marker position when registered.
    Late,
    /// The track ended (finished or failed) without reaching the marker.
    Ended,
}

/// Handler callback invoked when a marker's lifecycle state transitions.
pub trait TrackMarkerHandler: Send + Sync + 'static {
    fn handle(&self, state: MarkerState);
}

impl<F> TrackMarkerHandler for F
where
    F: Fn(MarkerState) + Send + Sync + 'static,
{
    fn handle(&self, state: MarkerState) {
        self(state);
    }
}

/// A track position marker with an associated handler.
#[derive(Clone)]
pub struct TrackMarker {
    id: MarkerId,
    position: Millis,
    handler: Arc<dyn TrackMarkerHandler>,
    triggered: Arc<AtomicBool>,
}

impl TrackMarker {
    /// Create a new marker at the given timeline position with a callback.
    pub fn new(position: Millis, handler: impl Fn(MarkerState) + Send + Sync + 'static) -> Self {
        Self {
            id: next_marker_id(),
            position,
            handler: Arc::new(handler),
            triggered: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a marker using an existing [`TrackMarkerHandler`].
    pub fn with_handler(position: Millis, handler: Arc<dyn TrackMarkerHandler>) -> Self {
        Self {
            id: next_marker_id(),
            position,
            handler,
            triggered: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Convenience constructor creating a marker paired with a oneshot receiver for the outcome.
    pub fn channel(position: Millis) -> (Self, tokio::sync::oneshot::Receiver<MarkerState>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = Mutex::new(Some(tx));
        let marker = Self::new(position, move |state| {
            if let Ok(mut lock) = tx.lock()
                && let Some(sender) = lock.take()
            {
                let _ = sender.send(state);
            }
        });
        (marker, rx)
    }

    #[must_use]
    pub const fn id(&self) -> MarkerId {
        self.id
    }

    #[must_use]
    pub const fn position(&self) -> Millis {
        self.position
    }

    pub(crate) fn trigger(&self, state: MarkerState) -> bool {
        if !self.triggered.swap(true, Ordering::AcqRel) {
            self.handler.handle(state);
            true
        } else {
            false
        }
    }
}

impl std::fmt::Debug for TrackMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackMarker")
            .field("id", &self.id)
            .field("position", &self.position)
            .finish_non_exhaustive()
    }
}

impl PartialEq for TrackMarker {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for TrackMarker {}

impl std::hash::Hash for TrackMarker {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// Tracks registered markers and evaluates their transitions during playback, seeking, and termination.
#[derive(Debug, Default)]
pub struct TrackMarkerTracker {
    count: AtomicUsize,
    markers: Mutex<Vec<TrackMarker>>,
}

impl TrackMarkerTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn has_markers(&self) -> bool {
        self.count.load(Ordering::Acquire) > 0
    }

    /// Set a single marker, overwriting any previously registered markers.
    /// If `marker` is `None`, previously registered markers are triggered with `MarkerState::Removed`.
    /// If `marker` is `Some`, existing markers receive `MarkerState::Overwritten`.
    pub fn set(&self, marker: Option<TrackMarker>, current_time: Millis) {
        let old = {
            let mut guard = self.markers.lock().unwrap_or_else(|e| e.into_inner());
            let old = std::mem::take(&mut *guard);
            if let Some(ref m) = marker {
                if current_time >= m.position {
                    // Marker is late; do not register
                    self.count.store(0, Ordering::Release);
                } else {
                    guard.push(m.clone());
                    self.count.store(1, Ordering::Release);
                }
            } else {
                self.count.store(0, Ordering::Release);
            }
            old
        };

        let state = if marker.is_some() { MarkerState::Overwritten } else { MarkerState::Removed };
        for m in old {
            m.trigger(state);
        }

        if let Some(m) = marker
            && current_time >= m.position
        {
            m.trigger(MarkerState::Late);
        }
    }

    /// Register an additional marker without clearing existing ones.
    pub fn add(&self, marker: TrackMarker, current_time: Millis) {
        if current_time >= marker.position {
            marker.trigger(MarkerState::Late);
            return;
        }

        let mut guard = self.markers.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(marker);
        self.count.store(guard.len(), Ordering::Release);
    }

    /// Remove a marker by ID, triggering `MarkerState::Removed`.
    pub fn remove(&self, id: MarkerId) -> bool {
        let removed = {
            let mut guard = self.markers.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(pos) = guard.iter().position(|m| m.id == id) {
                let m = guard.remove(pos);
                self.count.store(guard.len(), Ordering::Release);
                Some(m)
            } else {
                None
            }
        };

        if let Some(m) = removed {
            m.trigger(MarkerState::Removed);
            true
        } else {
            false
        }
    }

    /// Clear all registered markers without triggering them.
    pub fn clear(&self) {
        let mut guard = self.markers.lock().unwrap_or_else(|e| e.into_inner());
        guard.clear();
        self.count.store(0, Ordering::Release);
    }

    /// Check if current playback position reached any markers.
    pub fn check_playback(&self, current_time: Millis) {
        if !self.has_markers() {
            return;
        }

        let mut triggered = Vec::new();
        {
            let mut guard = self.markers.lock().unwrap_or_else(|e| e.into_inner());
            let mut i = 0;
            while i < guard.len() {
                if current_time >= guard[i].position {
                    triggered.push(guard.remove(i));
                } else {
                    i += 1;
                }
            }
            self.count.store(guard.len(), Ordering::Release);
        }

        for m in triggered {
            m.trigger(MarkerState::Reached);
        }
    }

    /// Check if a seek jumped past any markers.
    pub fn check_seek(&self, seek_target: Millis) {
        if !self.has_markers() {
            return;
        }

        let mut triggered = Vec::new();
        {
            let mut guard = self.markers.lock().unwrap_or_else(|e| e.into_inner());
            let mut i = 0;
            while i < guard.len() {
                if seek_target >= guard[i].position {
                    triggered.push(guard.remove(i));
                } else {
                    i += 1;
                }
            }
            self.count.store(guard.len(), Ordering::Release);
        }

        for m in triggered {
            m.trigger(MarkerState::Bypassed);
        }
    }

    /// Trigger and remove all registered markers with the given state.
    pub fn trigger_all(&self, state: MarkerState) {
        if !self.has_markers() {
            return;
        }

        let triggered = {
            let mut guard = self.markers.lock().unwrap_or_else(|e| e.into_inner());
            let list = std::mem::take(&mut *guard);
            self.count.store(0, Ordering::Release);
            list
        };

        for m in triggered {
            m.trigger(state);
        }
    }
}
