// ABOUTME: Pins the HTTP classification of scraper errors: busy=503+Retry-After, dead session=401, rest=500
// ABOUTME: A 401 session marker is what lets the platform hand the athlete a reconnect link instead of paging an operator
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use axum::body::to_bytes;
use axum::http::{header, StatusCode};
use dravr_sciotte::error::ScraperError;
use dravr_sciotte_server::error_response::scraper_error_response;
use serde_json::Value;

/// Collapse a response into (status, parsed JSON body).
async fn parts(error: &ScraperError) -> (StatusCode, Value) {
    let response = scraper_error_response(error);
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body collects");
    let body = serde_json::from_slice(&bytes).expect("body is JSON");
    (status, body)
}

#[tokio::test]
async fn a_dead_session_answers_401_with_the_session_marker() {
    for error in [
        ScraperError::SessionExpired {
            reason: "strava redirected to /login".to_owned(),
        },
        ScraperError::Auth {
            reason: "cookie jar rejected".to_owned(),
        },
    ] {
        let (status, body) = parts(&error).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("session_expired"),
            "the platform matches this marker to trigger the athlete's reconnect flow"
        );
    }
}

#[tokio::test]
async fn a_busy_shed_keeps_its_503_and_retry_after() {
    let error = ScraperError::Busy {
        reason: "queue full".to_owned(),
        retry_after_secs: 30,
    };
    let response = scraper_error_response(&error);
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("30")
    );
}

#[tokio::test]
async fn a_genuine_fault_stays_500() {
    let (status, body) = parts(&ScraperError::Browser {
        reason: "chrome crashed".to_owned(),
    })
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_ne!(
        body.get("error").and_then(Value::as_str),
        Some("session_expired"),
        "a browser fault must not send the athlete to re-login"
    );
}
