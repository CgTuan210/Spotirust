//! Loopback HTTP callback server bound strictly to `127.0.0.1` for OAuth 2.0 PKCE redirects.
//!
//! Binds on `127.0.0.1:<port>` (ephemeral port when `port == 0`), ignores unrelated browser
//! probes like `/favicon.ico`, validates the OAuth CSRF `state`, responds with a clean HTML
//! status page, and terminates immediately.

use crate::auth::oauth::{OAuthError, validate_callback_params};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Bound loopback callback listener awaiting one OAuth redirect from the system browser.
pub struct LoopbackCallbackServer {
    listener: TcpListener,
    redirect_uri: String,
    expected_path: String,
}

impl LoopbackCallbackServer {
    /// Bind exclusively to `127.0.0.1` on the requested port (`0` selects an OS ephemeral port).
    pub async fn bind(port: u16, path: &str) -> Result<Self, OAuthError> {
        let normalized_path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| OAuthError::CallbackServer(format!("failed to bind {addr}: {e}")))?;

        let local_addr = listener
            .local_addr()
            .map_err(|e| OAuthError::CallbackServer(e.to_string()))?;

        let redirect_uri = format!("http://127.0.0.1:{}{}", local_addr.port(), normalized_path);
        tracing::info!(redirect_uri = %redirect_uri, "OAuth loopback callback server listening");

        Ok(Self {
            listener,
            redirect_uri,
            expected_path: normalized_path,
        })
    }

    /// The exact redirect URI to pass to Spotify's `/authorize` and `/api/token` endpoints.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Wait for a matching callback request within `timeout`, validating CSRF `state`.
    pub async fn wait_for_code(
        self,
        expected_state: &str,
        timeout: Duration,
    ) -> Result<String, OAuthError> {
        tokio::time::timeout(timeout, self.accept_loop(expected_state))
            .await
            .map_err(|_| OAuthError::TimedOut)?
    }

    async fn accept_loop(self, expected_state: &str) -> Result<String, OAuthError> {
        loop {
            let (mut stream, peer) = self
                .listener
                .accept()
                .await
                .map_err(|e| OAuthError::CallbackServer(e.to_string()))?;

            if !peer.ip().is_loopback() {
                let _ = write_http_response(
                    &mut stream,
                    "403 Forbidden",
                    "Forbidden",
                    "Non-loopback request rejected.",
                )
                .await;
                continue;
            }

            let mut buf = vec![0u8; 8192];
            let read = match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
                .await
            {
                Ok(Ok(0)) => continue,
                Ok(Ok(n)) => n,
                Ok(Err(e)) => {
                    tracing::debug!(error = %e, "Failed reading callback socket");
                    continue;
                }
                Err(_) => {
                    // Ignore idle speculative browser preconnect sockets without aborting login
                    tracing::debug!("Timed out reading idle loopback socket; continuing listener");
                    continue;
                }
            };

            let raw_request = String::from_utf8_lossy(&buf[..read]);
            let Some((path, query)) = parse_http_request_target(&raw_request) else {
                let _ = write_http_response(
                    &mut stream,
                    "400 Bad Request",
                    "Invalid Request",
                    "Malformed HTTP request.",
                )
                .await;
                continue;
            };

            // Ignore favicon or unrelated browser preflight probes
            if path != self.expected_path {
                let _ = write_http_response(
                    &mut stream,
                    "404 Not Found",
                    "Not Found",
                    "Waiting for Spotirust OAuth callback.",
                )
                .await;
                continue;
            }

            match validate_callback_params(&query, expected_state) {
                Ok(code) => {
                    let _ = write_http_response(
                        &mut stream,
                        "200 OK",
                        "Sign-in Complete",
                        "You are now authenticated with Spotify. You can close this tab and return to Spotirust.",
                    )
                    .await;
                    return Ok(code);
                }
                Err(err) => {
                    let _ = write_http_response(
                        &mut stream,
                        "400 Bad Request",
                        "Authentication Failed",
                        &format!("Spotirust could not complete sign-in: {err}. You may close this tab and retry in the app."),
                    )
                    .await;
                    return Err(err);
                }
            }
        }
    }
}

/// Extract `(path, query)` from the HTTP request line `GET /callback?code=...&state=... HTTP/1.1`.
pub fn parse_http_request_target(raw_request: &str) -> Option<(String, String)> {
    let request_line = raw_request.lines().next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?;
    if !method.eq_ignore_ascii_case("GET") {
        return None;
    }
    let target = parts.next()?;
    match target.split_once('?') {
        Some((path, query)) => Some((path.to_string(), query.to_string())),
        None => Some((target.to_string(), String::new())),
    }
}

