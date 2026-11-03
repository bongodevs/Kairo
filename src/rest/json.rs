//! A JSON body extractor with the v4 error status.
//!
//! Axum's `Json` rejects a wrong-typed body with `422 Unprocessable Entity`. The protocol expects a
//! `400` and a message that names the offending field, so this wrapper keeps axum's text and changes
//! only the status.

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use serde::de::DeserializeOwned;

use super::error::RestError;

/// A deserialized JSON body, or a [`RestError`] the error middleware renders as the v4 body.
pub struct JsonBody<T>(pub T);

impl<T, S> FromRequest<S> for JsonBody<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = RestError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(request, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) => {
                // A typed-but-wrong or malformed body is a bad request. A missing content type or a
                // transport failure keeps axum's own status.
                let status = match &rejection {
                    JsonRejection::JsonDataError(_) | JsonRejection::JsonSyntaxError(_) => {
                        StatusCode::BAD_REQUEST
                    }
                    _ => rejection.status(),
                };
                Err(RestError::new(status, rejection.body_text()))
            }
        }
    }
}
