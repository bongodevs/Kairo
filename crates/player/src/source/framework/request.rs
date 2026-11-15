use reqwest::Url;

/// Identifiers longer than this are not claimed, so a malformed request cannot force work.
pub const MAX_IDENTIFIER_BYTES: usize = 16 * 1024;
/// Search text longer than this is rejected before a provider sees it.
pub const MAX_QUERY_BYTES: usize = 8 * 1024;
/// Item identifiers longer than this are rejected before a provider sees it.
pub const MAX_ID_BYTES: usize = 512;

/// The kind of item a route returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Track,
    Album,
    Playlist,
    Artist,
    Recommendations,
}

impl Kind {
    /// The plugin ABI spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Album => "album",
            Self::Playlist => "playlist",
            Self::Artist => "artist",
            Self::Recommendations => "recommendations",
        }
    }
}

/// A request the framework has already claimed and parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// A prefixed search such as `jss:demo`.
    Search(String),
    /// One item identified by a URL route or an item prefix.
    Item(Kind, String),
}

#[derive(Debug, Clone)]
enum Segment {
    Literal(String),
    Capture { name: String },
}

#[derive(Debug, Clone)]
pub(crate) struct Route {
    host: String,
    segments: Vec<Segment>,
    kind: Kind,
}

impl Route {
    pub(crate) fn new(host: &str, template: &str, kind: Kind) -> Self {
        let segments = template
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(|segment| {
                segment.strip_prefix('{').and_then(|segment| segment.strip_suffix('}')).map_or_else(
                    || Segment::Literal(segment.to_owned()),
                    |name| Segment::Capture { name: name.to_owned() },
                )
            })
            .collect();
        Self { host: host.to_owned(), segments, kind }
    }

    fn parse(&self, url: &Url) -> Option<Request> {
        let host = url.host_str()?;
        if !host.eq_ignore_ascii_case(&self.host) {
            return None;
        }
        let path: Vec<&str> = url.path_segments()?.filter(|segment| !segment.is_empty()).collect();
        if path.len() != self.segments.len() {
            return None;
        }
        let mut id = None;
        for (segment, value) in self.segments.iter().zip(path) {
            match segment {
                Segment::Literal(literal) if literal != value => return None,
                Segment::Literal(_) => {}
                Segment::Capture { name } => {
                    if value.is_empty() {
                        return None;
                    }
                    if name == "id" {
                        id = Some(value.to_owned());
                    }
                }
            }
        }
        let id = id?;
        (!id.is_empty() && id.len() <= MAX_ID_BYTES).then_some(Request::Item(self.kind, id))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrefixTarget {
    Search,
    Item(Kind),
}

#[derive(Debug, Clone)]
pub(crate) struct PrefixRoute {
    prefix: String,
    target: PrefixTarget,
}

impl PrefixRoute {
    pub(crate) fn new(prefix: &str, target: PrefixTarget) -> Self {
        Self { prefix: prefix.to_owned(), target }
    }

    fn parse(&self, identifier: &str) -> Option<Request> {
        let rest = identifier.strip_prefix(&self.prefix)?;
        if rest.is_empty() {
            return None;
        }
        match self.target {
            PrefixTarget::Search => {
                if rest.len() > MAX_QUERY_BYTES {
                    return None;
                }
                Some(Request::Search(rest.to_owned()))
            }
            PrefixTarget::Item(kind) => {
                if rest.len() > MAX_ID_BYTES {
                    return None;
                }
                Some(Request::Item(kind, rest.to_owned()))
            }
        }
    }

    pub(crate) fn owns_search(&self, query: &str) -> bool {
        matches!(self.target, PrefixTarget::Search) && query.starts_with(&self.prefix)
    }
}

pub(crate) fn parse(
    prefixes: &[PrefixRoute],
    routes: &[Route],
    identifier: &str,
) -> Option<Request> {
    if identifier.len() > MAX_IDENTIFIER_BYTES {
        return None;
    }
    if let Some(request) = prefixes.iter().find_map(|prefix| prefix.parse(identifier)) {
        return Some(request);
    }
    let url = Url::parse(identifier).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    routes.iter().find_map(|route| route.parse(&url))
}
