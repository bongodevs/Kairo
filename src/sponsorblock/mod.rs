use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use tracing::error;

use player::tools::http::{HttpInterface, HttpResponse};
use player::tools::json::JsonBrowser;

const SPONSORBLOCK_URL: &str = "https://sponsor.ajay.app/api/skipSegments";

/// How many videos one session keeps SponsorBlock results for. A session cache only has to cover
/// restarts and replays; the bound keeps a long-lived session's memory flat.
const CACHE_CAPACITY: usize = 128;

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(char::from(b"0123456789ABCDEF"[(b >> 4) as usize]));
                out.push(char::from(b"0123456789ABCDEF"[(b & 0x0F) as usize]));
            }
        }
    }
    out
}

pub use crate::protocol::sponsorblock::{Chapter, Segment};

/// A segment or chapter attached to a track position.
#[derive(Debug, Clone)]
pub enum TrackMarkable {
    Segment(Segment),
    Chapter(Chapter),
}

impl TrackMarkable {
    pub fn start(&self) -> u64 {
        match self {
            TrackMarkable::Segment(s) => s.start,
            TrackMarkable::Chapter(c) => c.start,
        }
    }
}

/// Session-scoped SponsorBlock results, keyed by video and the guild's category set. Segments are
/// filtered by categories server-side, so two guilds with different sets need separate entries.
#[derive(Default)]
pub struct MarkableCache {
    entries: HashMap<(String, Vec<String>), Arc<Vec<TrackMarkable>>>,
    inserted: VecDeque<(String, Vec<String>)>,
}

impl MarkableCache {
    fn key(video_id: &str, categories: &HashSet<String>) -> (String, Vec<String>) {
        let mut categories: Vec<String> = categories.iter().cloned().collect();
        categories.sort_unstable();
        (video_id.to_string(), categories)
    }

    /// The cached result for this video and category set, if one was stored in this session.
    pub fn get(
        &self,
        video_id: &str,
        categories: &HashSet<String>,
    ) -> Option<Arc<Vec<TrackMarkable>>> {
        self.entries.get(&Self::key(video_id, categories)).cloned()
    }

