use std::collections::BTreeMap;

use kairo_plugin_api::{
    HostCallbacks, HostFuture, HttpMethod, HttpRequest, HttpResponse, LogLevel, PluginError,
};
use reqwest::header::{HeaderName, HeaderValue};
use reqwest::redirect::Policy;

const MAX_HTTP_BODY: usize = 8 * 1024 * 1024;

pub(super) struct PluginHost {
    name: String,
    client: reqwest::Client,
}

impl PluginHost {
    pub(super) fn new(name: String, client: reqwest::Client) -> Self {
        Self { name, client }
    }
}

impl HostCallbacks for PluginHost {
    fn log(&self, level: LogLevel, message: &str) {
        match level {
            LogLevel::Trace => tracing::trace!(plugin = %self.name, message),
            LogLevel::Debug => tracing::debug!(plugin = %self.name, message),
            LogLevel::Info => tracing::info!(plugin = %self.name, message),
            LogLevel::Warn => tracing::warn!(plugin = %self.name, message),
            LogLevel::Error => tracing::error!(plugin = %self.name, message),
        }
    }

    fn http<'a>(
        &'a self,
        request: HttpRequest,
    ) -> HostFuture<'a, Result<HttpResponse, PluginError>> {
        Box::pin(send(&self.client, request))
    }
}

pub(super) fn client() -> Result<reqwest::Client, reqwest::Error> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .timeout(kairo_core::http::REQUEST_TIMEOUT)
        .redirect(Policy::custom(|attempt| {
            let scheme = attempt.url().scheme();
            if !matches!(scheme, "http" | "https") {
                return attempt.error("plugin HTTP redirect uses an unsupported scheme");
            }
            if attempt.previous().last().is_some_and(|previous| previous.scheme() != scheme) {
                return attempt.error("plugin HTTP redirect changed scheme");
            }
            if attempt.previous().len() >= 10 {
                attempt.error("plugin HTTP redirect limit exceeded")
            } else {
                attempt.follow()
            }
        }))
        .user_agent(kairo_core::http::USER_AGENT)
        .build()
}

async fn send(client: &reqwest::Client, request: HttpRequest) -> Result<HttpResponse, PluginError> {
    let url = reqwest::Url::parse(&request.url)
        .map_err(|error| PluginError::common(format!("invalid HTTP URL: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(PluginError::common("host HTTP supports only http and https"));
    }
    let method = method(request.method)?;
    let mut call = client.request(method, url);
    for (name, value) in request.headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| PluginError::common(format!("invalid HTTP header name: {error}")))?;
        let value = HeaderValue::from_str(&value)
            .map_err(|error| PluginError::common(format!("invalid HTTP header value: {error}")))?;
        call = call.header(name, value);
    }
    if let Some(body) = request.body {
        call = call.body(body);
    }
    let mut response = call
        .send()
        .await
        .map_err(|error| PluginError::unavailable(format!("host HTTP request failed: {error}")))?;
    let too_large =
        || PluginError::fault(format!("host HTTP response exceeds {MAX_HTTP_BODY} bytes"));
    if response.content_length().is_some_and(|length| length > MAX_HTTP_BODY as u64) {
        return Err(too_large());
    }
    let status = response.status().as_u16();
    let headers = response_headers(response.headers())?;
    let mut body =
        Vec::with_capacity(response.content_length().map_or(0, |length| length as usize));
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| PluginError::unavailable(format!("host HTTP body failed: {error}")))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_HTTP_BODY {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    let body = String::from_utf8(body)
        .map_err(|_| PluginError::suspicious("host HTTP response body is not UTF-8"))?;
    Ok(HttpResponse { status, headers, body })
}

fn method(method: HttpMethod) -> Result<reqwest::Method, PluginError> {
    match method {
        HttpMethod::Delete => Ok(reqwest::Method::DELETE),
        HttpMethod::Get => Ok(reqwest::Method::GET),
        HttpMethod::Head => Ok(reqwest::Method::HEAD),
        HttpMethod::Patch => Ok(reqwest::Method::PATCH),
        HttpMethod::Post => Ok(reqwest::Method::POST),
        HttpMethod::Put => Ok(reqwest::Method::PUT),
        _ => Err(PluginError::common("unsupported HTTP method")),
    }
}

fn response_headers(
    headers: &reqwest::header::HeaderMap,
) -> Result<BTreeMap<String, String>, PluginError> {
    headers
        .iter()
        .map(|(name, value)| {
            let value = value
                .to_str()
                .map_err(|_| PluginError::suspicious("host HTTP response header is not UTF-8"))?;
            Ok((name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

#[cfg(test)]
#[path = "../tests/plugins_host_api.rs"]
mod tests;
