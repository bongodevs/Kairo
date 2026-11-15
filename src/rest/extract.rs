//! Extractor wrappers that report framework rejections with stable HTTP reasons.
//!
//! Axum's own rejection bodies are implementation text ("Failed to deserialize the JSON body
//! into the target type: ..."), which changes with the framework and never matches Spring's
//! exception wording. A rejection therefore becomes the mapped status plus its canonical reason;
//! handler-authored messages pass through untouched.

use axum::Json;
use axum::extract::{FromRequest, FromRequestParts, Path, Query};
use axum::http::StatusCode;
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

pub(crate) struct ApiJson<T>(pub(crate) T);

pub(crate) struct ApiQuery<T>(pub(crate) T);

pub(crate) struct ApiPath<T>(pub(crate) T);

fn canonical(status: StatusCode) -> (StatusCode, &'static str) {
    let status =
        if status == StatusCode::UNPROCESSABLE_ENTITY { StatusCode::BAD_REQUEST } else { status };
    (status, status.canonical_reason().unwrap_or("Unknown Error"))
}

impl<T, S> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request(
        request: axum::extract::Request,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(request, state)
            .await
            .map_err(|rejection| canonical(rejection.status()))?;
        Ok(Self(value))
    }
}

impl<T, S> FromRequestParts<S> for ApiQuery<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(value) = Query::<T>::from_request_parts(parts, state)
            .await
            .map_err(|rejection| canonical(rejection.status()))?;
        Ok(Self(value))
    }
}

impl<T, S> FromRequestParts<S> for ApiPath<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Path(value) = Path::<T>::from_request_parts(parts, state)
            .await
            .map_err(|rejection| canonical(rejection.status()))?;
        Ok(Self(value))
    }
}
