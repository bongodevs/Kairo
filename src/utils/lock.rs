use std::sync::{Mutex, MutexGuard, PoisonError};

// Lock a mutex, recovering from poison instead of propagating a panic.
pub fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
