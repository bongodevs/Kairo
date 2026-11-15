use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::redirect::Policy;
use reqwest::{Client, Method, Response, Url};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::source::error::SourceError;
use crate::source::proxy::ProxyConfig;

const DEFAULT_MAX_BYTES: usize = 2 * 1024 * 1024;
const DEFAULT_RETRIES: u8 = 2;
const RETRY_BASE: Duration = Duration::from_millis(100);

/// A bounded JSON client shared by every provider.
///
/// Cloning is cheap: the underlying `reqwest::Client` is reference counted.
#[derive(Clone)]
pub struct Api {
    client: Client,
    base: Url,
    name: String,
    max_bytes: usize,
    retries: u8,
}

impl Api {
    pub fn new(name: impl Into<String>, base: Url) -> Result<Self, SourceError> {
        Self::with_proxy(name, base, None)
    }

    pub fn with_proxy(
        name: impl Into<String>,
        base: Url,
        proxy: Option<&ProxyConfig>,
    ) -> Result<Self, SourceError> {
        let client = crate::source::net::client(
            kairo_core::http::REQUEST_TIMEOUT,
            kairo_core::http::REQUEST_TIMEOUT,
            Policy::limited(5),
            proxy,
            None,
        )?;
        Ok(Self {
            client,
            base,
            name: name.into(),
            max_bytes: DEFAULT_MAX_BYTES,
            retries: DEFAULT_RETRIES,
        })
    }

    #[must_use]
    pub const fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    #[must_use]
    pub const fn with_retries(mut self, retries: u8) -> Self {
        self.retries = retries;
        self
    }

    /// Start a typed `GET`.
    pub fn get(&self, path: &str) -> Call<'_> {
        Call::new(self, Method::GET, path)
    }

    /// Start a typed `POST`.
    pub fn post(&self, path: &str) -> Call<'_> {
        Call::new(self, Method::POST, path)
    }

    async fn send(&self, call: Call<'_>) -> Result<Vec<u8>, SourceError> {
        let url = self.url(&call)?;
        let mut attempt = 0_u8;
        loop {
            let mut request = self.client.request(call.method.clone(), url.clone());
            for (name, value) in &call.headers {
                request = request.header(name, value);
            }
            if let Some(body) = &call.body {
                request = request.body(body.clone());
            }
            match request.send().await {
                Ok(response) => {
                    let status = response.status().as_u16();
                    if (200..300).contains(&status) {
                        return self.read(response).await;
                    }
                    if retryable(status) && attempt < self.retries {
                        attempt += 1;
                        tokio::time::sleep(backoff(attempt)).await;
                        continue;
                    }
                    return Err(SourceError::api(&self.name, status));
                }
                Err(error) => {
                    if attempt < self.retries {
                        attempt += 1;
                        tokio::time::sleep(backoff(attempt)).await;
                        continue;
                    }
                    return Err(error.into());
                }
            }
        }
    }

    fn url(&self, call: &Call<'_>) -> Result<Url, SourceError> {
        let mut url = if call.path.is_empty() {
            self.base.clone()
        } else {
            self.base.join(&call.path).map_err(|error| SourceError::fault(&self.name, error))?
        };
        if !call.query.is_empty() {
            url.query_pairs_mut().extend_pairs(
                call.query.iter().map(|(name, value)| (name.as_str(), value.as_str())),
            );
        }
        Ok(url)
    }

    async fn read(&self, mut response: Response) -> Result<Vec<u8>, SourceError> {
        if response.content_length().is_some_and(|length| length > self.max_bytes as u64) {
            return Err(SourceError::ResponseTooLarge { max: self.max_bytes });
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if body.len().saturating_add(chunk.len()) > self.max_bytes {
                return Err(SourceError::ResponseTooLarge { max: self.max_bytes });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

pub struct Call<'a> {
    api: &'a Api,
    method: Method,
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

impl<'a> Call<'a> {
    fn new(api: &'a Api, method: Method, path: &str) -> Self {
        Self {
            api,
            method,
            path: path.to_owned(),
            query: Vec::new(),
            headers: vec![(ACCEPT.as_str().to_owned(), "application/json".to_owned())],
            body: None,
        }
    }

    /// Add query parameters.
    #[must_use]
    pub fn query(mut self, parameters: &[(&str, &str)]) -> Self {
        self.query.extend(
            parameters.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
        );
        self
    }

    /// Add one request header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Send a JSON body.
    pub fn body<T>(mut self, body: &T) -> Result<Self, SourceError>
    where
        T: Serialize,
    {
        self.body = Some(serde_json::to_vec(body)?);
        self.headers.push((CONTENT_TYPE.as_str().to_owned(), "application/json".to_owned()));
        Ok(self)
    }

    /// Execute and ignore the response body.
    pub async fn send(self) -> Result<(), SourceError> {
        let _ = self.api.send(self).await?;
        Ok(())
    }

    /// Execute and decode the response body.
    pub async fn json<T>(self) -> Result<T, SourceError>
    where
        T: DeserializeOwned,
    {
        let body = self.api.send(self).await?;
        Ok(serde_json::from_slice(&body)?)
    }
}

fn retryable(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

fn backoff(attempt: u8) -> Duration {
    RETRY_BASE.saturating_mul(1_u32 << u32::from(attempt.saturating_sub(1)))
}
