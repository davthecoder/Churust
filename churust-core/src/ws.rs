//! WebSocket support (feature `ws`).
//!
//! A handler upgrades a request by taking the [`WebSocketUpgrade`] extractor and
//! calling [`WebSocketUpgrade::on_upgrade`]:
//!
//! ```no_run
//! use churust_core::Churust;
//! use churust_core::ws::WebSocketUpgrade;
//!
//! # fn build() {
//! Churust::server().routing(|r| {
//!     r.get("/echo", |ws: WebSocketUpgrade| async move {
//!         ws.on_upgrade(|mut sock| async move {
//!             while let Some(Ok(msg)) = sock.recv().await {
//!                 if sock.send(msg).await.is_err() { break; }
//!             }
//!         })
//!     });
//! });
//! # }
//! ```

use crate::call::Call;
use crate::error::{Error, Result};
use crate::extract::FromCallParts;
use crate::response::Response;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use http::header::{
    CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_PROTOCOL,
    SEC_WEBSOCKET_VERSION, UPGRADE,
};
use http::{HeaderMap, HeaderValue, StatusCode};
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};
use tokio_tungstenite::WebSocketStream;

/// A cloneable, takeable holder for hyper's pending connection upgrade. The
/// engine inserts one into a [`Call`]'s extensions for WebSocket
/// handshake requests; [`WebSocketUpgrade`] takes it back out.
#[derive(Clone)]
pub struct OnUpgradeHandle(Arc<Mutex<Option<OnUpgrade>>>);

/// Frame and message size caps for an upgraded socket, seeded into the call by
/// the engine so `on_upgrade` can apply them without reaching for global state.
///
/// The two are separate on purpose: a peer that respects the frame cap can
/// still send an unbounded number of small continuation frames that reassemble
/// into one enormous message.
#[derive(Debug, Clone, Copy)]
pub struct WsLimits {
    /// Maximum size of a single frame, in bytes.
    pub max_frame_bytes: usize,
    /// Maximum size of a reassembled message, in bytes.
    pub max_message_bytes: usize,
}

impl OnUpgradeHandle {
    /// Wrap a pending upgrade.
    pub fn new(on_upgrade: OnUpgrade) -> Self {
        Self(Arc::new(Mutex::new(Some(on_upgrade))))
    }

    /// Take the upgrade future (can only succeed once).
    pub(crate) fn take(&self) -> Option<OnUpgrade> {
        self.0.lock().ok().and_then(|mut guard| guard.take())
    }
}

impl std::fmt::Debug for OnUpgradeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OnUpgradeHandle")
    }
}

/// True if the request headers request a WebSocket upgrade (`Connection:
/// upgrade` + `Upgrade: websocket`, case-insensitive).
pub(crate) fn is_upgrade_request(headers: &HeaderMap) -> bool {
    let connection_upgrade = headers
        .get(CONNECTION)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.to_ascii_lowercase()
                .split(',')
                .any(|p| p.trim() == "upgrade")
        })
        .unwrap_or(false);
    let upgrade_websocket = headers
        .get(UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    connection_upgrade && upgrade_websocket
}

use tokio_tungstenite::tungstenite::Message as TMessage;

/// A WebSocket message. A deliberately small enum so user code never has to name
/// `tungstenite` types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// A UTF-8 text frame.
    Text(String),
    /// A binary frame.
    Binary(Vec<u8>),
    /// A ping control frame (payload echoed back by the peer as a pong).
    Ping(Vec<u8>),
    /// A pong control frame.
    Pong(Vec<u8>),
    /// A close frame (connection is closing).
    Close,
}

impl From<Message> for TMessage {
    fn from(m: Message) -> Self {
        match m {
            Message::Text(s) => TMessage::Text(s.into()),
            Message::Binary(b) => TMessage::Binary(b.into()),
            Message::Ping(b) => TMessage::Ping(b.into()),
            Message::Pong(b) => TMessage::Pong(b.into()),
            Message::Close => TMessage::Close(None),
        }
    }
}

impl From<TMessage> for Message {
    fn from(m: TMessage) -> Self {
        match m {
            TMessage::Text(s) => Message::Text(s.to_string()),
            TMessage::Binary(b) => Message::Binary(b.to_vec()),
            TMessage::Ping(b) => Message::Ping(b.to_vec()),
            TMessage::Pong(b) => Message::Pong(b.to_vec()),
            TMessage::Close(_) => Message::Close,
            // Raw frames are not surfaced to user code.
            _ => Message::Close,
        }
    }
}

/// An established WebSocket connection. Obtained inside the
/// [`WebSocketUpgrade::on_upgrade`] callback.
pub struct WebSocket {
    inner: WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>,
}

impl WebSocket {
    /// Receive the next message. `None` when the connection has closed.
    pub async fn recv(&mut self) -> Option<Result<Message>> {
        match self.inner.next().await {
            Some(Ok(msg)) => Some(Ok(msg.into())),
            Some(Err(e)) => Some(Err(Error::internal(format!("websocket recv: {e}")))),
            None => None,
        }
    }

    /// Send a message.
    pub async fn send(&mut self, msg: Message) -> Result<()> {
        self.inner
            .send(msg.into())
            .await
            .map_err(|e| Error::internal(format!("websocket send: {e}")))
    }

    /// Convenience: send a text message.
    pub async fn send_text(&mut self, text: impl Into<String>) -> Result<()> {
        self.send(Message::Text(text.into())).await
    }

