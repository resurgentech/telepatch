//! Serves the page (the bundle `dx build` makes from `web/`), the app's
//! text from the repo's `docs/`
//! ([`content`]), and gives each WebSocket its own shell on a PTY: PTY
//! output goes to the socket as binary frames, binary frames from the
//! socket go to the PTY, and a text frame `"<cols> <rows>"` resizes it.
//!
//! The shell runs as whoever started the server, so the listener is on
//! 127.0.0.1 only and the WebSocket is refused unless the page that opens
//! it was served from this address.

mod content;

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower_http::services::ServeDir;

const ADDR: &str = "127.0.0.1:7681";
/// Where `dx build --web --release -p web` leaves the page.
const DIST: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../target/dx/web/release/web/public");
const DOCS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs");

/// The terminal size the page draws; the PTY is opened at the same size.
#[derive(serde::Deserialize)]
struct Size {
    cols: u16,
    rows: u16,
}

pub fn app(docs: content::Docs) -> Router {
    Router::new()
        .route("/ws", get(upgrade))
        .route("/content/menus", get(menus))
        .route("/content/style.css", get(style))
        .route("/content/page/{*id}", get(page))
        .with_state(Arc::new(docs))
        .fallback_service(ServeDir::new(DIST))
}

async fn menus(State(docs): State<Arc<content::Docs>>) -> Json<Vec<content::Menu>> {
    Json(docs.menus())
}

async fn style(State(docs): State<Arc<content::Docs>>) -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], docs.style()).into_response()
}

async fn page(State(docs): State<Arc<content::Docs>>, Path(id): Path<String>) -> Response {
    match docs.page(&id) {
        Some(html) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// True when the request comes from a page this server served: `Origin`
/// names the same loopback `Host` the request was sent to. Refuses other
/// sites' pages (a WebSocket is not bound by the same-origin policy) and
/// names rebound to 127.0.0.1.
fn same_origin(headers: &HeaderMap) -> bool {
    let get = |name| headers.get(name).and_then(|v| v.to_str().ok());
    match (get(header::HOST), get(header::ORIGIN)) {
        (Some(host), Some(origin)) => {
            (host.starts_with("127.0.0.1:") || host.starts_with("localhost:"))
                && origin.strip_prefix("http://") == Some(host)
        }
        _ => false,
    }
}

async fn upgrade(ws: WebSocketUpgrade, headers: HeaderMap, Query(size): Query<Size>) -> Response {
    if !same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = shell(socket, size).await {
            eprintln!("shell: {e}");
        }
    })
}

async fn shell(mut ws: WebSocket, size: Size) -> Result<(), Box<dyn std::error::Error>> {
    let (pty, pts) = pty_process::open()?;
    pty.resize(pty_process::Size::new(size.rows, size.cols))?;
    let program = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut child = pty_process::Command::new(program)
        .env("TERM", "xterm-256color")
        .kill_on_drop(true)
        .spawn(pts)?;
    let (mut from_shell, mut to_shell) = pty.into_split();

    let mut buf = vec![0u8; 16 * 1024];
    loop {
        tokio::select! {
            read = from_shell.read(&mut buf) => match read {
                // The shell exited (a PTY reports that as EIO).
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if ws.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() {
                        break;
                    }
                }
            },
            msg = ws.recv() => match msg {
                Some(Ok(Message::Binary(keys))) => to_shell.write_all(&keys).await?,
                Some(Ok(Message::Text(size))) => {
                    let mut n = size.split(' ').map(str::parse::<u16>);
                    if let (Some(Ok(cols)), Some(Ok(rows))) = (n.next(), n.next()) {
                        to_shell.resize(pty_process::Size::new(rows, cols))?;
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
    let _ = child.kill().await;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(ADDR).await?;
    println!("http://{ADDR}/");
    axum::serve(listener, app(content::Docs::new(DOCS))).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::{self, Message as WsMessage};

    async fn serve() -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = app(content::Docs::new(DOCS));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    }

    fn request(addr: std::net::SocketAddr, origin: &str) -> tungstenite::handshake::client::Request {
        let mut req = format!("ws://{addr}/ws?cols=80&rows=24")
            .into_client_request()
            .unwrap();
        req.headers_mut().insert("origin", origin.parse().unwrap());
        req
    }

    #[tokio::test]
    async fn keys_reach_the_shell_and_its_output_comes_back() {
        let addr = serve().await;
        let origin = format!("http://{addr}");
        let (mut ws, _) = tokio_tungstenite::connect_async(request(addr, &origin))
            .await
            .unwrap();
        // The shell echoes the typed line; only its output has the sum.
        ws.send(WsMessage::Binary(b"echo $((40000+2345))\r".to_vec().into()))
            .await
            .unwrap();
        let mut seen = Vec::new();
        let found = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(Ok(msg)) = ws.next().await {
                if let WsMessage::Binary(b) = msg {
                    seen.extend_from_slice(&b);
                    if String::from_utf8_lossy(&seen).contains("42345") {
                        return true;
                    }
                }
            }
            false
        })
        .await;
        assert_eq!(found, Ok(true), "got: {}", String::from_utf8_lossy(&seen));
    }

    #[tokio::test]
    async fn a_text_frame_resizes_the_pty() {
        let addr = serve().await;
        let origin = format!("http://{addr}");
        let (mut ws, _) = tokio_tungstenite::connect_async(request(addr, &origin))
            .await
            .unwrap();
        ws.send(WsMessage::Text("132 43".into())).await.unwrap();
        ws.send(WsMessage::Binary(b"stty size\r".to_vec().into()))
            .await
            .unwrap();
        let mut seen = Vec::new();
        let found = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(Ok(msg)) = ws.next().await {
                if let WsMessage::Binary(b) = msg {
                    seen.extend_from_slice(&b);
                    if String::from_utf8_lossy(&seen).contains("43 132") {
                        return true;
                    }
                }
            }
            false
        })
        .await;
        assert_eq!(found, Ok(true), "got: {}", String::from_utf8_lossy(&seen));
    }

    #[tokio::test]
    async fn another_sites_page_is_refused() {
        let addr = serve().await;
        let err = tokio_tungstenite::connect_async(request(addr, "http://evil.example"))
            .await
            .unwrap_err();
        match err {
            tungstenite::Error::Http(resp) => assert_eq!(resp.status(), 403),
            other => panic!("expected 403, got {other:?}"),
        }
    }
}
