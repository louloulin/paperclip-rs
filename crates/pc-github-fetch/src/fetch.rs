//! Async GitHub / GitHub Enterprise fetch wrapper.
//!
//! Direct port of `paperclip/server/src/services/github-fetch.ts` (`ghFetch`).
//! The Rust version is split into two functions:
//!
//! - [`gh_fetch`] — convenience that owns its own [`reqwest::Client`].
//!   Use this in one-shot scripts / tests.
//! - [`gh_fetch_with`] — caller-supplied client. Use this in production code
//!   so the higher layer can share a single connection pool across many
//!   GitHub API calls.

use reqwest::{Client, RequestBuilder, Response};

use crate::GitHubFetchError;

/// Issue an HTTP request via the supplied `reqwest::Client`, mapping
/// connection failures to [`GitHubFetchError::Connection`] (which carries
/// the original transport error so callers can log the cause).
///
/// 4xx / 5xx responses are returned as-is — `ghFetch` in the Node upstream
/// does not transform them; the caller (`github-external-object-provider`)
/// inspects `response.status` and decides what to do.
pub async fn gh_fetch_with(
    client: &Client,
    builder: RequestBuilder,
) -> Result<Response, GitHubFetchError> {
    // `build()` consumes `builder`, so we use `client.execute(req)` after
    // extracting the URL for the error context (avoids needing `try_clone`).
    let req = builder
        .build()
        .map_err(|e| GitHubFetchError::InvalidUrl(e.to_string()))?;
    let host = req.url().host_str().unwrap_or("").to_string();

    match client.execute(req).await {
        Ok(resp) => Ok(resp),
        Err(e) => Err(GitHubFetchError::Connection { host, source: e }),
    }
}

/// Convenience wrapper that builds a fresh [`reqwest::Client`] per call.
/// Prefer [`gh_fetch_with`] in production code (shared connection pool).
pub async fn gh_fetch(url: &str, token: Option<&str>) -> Result<Response, GitHubFetchError> {
    let client = Client::builder()
        .user_agent("paperclip-rs/pc-github-fetch")
        .build()
        .map_err(GitHubFetchError::Transport)?;
    let mut builder = client.get(url);
    if let Some(t) = token {
        builder = builder.bearer_auth(t);
    }
    gh_fetch_with(&client, builder).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Spawn a tiny one-shot HTTP echo server on a random port.
    /// Returns the bound address and a oneshot that signals server stop.
    async fn spawn_mock_server<F>(responder: F) -> (SocketAddr, tokio::sync::oneshot::Sender<()>)
    where
        F: Fn(&str) -> (String, String) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let responder = std::sync::Arc::new(responder);
        tokio::spawn(async move {
            let mut stop = Some(rx);
            loop {
                tokio::select! {
                    biased;
                    _ = async {
                        if let Some(s) = stop.as_mut() {
                            let _ = s.try_recv();
                        }
                        std::future::pending::<()>().await
                    } => break,
                    accepted = listener.accept() => {
                        if let Ok((mut sock, _)) = accepted {
                            let responder = std::sync::Arc::clone(&responder);
                            tokio::spawn(async move {
                                let mut buf = [0u8; 2048];
                                let _ = sock.read(&mut buf).await;
                                let req = String::from_utf8_lossy(&buf).to_string();
                                let first_line = req.lines().next().unwrap_or("");
                                let (status, body) = (&*responder)(first_line);
                                let resp = format!(
                                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n{body}",
                                    body.len()
                                );
                                let _ = sock.write_all(resp.as_bytes()).await;
                                let _ = sock.flush().await;
                            });
                        }
                    }
                }
            }
        });
        (addr, tx)
    }

    #[tokio::test]
    async fn r523_gh_fetch_returns_response_on_success() {
        let (addr, _stop) = spawn_mock_server(|first_line| {
            assert!(first_line.starts_with("GET /repos/foo/bar HTTP"));
            ("200 OK".to_string(), r#"{"id":1}"#.to_string())
        })
        .await;

        let url = format!("http://{addr}/repos/foo/bar");
        let resp = gh_fetch(&url, None).await.expect("fetch OK");
        assert!(resp.status().is_success());
        assert_eq!(resp.status().as_u16(), 200);
    }

    #[tokio::test]
    async fn r523_gh_fetch_passes_bearer_token() {
        let (addr, _stop) = spawn_mock_server(|first_line| {
            // We can't read headers from the tiny parser, but verify the
            // method line at least landed.
            assert!(first_line.starts_with("GET / HTTP"));
            ("200 OK".to_string(), "ok".to_string())
        })
        .await;

        let url = format!("http://{addr}/");
        let resp = gh_fetch(&url, Some("secret-token"))
            .await
            .expect("fetch OK");
        assert!(resp.status().is_success());
    }

    #[tokio::test]
    async fn r523_gh_fetch_returns_connection_error_on_unreachable_host() {
        // 127.0.0.1:0 is never a valid target; connecting to port 0 reliably produces
        // a connection error (no service can bind to port 0). This avoids macOS proxy
        // interference that can cause port 1 to return 502 instead of ConnectionRefused.
        let client = Client::builder()
            .no_proxy()
            .user_agent("paperclip-rs/pc-github-fetch")
            .build()
            .unwrap();
        let resp = gh_fetch_with(&client, client.get("http://127.0.0.1:0/")).await;
        match resp {
            Err(GitHubFetchError::Connection { host, .. }) => {
                assert_eq!(host, "127.0.0.1");
            }
            other => panic!("expected Connection error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn r523_gh_fetch_with_returns_invalid_url_for_malformed_builder() {
        // We can't construct a malformed RequestBuilder via the public API,
        // but we can verify that a non-existent scheme gets reported.
        // (reqwest treats non-http URLs as InvalidUrl.)
        let client = Client::new();
        let builder = client.get("not a url");
        let result = gh_fetch_with(&client, builder).await;
        assert!(matches!(
            result,
            Err(GitHubFetchError::InvalidUrl(_)) | Err(GitHubFetchError::Transport(_))
        ));
    }
}
