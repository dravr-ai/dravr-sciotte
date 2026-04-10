// ABOUTME: Embedded fake login server for testing without real provider interaction
// ABOUTME: Serves static HTML pages mimicking Strava, Garmin, and Google OAuth flows
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::error::Error;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::info;

/// Start a local HTTP server serving the fake login fixtures.
/// Returns the base URL (e.g., `http://127.0.0.1:12345`).
pub async fn start_fake_server() -> Result<String, Box<dyn Error + Send + Sync>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let base_url = format!("http://{addr}");

    info!(url = %base_url, "Fake login server started");

    tokio::spawn(async move {
        run_server(listener).await;
    });

    Ok(base_url)
}

async fn run_server(listener: TcpListener) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        tokio::spawn(async move {
            handle_request(stream).await;
        });
    }
}

async fn handle_request(stream: TcpStream) {
    let mut buf = vec![0u8; 4096];
    let (mut reader, mut writer) = stream.into_split();

    let n = reader.read(&mut buf).await.unwrap_or(0);
    if n == 0 {
        return;
    }

    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    let file_path = path
        .split('?')
        .next()
        .unwrap_or(path)
        .trim_start_matches('/');

    let body = get_fixture(file_path);

    let (status, content_type) = if body.is_some() {
        ("200 OK", "text/html; charset=utf-8")
    } else {
        ("404 Not Found", "text/plain")
    };

    let body = body.unwrap_or_else(|| b"Not Found".to_vec());

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );

    let _ = writer.write_all(response.as_bytes()).await;
    let _ = writer.write_all(&body).await;
}

/// Get a fixture file by path. Fixtures are embedded at compile time.
fn get_fixture(path: &str) -> Option<Vec<u8>> {
    match path {
        "strava/login.html" => Some(include_bytes!("../tests/fixtures/strava/login.html").to_vec()),
        "strava/dashboard.html" => {
            Some(include_bytes!("../tests/fixtures/strava/dashboard.html").to_vec())
        }
        "garmin/sign-in.html" => {
            Some(include_bytes!("../tests/fixtures/garmin/sign-in.html").to_vec())
        }
        "garmin/mfa.html" => Some(include_bytes!("../tests/fixtures/garmin/mfa.html").to_vec()),
        "garmin/dashboard.html" => {
            Some(include_bytes!("../tests/fixtures/garmin/dashboard.html").to_vec())
        }
        "google/identifier.html" => {
            Some(include_bytes!("../tests/fixtures/google/identifier.html").to_vec())
        }
        "google/challenge/pwd.html" => {
            Some(include_bytes!("../tests/fixtures/google/challenge/pwd.html").to_vec())
        }
        "google/challenge/pk.html" => {
            Some(include_bytes!("../tests/fixtures/google/challenge/pk.html").to_vec())
        }
        "google/challenge/selection.html" => {
            Some(include_bytes!("../tests/fixtures/google/challenge/selection.html").to_vec())
        }
        "google/challenge/totp.html" => {
            Some(include_bytes!("../tests/fixtures/google/challenge/totp.html").to_vec())
        }
        "google/challenge/number.html" => {
            Some(include_bytes!("../tests/fixtures/google/challenge/number.html").to_vec())
        }
        _ => None,
    }
}
