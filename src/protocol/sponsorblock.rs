//! SponsorBlock skip segments and chapter DTOs for wire events.

use serde::Serialize;

/// A SponsorBlock segment to skip.
#[derive(Debug, Clone, Serialize)]
pub struct Segment {
    pub category: String,
    pub start: u64,
    pub end: u64,
}

/// A video chapter.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Chapter {
    pub name: String,
    pub start: u64,
    pub end: u64,
    pub duration: u64,
}
