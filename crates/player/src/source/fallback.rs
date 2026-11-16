use std::future::Future;

tokio::task_local! {
    static SUPPRESSED: bool;
}

/// Whether the caller wants a failed open reported instead of replaced by a fallback stream.
///
/// Sources that can generate media of their own consult this before doing so. The resume and
/// re-resolve paths set it so their retries surface the real error.
#[must_use]
pub fn fallback_suppressed() -> bool {
    SUPPRESSED.try_with(|suppressed| *suppressed).unwrap_or(false)
}

/// Run one open without allowing a source to substitute generated media for a failure.
pub async fn without_fallback<F: Future>(future: F) -> F::Output {
    SUPPRESSED.scope(true, future).await
}