    /// Store a result, evicting the oldest entry once the cache is full.
    pub fn insert(
        &mut self,
        video_id: &str,
        categories: &HashSet<String>,
        markables: Arc<Vec<TrackMarkable>>,
    ) {
        let key = Self::key(video_id, categories);
        if self.entries.insert(key.clone(), markables).is_none() {
            self.inserted.push_back(key);
        }
        while self.entries.len() > CACHE_CAPACITY {
            let Some(oldest) = self.inserted.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

/// Fetch the skip segments for a video, with times in milliseconds.
pub fn retrieve_video_segments(
    http: &HttpInterface,
    video_id: &str,
    categories: &HashSet<String>,
) -> Vec<Segment> {
    let categories_json = format!(
        "[{}]",
        categories
            .iter()
            .map(|c| format!("\"{}\"", c))
            .collect::<Vec<_>>()
            .join(",")
    );
    let encoded = url_encode(&categories_json);
    let url = format!(
        "{}?videoID={}&categories={}",
        SPONSORBLOCK_URL, video_id, encoded
    );

    let response = http.get(&url, &[]);
    match read_json(response, "SponsorBlock segments") {
        Some(json) => parse_segments(&json),
        None => Vec::new(),
    }
}

// A missing or unreadable answer means no segments, so every failure is logged and swallowed.
fn read_json<E: std::fmt::Display>(
    response: Result<HttpResponse, E>,
    what: &str,
) -> Option<JsonBrowser> {
    let response = match response {
        Ok(response) => response,
        Err(cause) => {
            error!("Failed to fetch {what}: {cause}");
            return None;
        }
    };
    if !response.is_success() {
        return None;
    }
    match response.json() {
        Ok(json) => Some(json),
        Err(cause) => {
            error!("Failed to parse {what} response: {cause}");
            None
        }
    }
}

fn parse_segments(json: &JsonBrowser) -> Vec<Segment> {
    json.values()
        .into_iter()
        .filter_map(|entry| {
            let category = entry.get("category").text()?;
            let segment_arr = entry.get("segment");
            let start = segment_arr.index(0).as_f64()?;
            let end = segment_arr.index(1).as_f64()?;
            Some(Segment {
                category,
                start: (start * 1000.0) as u64,
                end: (end * 1000.0) as u64,
            })
        })
        .collect()
}

/// Fetch the chapters of a video through the InnerTube `/youtubei/v1/search` API.
pub fn retrieve_video_chapters(
    http: &HttpInterface,
    video_id: &str,
    track_duration_ms: u64,
) -> Vec<Chapter> {
    let body = serde_json::json!({
        "context": {
            "client": {
                "clientName": "WEB",
                "clientVersion": "2.20220502.01.00",
                "hl": "en",
                "gl": "US"
            }
        },
        "query": video_id
    });

    let response = http.post_json(
        "https://www.youtube.com/youtubei/v1/search?prettyPrint=false",
        &[("Referer", "https://www.youtube.com")],
        body.to_string(),
    );

    let Some(json) = read_json(response, "InnerTube chapters") else {
        return Vec::new();
    };
    let Some(video_renderer) = find_video_renderer(&json, video_id) else {
        return Vec::new();
    };

    parse_chapters(&video_renderer, track_duration_ms)
}

fn find_video_renderer(json: &JsonBrowser, video_id: &str) -> Option<JsonBrowser> {
    let contents = json
        .get("contents")
        .get("twoColumnSearchResultsRenderer")
        .get("primaryContents")
        .get("sectionListRenderer")
        .get("contents");

    for section in contents.values() {
        let items = section.get("itemSectionRenderer").get("contents");
        for item in items.values() {
            let renderer = item.get("videoRenderer");
            if renderer.get("videoId").text().as_deref() == Some(video_id) {
                return Some(renderer);
            }
        }
    }
    None
}

fn parse_chapters(video_renderer: &JsonBrowser, track_duration_ms: u64) -> Vec<Chapter> {
    let cards = video_renderer
        .get("expandableMetadata")
        .get("expandableMetadataRenderer")
        .get("expandedContent")
        .get("horizontalCardListRenderer")
        .get("cards");

    let card_list = cards.values();
    if card_list.is_empty() {
        return Vec::new();
    }

    let mut chapters = Vec::new();
    for (i, card) in card_list.iter().enumerate() {
        let renderer = card.get("macroMarkersListItemRenderer");
        let name = join_runs(&renderer.get("title"));
        let start = parse_duration_text(&join_runs(&renderer.get("timeDescription")));

        let end = if i + 1 < card_list.len() {
            let next = &card_list[i + 1];
            let next_renderer = next.get("macroMarkersListItemRenderer");
            parse_duration_text(&join_runs(&next_renderer.get("timeDescription")))
        } else {
            track_duration_ms
        };

        chapters.push(Chapter {
            name,
            start,
            end,
            duration: end.saturating_sub(start),
        });
    }
    chapters
}

fn join_runs(text_block: &JsonBrowser) -> String {
    text_block
        .get("runs")
        .values()
        .into_iter()
        .filter_map(|run| run.get("text").text())
        .collect::<Vec<_>>()
        .join("")
}

// Timestamps come as "1:23", "1:02:34" or "1:02:03:04" (d:h:m:s), so the fields are read right to
// left to give each one its own unit.
fn parse_duration_text(text: &str) -> u64 {
    const UNITS: [u64; 4] = [1, 60, 60 * 60, 24 * 60 * 60];

    let seconds: u64 = text
        .rsplit(':')
        .zip(UNITS)
        .map(|(part, unit)| part.trim().parse::<u64>().unwrap_or(0) * unit)
        .sum();
    seconds * 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_text_scales_each_field_by_its_own_unit() {
        assert_eq!(parse_duration_text("45"), 45_000);
        assert_eq!(parse_duration_text("1:23"), 83_000);
        assert_eq!(parse_duration_text("1:02:34"), 3_754_000);
        // 2 d + 3 h + 4 m + 5 s.
        assert_eq!(parse_duration_text("2:03:04:05"), 183_845_000);
        assert_eq!(parse_duration_text("nonsense"), 0);
    }

    fn chapter(name: &str, start: u64) -> Arc<Vec<TrackMarkable>> {
        Arc::new(vec![TrackMarkable::Chapter(Chapter {
            name: name.to_string(),
            start,
            end: start + 1000,
            duration: 1000,
        })])
    }

    #[test]
    fn cache_hits_match_on_video_and_category_set() {
        let mut cache = MarkableCache::default();
        let music: HashSet<String> = ["music_offtopic".to_string()].into();
        let sponsor: HashSet<String> = ["sponsor".to_string()].into();

        cache.insert("video", &music, chapter("music", 0));

        assert!(cache.get("video", &music).is_some());
        assert!(cache.get("video", &sponsor).is_none());
        assert!(cache.get("other", &music).is_none());
    }

    #[test]
    fn cache_is_bounded_and_evicts_the_oldest_entry() {
        let mut cache = MarkableCache::default();
        let categories = HashSet::new();
        for i in 0..=CACHE_CAPACITY {
            let video = format!("video-{i}");
            cache.insert(&video, &categories, chapter(&video, i as u64));
        }

        assert!(cache.get("video-0", &categories).is_none());
        assert!(cache
            .get(&format!("video-{CACHE_CAPACITY}"), &categories)
            .is_some());
    }
}
