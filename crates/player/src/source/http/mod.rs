mod hls;
mod mpegts;
mod source;
mod stream;

pub use source::{HTTP_SOURCE, HttpSource};
#[cfg(test)]
pub(crate) use stream::gone_for_test;
pub(crate) use stream::source_gone;

#[cfg(test)]
#[path = "../../../tests/source_http_decode.rs"]
mod decode_tests;
#[cfg(test)]
#[path = "../../../tests/source_hls.rs"]
mod hls_tests;
#[cfg(test)]
#[path = "../../../tests/source_http.rs"]
mod tests;