    /// Convenience: send a binary message.
    pub async fn send_binary(&mut self, bytes: impl Into<Vec<u8>>) -> Result<()> {
        self.send(Message::Binary(bytes.into())).await
    }

    /// Close the connection.
    pub async fn close(&mut self) -> Result<()> {
        self.inner
            .close(None)
            .await
            .map_err(|e| Error::internal(format!("websocket close: {e}")))
    }
}

/// Extractor that represents a pending WebSocket upgrade. A handler takes it as
/// an argument, then calls [`on_upgrade`](WebSocketUpgrade::on_upgrade).
///
/// Extraction fails with **426 Upgrade Required** if the request is not a valid
/// WebSocket handshake.
pub struct WebSocketUpgrade {
    on_upgrade: OnUpgrade,
    accept_key: HeaderValue,
    protocol: Option<HeaderValue>,
    limits: WsLimits,
}

#[async_trait]
impl FromCallParts for WebSocketUpgrade {
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        let version_ok = call
            .header(SEC_WEBSOCKET_VERSION.as_str())
            .map(|v| v == "13")
            .unwrap_or(false);
        if !is_upgrade_request(call.headers()) || !version_ok {
            return Err(Error::new(
                StatusCode::UPGRADE_REQUIRED,
                "expected a WebSocket upgrade request",
            )
            .with_response_header(UPGRADE, HeaderValue::from_static("websocket")));
        }

        let key = call
            .header(SEC_WEBSOCKET_KEY.as_str())
            .ok_or_else(|| Error::bad_request("missing Sec-WebSocket-Key"))?;
        let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
        let accept_key =
            HeaderValue::from_str(&accept).map_err(|_| Error::internal("invalid accept key"))?;

        let protocol = call
            .header(SEC_WEBSOCKET_PROTOCOL.as_str())
            .and_then(|p| p.split(',').next())
            .and_then(|p| HeaderValue::from_str(p.trim()).ok());

        let handle = call.get::<OnUpgradeHandle>().ok_or_else(|| {
            Error::new(
                StatusCode::UPGRADE_REQUIRED,
                "WebSocket upgrade unavailable (no pending connection upgrade)",
            )
        })?;
        let on_upgrade = handle
            .take()
            .ok_or_else(|| Error::internal("WebSocket upgrade already consumed"))?;

        // Absent only when a call was built without the engine (unit tests);
        // the conservative defaults then apply.
        let limits = call.get::<WsLimits>().unwrap_or(WsLimits {
            max_frame_bytes: 1 << 20,
            max_message_bytes: 4 << 20,
        });

        Ok(WebSocketUpgrade {
            on_upgrade,
            limits,
            accept_key,
            protocol,
        })
    }
}

impl WebSocketUpgrade {
    /// Finish the handshake: spawn a task that runs `callback` with the
    /// established [`WebSocket`] once the upgrade completes, and return the
    /// `101 Switching Protocols` response the engine will send.
    pub fn on_upgrade<F, Fut>(self, callback: F) -> Response
    where
        F: FnOnce(WebSocket) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let WebSocketUpgrade {
            on_upgrade,
            accept_key,
            protocol,
            limits,
        } = self;

        tokio::spawn(async move {
            if let Ok(upgraded) = on_upgrade.await {
                // Bound both a single frame and a reassembled message: a peer
                // respecting the frame cap can still stream unbounded
                // continuation frames into one enormous message.
                let mut ws_config = WebSocketConfig::default();
                ws_config.max_frame_size = Some(limits.max_frame_bytes);
                ws_config.max_message_size = Some(limits.max_message_bytes);

                let stream = WebSocketStream::from_raw_socket(
                    TokioIo::new(upgraded),
                    Role::Server,
                    Some(ws_config),
                )
                .await;
                callback(WebSocket { inner: stream }).await;
            }
        });

        let mut res = Response::new(StatusCode::SWITCHING_PROTOCOLS);
        res.headers
            .insert(UPGRADE, HeaderValue::from_static("websocket"));
        res.headers
            .insert(CONNECTION, HeaderValue::from_static("upgrade"));
        res.headers.insert(SEC_WEBSOCKET_ACCEPT, accept_key);
        if let Some(p) = protocol {
            res.headers.insert(SEC_WEBSOCKET_PROTOCOL, p);
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Churust, TestClient};
    use tokio_tungstenite::tungstenite::Message as TMessage;

    #[tokio::test]
    async fn plain_get_to_ws_route_is_426() {
        let app = Churust::server()
            .routing(|r| {
                r.get("/ws", |ws: WebSocketUpgrade| async move {
                    ws.on_upgrade(|_sock| async {})
                });
            })
            .build();
        // A normal GET (no upgrade headers, no captured handle) must be rejected.
        let res = TestClient::new(app).get("/ws").send().await;
        assert_eq!(res.status(), http::StatusCode::UPGRADE_REQUIRED);
    }

    #[test]
    fn message_round_trips_through_tungstenite() {
        let cases = [
            Message::Text("hi".into()),
            Message::Binary(vec![1, 2, 3]),
            Message::Ping(vec![9]),
            Message::Pong(vec![8]),
            Message::Close,
        ];
        for m in cases {
            let t: TMessage = m.clone().into();
            let back: Message = t.into();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn accept_key_matches_rfc6455_example() {
        // RFC 6455 §1.3 worked example.
        let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(
            b"dGhlIHNhbXBsZSBub25jZQ==",
        );
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }
}