async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    heading: &str,
    message: &str,
) -> std::io::Result<()> {
    let escaped_heading = escape_html(heading);
    let escaped_message = escape_html(message);
    let body = format!(
        "<!doctype html>\
         <html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>Spotirust — {escaped_heading}</title>\
         <style>\
         body{{margin:0;min-height:100vh;display:grid;place-items:center;\
         background:#0e1114;color:#f3f4f6;font-family:system-ui,-apple-system,sans-serif;}}\
         .card{{max-width:460px;padding:32px;border-radius:16px;background:#181c20;\
         border:1px solid #2b323a;box-shadow:0 20px 50px rgba(0,0,0,.45);text-align:center;}}\
         h1{{margin:0 0 12px;font-size:22px;color:#1ed760;}}\
         p{{margin:0;line-height:1.5;color:#cbd5e1;font-size:15px;}}\
         </style></head><body><div class=\"card\">\
         <h1>{escaped_heading}</h1><p>{escaped_message}</p>\
         </div></body></html>"
    );

    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n\
         {body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_get_target_and_query() {
        let req = "GET /callback?code=xyz&state=abc HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        let (path, query) = parse_http_request_target(req).unwrap();
        assert_eq!(path, "/callback");
        assert_eq!(query, "code=xyz&state=abc");
    }

    #[tokio::test]
    async fn loopback_server_validates_state_and_ignores_favicon() {
        let server = LoopbackCallbackServer::bind(0, "/callback").await.unwrap();
        let redirect_uri = server.redirect_uri().to_string();
        assert!(redirect_uri.starts_with("http://127.0.0.1:"));
        assert!(redirect_uri.ends_with("/callback"));

        let expected_state = "secure-random-state-123";
        let server_task = tokio::spawn(async move {
            server
                .wait_for_code(expected_state, Duration::from_secs(5))
                .await
        });

        let client = reqwest::Client::new();
        let base = redirect_uri.trim_end_matches("/callback");

        // Unrelated browser request should not abort auth
        let fav = client
            .get(format!("{base}/favicon.ico"))
            .send()
            .await
            .unwrap();
        assert_eq!(fav.status(), 404);

        // Actual OAuth redirect
        let cb = client
            .get(format!(
                "{redirect_uri}?code=test_auth_code&state={expected_state}"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(cb.status(), 200);

        let code = server_task.await.unwrap().unwrap();
        assert_eq!(code, "test_auth_code");
    }

    #[tokio::test]
    async fn loopback_server_handles_malformed_denied_missing_state_and_timeout() {
        // 1. Malformed POST request returns 400 and listener stays alive for subsequent GET denial
        let server = LoopbackCallbackServer::bind(0, "/callback").await.unwrap();
        let redirect_uri = server.redirect_uri().to_string();
        let expected_state = "csrf-state-xyz";
        let server_task = tokio::spawn(async move {
            server
                .wait_for_code(expected_state, Duration::from_secs(5))
                .await
        });

        let client = reqwest::Client::new();
        let post_resp = client.post(&redirect_uri).send().await.unwrap();
        assert_eq!(post_resp.status(), 400);

        let denied_resp = client
            .get(format!(
                "{redirect_uri}?error=access_denied&state={expected_state}"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(denied_resp.status(), 400);
        assert!(matches!(
            server_task.await.unwrap(),
            Err(OAuthError::AccessDenied { .. })
        ));

        // 2. Missing state parameter is rejected with MissingState
        let server2 = LoopbackCallbackServer::bind(0, "/callback").await.unwrap();
        let redirect_uri2 = server2.redirect_uri().to_string();
        let task2 = tokio::spawn(async move {
            server2
                .wait_for_code("expected-state", Duration::from_secs(5))
                .await
        });
        let missing_state_resp = client
            .get(format!("{redirect_uri2}?code=abc"))
            .send()
            .await
            .unwrap();
        assert_eq!(missing_state_resp.status(), 400);
        assert!(matches!(
            task2.await.unwrap(),
            Err(OAuthError::MissingState)
        ));

        // 3. Missing code parameter with valid state is rejected with MissingAuthorizationCode
        let server3 = LoopbackCallbackServer::bind(0, "/callback").await.unwrap();
        let redirect_uri3 = server3.redirect_uri().to_string();
        let task3 = tokio::spawn(async move {
            server3
                .wait_for_code("expected-state", Duration::from_secs(5))
                .await
        });
        let missing_code_resp = client
            .get(format!("{redirect_uri3}?state=expected-state"))
            .send()
            .await
            .unwrap();
        assert_eq!(missing_code_resp.status(), 400);
        assert!(matches!(
            task3.await.unwrap(),
            Err(OAuthError::MissingAuthorizationCode)
        ));

        // 4. Timeout returns OAuthError::TimedOut cleanly
        let server4 = LoopbackCallbackServer::bind(0, "/callback").await.unwrap();
        let timeout_res = server4
            .wait_for_code("expected-state", Duration::from_millis(80))
            .await;
        assert!(matches!(timeout_res, Err(OAuthError::TimedOut)));
    }
}
