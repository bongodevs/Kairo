use std::fmt;

use reqwest::{ClientBuilder, Proxy};
use serde::{Deserialize, Serialize};

/// Optional outbound proxy shared by every source that makes HTTP requests.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfig {
    pub url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

impl ProxyConfig {
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into(), username: None, password: None }
    }

    #[must_use]
    pub fn with_basic_auth(
        mut self,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        self.username = Some(username.into());
        self.password = Some(password.into());
        self
    }

    /// Attach this proxy to a client builder. The caller keeps its own error type.
    pub fn apply(&self, builder: ClientBuilder) -> Result<ClientBuilder, reqwest::Error> {
        let mut proxy = Proxy::all(&self.url)?;
        if let Some(username) = &self.username {
            proxy = proxy.basic_auth(username, self.password.as_deref().unwrap_or_default());
        }
        Ok(builder.proxy(proxy))
    }
}

impl fmt::Debug for ProxyConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProxyConfig")
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}
