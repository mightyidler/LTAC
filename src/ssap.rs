use crate::config::Transport;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{net::Ipv4Addr, time::Duration};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{self, Message},
};

pub const POWER_STATE: &str = "ssap://com.webos.service.tvpower/power/getPowerState";
pub const POWER_OFF: &str = "ssap://system/turnOff";
pub const FOREGROUND_APP: &str = "ssap://com.webos.applicationManager/getForegroundAppInfo";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} timed out")]
    Timeout(&'static str),
    #[error("WebSocket transport: {0}")]
    Transport(String),
    #[error("TV rejected {operation}: {message}")]
    Rejected {
        operation: &'static str,
        message: String,
    },
    #[error("TV closed the connection")]
    Disconnected,
    #[error("Invalid TV response: {0}")]
    Protocol(&'static str),
    #[error("TV requires pairing; run `ltac pair --reset` with the TV on")]
    PairingRequired,
    #[error("TV TLS certificate changed. Verify the TV address and run `ltac pair --reset`.")]
    CertificateChanged,
}
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Timeout(_) | Self::Transport(_) | Self::Disconnected
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "raw", rename_all = "snake_case")]
pub enum PowerState {
    On,
    ScreenOff,
    Standby,
    Unknown(String),
}
impl PowerState {
    pub fn from_payload(payload: &Value) -> Self {
        match payload.get("state").and_then(Value::as_str) {
            Some("Active" | "Screen Saver") => Self::On,
            Some("Screen Off") => Self::ScreenOff,
            Some("Power Off" | "Suspend" | "Active Standby") => Self::Standby,
            other => Self::Unknown(other.unwrap_or("missing state").to_owned()),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OffEvidence {
    SkippedInput,
    AlreadyReportedStandby,
    Acknowledged,
    ConnectionClosedAfterSend,
    ResponseTimedOutAfterSend,
}

pub struct Session {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    pub endpoint: String,
    pub certificate_sha256: Option<String>,
    next_id: u64,
}

impl Session {
    pub async fn connect(ip: Ipv4Addr, transport: Transport, pinned: Option<&str>) -> Result<Self> {
        let ports: &[u16] = match transport {
            Transport::Auto => &[3001, 3000],
            Transport::Wss => &[3001],
            Transport::Ws => &[3000],
        };
        let mut errors = Vec::new();
        for port in ports {
            // Once a TLS identity is pinned, auto cannot silently downgrade to plaintext.
            if *port == 3000 && pinned.is_some() {
                continue;
            }
            let endpoint = format!(
                "{}://{}:{}",
                if *port == 3001 { "wss" } else { "ws" },
                ip,
                port
            );
            let connector = native_tls::TlsConnector::builder()
                .danger_accept_invalid_certs(true)
                .danger_accept_invalid_hostnames(true)
                .build()
                .map_err(|e| Error::Transport(e.to_string()))?;
            let connection = timeout(
                Duration::from_secs(3),
                connect_async_tls_with_config(
                    &endpoint,
                    Some(
                        tungstenite::protocol::WebSocketConfig::default()
                            .max_message_size(Some(1024 * 1024))
                            .max_frame_size(Some(1024 * 1024)),
                    ),
                    true,
                    Some(Connector::NativeTls(connector)),
                ),
            )
            .await;
            let (socket, _) = match connection {
                Ok(Ok(value)) => value,
                Ok(Err(e)) => {
                    errors.push(format!("{endpoint}: {e}"));
                    continue;
                }
                Err(_) => {
                    errors.push(format!("{endpoint}: connect timeout"));
                    continue;
                }
            };
            let certificate_sha256 = match socket.get_ref() {
                MaybeTlsStream::NativeTls(tls) => tls
                    .get_ref()
                    .peer_certificate()
                    .map_err(|e| Error::Transport(e.to_string()))?
                    .map(|cert| {
                        cert.to_der()
                            .map(|der| format!("{:x}", Sha256::digest(der)))
                    })
                    .transpose()
                    .map_err(|e| Error::Transport(e.to_string()))?,
                _ => None,
            };
            if let Some(expected) = pinned
                && certificate_sha256.as_deref() != Some(expected)
            {
                return Err(Error::CertificateChanged);
            }
            return Ok(Self {
                socket,
                endpoint,
                certificate_sha256,
                next_id: 0,
            });
        }
        Err(Error::Transport(errors.join("; ")))
    }

    pub async fn register(&mut self, key: Option<&str>, allow_pairing: bool) -> Result<String> {
        let limit = if allow_pairing {
            Duration::from_secs(90)
        } else {
            Duration::from_secs(8)
        };
        timeout(limit, async {
            self.send(json!({"type":"hello", "id":"hello", "payload":{"appId":"com.ltac.windows", "appName":"LTAC"}})).await?;
            let hello = timeout(REQUEST_TIMEOUT, self.read_for("hello")).await
                .map_err(|_| Error::Timeout("Hello handshake"))??;
            if hello["type"] != "hello" { return Err(Error::Protocol("Expected hello")); }
            // Recent webOS versions require this pre-registration request. A TV
            // denial is optional; a broken connection or timeout is not ignored.
            match self.request("ssap://system/getSystemInfo").await {
                Ok(_) => {},
                Err(Error::Rejected { .. }) => {},
                Err(e) => return Err(e),
            }
            let mut payload = json!({"forcePairing":false, "pairingType":"PROMPT", "manifest":{
                "manifestVersion":1, "appVersion":env!("CARGO_PKG_VERSION"),
                "permissions":["CONTROL_POWER", "READ_POWER_STATE", "READ_NETWORK_STATE", "READ_RUNNING_APPS"]
            }});
            if let Some(key) = key { payload["client-key"] = key.into(); }
            self.send(json!({"type":"register", "id":"register", "payload":payload})).await?;
            loop {
                let frame = self.read_for("register").await?;
                if frame["type"] == "registered" {
                    return frame["payload"]["client-key"].as_str().filter(|s| !s.is_empty())
                        .map(str::to_owned).ok_or(Error::Protocol("Registered without client key"));
                }
                if frame["payload"]["pairingType"] == "PROMPT" && !allow_pairing { return Err(Error::PairingRequired); }
                if frame["payload"]["pairingType"].as_str().is_some_and(|s| s != "PROMPT") {
                    return Err(Error::Protocol("TV requested an unsupported pairing method"));
                }
            }
        }).await.map_err(|_| Error::Timeout("Pairing/registration"))?
    }

    async fn send(&mut self, frame: Value) -> Result<()> {
        timeout(
            REQUEST_TIMEOUT,
            self.socket.send(Message::Text(frame.to_string().into())),
        )
        .await
        .map_err(|_| Error::Timeout("Send"))?
        .map_err(map_ws_error)
    }
    async fn read_for(&mut self, id: &str) -> Result<Value> {
        loop {
            let message = self
                .socket
                .next()
                .await
                .ok_or(Error::Disconnected)?
                .map_err(map_ws_error)?;
            match message {
                Message::Text(text) => {
                    let frame: Value = serde_json::from_str(&text)
                        .map_err(|_| Error::Protocol("Malformed JSON"))?;
                    // webOS hello frames may omit the echoed request ID.
                    let hello = id == "hello" && frame["type"] == "hello";
                    let global_error = frame["type"] == "error" && frame.get("id").is_none();
                    if frame["id"].as_str() != Some(id) && !hello && !global_error {
                        continue;
                    }
                    if frame["type"] == "error" {
                        return Err(Error::Rejected {
                            operation: if id == "register" {
                                "registration"
                            } else {
                                "request"
                            },
                            message: frame["error"]
                                .as_str()
                                .unwrap_or("unspecified error")
                                .to_owned(),
                        });
                    }
                    return Ok(frame);
                }
                Message::Ping(_) => self.socket.flush().await.map_err(map_ws_error)?,
                Message::Close(_) => return Err(Error::Disconnected),
                _ => {}
            }
        }
    }
    pub async fn request(&mut self, uri: &str) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        self.send(json!({"type":"request", "id":id, "uri":uri, "payload":{}}))
            .await?;
        let frame = timeout(REQUEST_TIMEOUT, self.read_for(&id))
            .await
            .map_err(|_| Error::Timeout("Response"))??;
        response_payload(frame)
    }
    pub async fn power_state(&mut self) -> Result<PowerState> {
        Ok(PowerState::from_payload(&self.request(POWER_STATE).await?))
    }
    pub async fn foreground_app(&mut self) -> Result<String> {
        let payload = self.request(FOREGROUND_APP).await?;
        if payload["returnValue"] != true {
            return Err(Error::Protocol(
                "Foreground app response did not report success",
            ));
        }
        payload
            .get("appId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or(Error::Protocol("Missing foreground app ID"))
    }
    /// Wait for incoming traffic without polling, replying to server pings.
    /// The watcher retains its authenticated socket for a prompt suspend OFF.
    pub async fn idle_step(&mut self) -> Result<()> {
        match self
            .socket
            .next()
            .await
            .ok_or(Error::Disconnected)?
            .map_err(map_ws_error)?
        {
            Message::Close(_) => Err(Error::Disconnected),
            Message::Ping(_) => self.socket.flush().await.map_err(map_ws_error),
            _ => Ok(()),
        }
    }
    pub async fn power_off(&mut self) -> Result<OffEvidence> {
        self.power_off_tracked(None).await
    }

    /// Marks a possibly transmitted OFF before awaiting the socket write. This
    /// survives cancellation and prevents shutdown from blindly resending OFF.
    pub async fn power_off_tracked(
        &mut self,
        attempted: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<OffEvidence> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        if let Some(attempted) = attempted {
            attempted.store(true, std::sync::atomic::Ordering::Release);
        }
        self.send(json!({"type":"request", "id":id, "uri":POWER_OFF, "payload":{}}))
            .await?;
        // Do not retry a successfully written OFF. Some TVs close before ACK.
        match timeout(REQUEST_TIMEOUT, self.read_for(&id)).await {
            Ok(Ok(frame)) => {
                let payload = response_payload(frame)?;
                if payload["returnValue"] != true {
                    return Err(Error::Protocol(
                        "OFF response did not acknowledge returnValue=true",
                    ));
                }
                Ok(OffEvidence::Acknowledged)
            }
            Ok(Err(Error::Disconnected)) => Ok(OffEvidence::ConnectionClosedAfterSend),
            Err(_) => Ok(OffEvidence::ResponseTimedOutAfterSend),
            Ok(Err(e)) => Err(e),
        }
    }
}

fn response_payload(frame: Value) -> Result<Value> {
    if frame["type"] != "response" {
        return Err(Error::Protocol("Expected response"));
    }
    let payload = frame
        .get("payload")
        .filter(|v| v.is_object())
        .ok_or(Error::Protocol("Missing payload"))?;
    if payload["returnValue"] == false {
        return Err(Error::Rejected {
            operation: "request",
            message: payload["errorText"]
                .as_str()
                .unwrap_or("returnValue=false")
                .to_owned(),
        });
    }
    Ok(payload.clone())
}
fn map_ws_error(error: tungstenite::Error) -> Error {
    match error {
        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => {
            Error::Disconnected
        }
        tungstenite::Error::Protocol(
            tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
        ) => Error::Disconnected,
        tungstenite::Error::Io(ref e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionAborted
            ) =>
        {
            Error::Disconnected
        }
        other => Error::Transport(other.to_string()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::net::TcpListener;

    pub(crate) async fn peer() -> (Session, WebSocketStream<TcpStream>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = tokio::spawn(async move {
            let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}"))
                .await
                .unwrap();
            Session {
                socket,
                endpoint: address.to_string(),
                certificate_sha256: None,
                next_id: 0,
            }
        });
        let (tcp, _) = listener.accept().await.unwrap();
        let server = tokio_tungstenite::accept_async(tcp).await.unwrap();
        (client.await.unwrap(), server)
    }
    async fn receive(server: &mut WebSocketStream<TcpStream>) -> Value {
        loop {
            if let Message::Text(text) = server.next().await.unwrap().unwrap() {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }
    async fn send(server: &mut WebSocketStream<TcpStream>, frame: Value) {
        server
            .send(Message::Text(frame.to_string().into()))
            .await
            .unwrap();
    }
    async fn handshake_prefix(server: &mut WebSocketStream<TcpStream>) {
        let hello = receive(server).await;
        assert_eq!(hello["type"], "hello");
        send(
            server,
            json!({"id":hello["id"], "type":"hello", "payload":{}}),
        )
        .await;
        let info = receive(server).await;
        assert_eq!(info["uri"], "ssap://system/getSystemInfo");
        // Permission denied here must not prevent registering on recent firmware.
        send(
            server,
            json!({"id":info["id"], "type":"error", "error":"401 insufficient permissions"}),
        )
        .await;
    }

    #[tokio::test]
    async fn pairing_handles_ping_and_prompt_then_registered() {
        let (mut client, mut server) = peer().await;
        let remote = tokio::spawn(async move {
            handshake_prefix(&mut server).await;
            let register = receive(&mut server).await;
            assert!(register["payload"].get("client-key").is_none());
            server.send(Message::Ping(vec![1, 2].into())).await.unwrap();
            send(
                &mut server,
                json!({"id":"unrelated", "type":"response", "payload":{}}),
            )
            .await;
            send(
                &mut server,
                json!({"id":"register", "type":"response", "payload":{"pairingType":"PROMPT"}}),
            )
            .await;
            send(
                &mut server,
                json!({"id":"register", "type":"registered", "payload":{"client-key":"test-key"}}),
            )
            .await;
        });
        assert_eq!(client.register(None, true).await.unwrap(), "test-key");
        remote.await.unwrap();
    }

    #[tokio::test]
    async fn hello_without_echoed_request_id_is_supported() {
        let (mut client, mut server) = peer().await;
        let remote = tokio::spawn(async move {
            receive(&mut server).await;
            send(&mut server, json!({"type":"hello", "payload":{}})).await;
            let info = receive(&mut server).await;
            send(
                &mut server,
                json!({"id":info["id"], "type":"response", "payload":{"returnValue":true}}),
            )
            .await;
            receive(&mut server).await;
            send(
                &mut server,
                json!({"id":"register", "type":"registered", "payload":{"client-key":"test-key"}}),
            )
            .await;
        });
        assert_eq!(client.register(None, true).await.unwrap(), "test-key");
        remote.await.unwrap();
    }

    #[tokio::test]
    async fn retained_idle_connection_answers_ping_and_detects_close() {
        let (mut client, mut server) = peer().await;
        let remote = tokio::spawn(async move {
            server.send(Message::Ping(vec![7, 8].into())).await.unwrap();
            let pong = server.next().await.unwrap().unwrap();
            assert!(matches!(pong, Message::Pong(data) if data.as_ref() == [7, 8]));
            server.close(None).await.unwrap();
        });
        client.idle_step().await.unwrap();
        assert!(matches!(client.idle_step().await, Err(Error::Disconnected)));
        remote.await.unwrap();
    }

    #[tokio::test]
    async fn off_requires_an_explicit_success_response_for_acknowledgement() {
        for (payload, succeeds) in [(json!({"returnValue":true}), true), (json!({}), false)] {
            let (mut client, mut server) = peer().await;
            let remote = tokio::spawn(async move {
                let request = receive(&mut server).await;
                send(
                    &mut server,
                    json!({"id":request["id"], "type":"response", "payload":payload}),
                )
                .await;
            });
            let result = client.power_off().await;
            if succeeds {
                assert!(matches!(result, Ok(OffEvidence::Acknowledged)));
            } else {
                assert!(matches!(result, Err(Error::Protocol(_))));
            }
            remote.await.unwrap();
        }
    }

    #[tokio::test]
    async fn stored_key_reconnect_does_not_need_a_new_prompt() {
        let (mut client, mut server) = peer().await;
        let remote = tokio::spawn(async move {
            handshake_prefix(&mut server).await;
            let register = receive(&mut server).await;
            assert_eq!(register["payload"]["client-key"], "saved-key");
            assert!(
                register["payload"]["manifest"]["permissions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|permission| permission == "READ_RUNNING_APPS")
            );
            send(
                &mut server,
                json!({"id":"register", "type":"registered", "payload":{"client-key":"saved-key"}}),
            )
            .await;
        });
        assert_eq!(
            client.register(Some("saved-key"), false).await.unwrap(),
            "saved-key"
        );
        remote.await.unwrap();
    }

    #[tokio::test]
    async fn stale_key_prompt_stops_noninteractive_commands() {
        let (mut client, mut server) = peer().await;
        let remote = tokio::spawn(async move {
            handshake_prefix(&mut server).await;
            receive(&mut server).await;
            send(
                &mut server,
                json!({"id":"register", "type":"response", "payload":{"pairingType":"PROMPT"}}),
            )
            .await;
        });
        assert!(matches!(
            client.register(Some("stale-key"), false).await,
            Err(Error::PairingRequired)
        ));
        remote.await.unwrap();
    }

    #[tokio::test]
    async fn pairing_rejection_is_terminal() {
        let (mut client, mut server) = peer().await;
        let remote = tokio::spawn(async move {
            handshake_prefix(&mut server).await;
            receive(&mut server).await;
            send(
                &mut server,
                json!({"id":"register", "type":"error", "error":"403 User rejected pairing"}),
            )
            .await;
        });
        let error = client.register(None, true).await.unwrap_err();
        assert!(matches!(error, Error::Rejected { .. }));
        assert!(!error.retryable());
        remote.await.unwrap();
    }

    #[tokio::test]
    async fn off_disconnect_is_evidence_not_an_acknowledgement() {
        let (mut client, mut server) = peer().await;
        let remote = tokio::spawn(async move {
            let request = receive(&mut server).await;
            assert_eq!(request["uri"], POWER_OFF);
            server.close(None).await.unwrap();
        });
        assert!(matches!(
            client.power_off().await.unwrap(),
            OffEvidence::ConnectionClosedAfterSend
        ));
        remote.await.unwrap();
    }

    #[tokio::test]
    async fn failed_off_write_is_never_reported_as_sent() {
        let (mut client, _server) = peer().await;
        client.socket.close(None).await.unwrap();
        assert!(client.power_off().await.is_err());
    }

    #[tokio::test]
    async fn off_permission_denied_is_failure() {
        let (mut client, mut server) = peer().await;
        let remote = tokio::spawn(async move {
            let request = receive(&mut server).await;
            send(&mut server, json!({"id":request["id"], "type":"response", "payload":{"returnValue":false, "errorText":"Permission denied"}})).await;
        });
        assert!(matches!(
            client.power_off().await,
            Err(Error::Rejected { .. })
        ));
        remote.await.unwrap();
    }

    #[tokio::test]
    async fn response_timeout_is_bounded_even_with_unrelated_frames() {
        let (mut client, mut server) = peer().await;
        let remote = tokio::spawn(async move {
            receive(&mut server).await;
            for _ in 0..60 {
                send(
                    &mut server,
                    json!({"id":"other", "type":"response", "payload":{}}),
                )
                .await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
        let started = std::time::Instant::now();
        assert!(matches!(
            client.power_state().await,
            Err(Error::Timeout("Response"))
        ));
        assert!(started.elapsed() < Duration::from_secs(7));
        remote.abort();
    }
}
