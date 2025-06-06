use crate::WsBackend;
use alloy_pubsub::PubSubConnect;
use alloy_transport::{utils::Spawnable, Authorization, TransportErrorKind, TransportResult};
use futures::{SinkExt, StreamExt};
use serde_json::value::RawValue;
use std::time::Duration;
use tokio::time::sleep;
use tokio_tungstenite::{
    tungstenite::{self, client::IntoClientRequest, Message},
    MaybeTlsStream, WebSocketStream,
};

type TungsteniteStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

const KEEPALIVE: u64 = 10;

/// Simple connection details for a websocket connection.
#[derive(Clone, Debug)]
pub struct WsConnect {
    /// The URL to connect to.
    url: String,
    /// The authorization header to use.
    auth: Option<Authorization>,
    /// The websocket config.
    config: Option<WebSocketConfig>,
    /// Max number of retries before failing and exiting the connection.
    /// Default is 10.
    max_retries: u32,
    /// The interval between retries.
    /// Default is 3 seconds.
    retry_interval: Duration,
}

impl WsConnect {
    /// Creates a new websocket connection configuration.
    pub fn new<S: Into<String>>(url: S) -> Self {
        Self {
            url: url.into(),
            auth: None,
            config: None,
            max_retries: 10,
            retry_interval: Duration::from_secs(3),
        }
    }

    /// Sets the authorization header.
    pub fn with_auth(mut self, auth: Authorization) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Sets the websocket config.
    pub const fn with_config(mut self, config: WebSocketConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Get the URL string of the connection.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Get the authorization header.
    pub fn auth(&self) -> Option<&Authorization> {
        self.auth.as_ref()
    }

    /// Get the websocket config.
    pub fn config(&self) -> Option<&WebSocketConfig> {
        self.config.as_ref()
    }

    /// Sets the max number of retries before failing and exiting the connection.
    /// Default is 10.
    pub const fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Sets the interval between retries.
    /// Default is 3 seconds.
    pub const fn with_retry_interval(mut self, retry_interval: Duration) -> Self {
        self.retry_interval = retry_interval;
        self
    }
}

impl IntoClientRequest for WsConnect {
    fn into_client_request(self) -> tungstenite::Result<tungstenite::handshake::client::Request> {
        let mut request: http::Request<()> = self.url.into_client_request()?;
        if let Some(auth) = self.auth {
            let mut auth_value = http::HeaderValue::from_str(&auth.to_string())?;
            auth_value.set_sensitive(true);

            request.headers_mut().insert(http::header::AUTHORIZATION, auth_value);
        }

        request.into_client_request()
    }
}

impl PubSubConnect for WsConnect {
    fn is_local(&self) -> bool {
        alloy_transport::utils::guess_local_url(&self.url)
    }

    async fn connect(&self) -> TransportResult<alloy_pubsub::ConnectionHandle> {
        let request = self.clone().into_client_request();
        let req = request.map_err(TransportErrorKind::custom)?;
        let (socket, _) = tokio_tungstenite::connect_async_with_config(req, self.config, false)
            .await
            .map_err(TransportErrorKind::custom)?;

        let (handle, interface) = alloy_pubsub::ConnectionHandle::new();
        let backend = WsBackend { socket, interface };

        backend.spawn();

        Ok(handle.with_max_retries(self.max_retries).with_retry_interval(self.retry_interval))
    }
}

impl WsBackend<TungsteniteStream> {
    /// Handle a message from the server.
    #[expect(clippy::result_unit_err)]
    pub fn handle(&mut self, msg: Message) -> Result<(), ()> {
        match msg {
            Message::Text(text) => self.handle_text(&text),
            Message::Close(frame) => {
                if frame.is_some() {
                    error!(?frame, "Received close frame with data");
                } else {
                    error!("WS server has gone away");
                }
                Err(())
            }
            Message::Binary(_) => {
                error!("Received binary message, expected text");
                Err(())
            }
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => Ok(()),
        }
    }

    /// Send a message to the server.
    pub async fn send(&mut self, msg: Box<RawValue>) -> Result<(), tungstenite::Error> {
        self.socket.send(Message::Text(msg.get().to_owned().into())).await
    }

    /// Spawn a new backend task.
    pub fn spawn(mut self) {
        let fut = async move {
            let mut errored = false;
            let mut expecting_pong = false;
            let keepalive = sleep(Duration::from_secs(KEEPALIVE));
            tokio::pin!(keepalive);
            loop {
                // We bias the loop as follows
                // 1. New dispatch to server.
                // 2. Keepalive.
                // 3. Response or notification from server.
                // This ensures that keepalive is sent only if no other messages
                // have been sent in the last 10 seconds. And prioritizes new
                // dispatches over responses from the server. This will fail if
                // the client saturates the task with dispatches, but that's
                // probably not a big deal.
                tokio::select! {
                    biased;
                    // we've received a new dispatch, so we send it via
                    // websocket. We handle new work before processing any
                    // responses from the server.
                    inst = self.interface.recv_from_frontend() => {
                        match inst {
                            Some(msg) => {
                                // Reset the keepalive timer.
                                keepalive.set(sleep(Duration::from_secs(KEEPALIVE)));
                                if let Err(err) = self.send(msg).await {
                                    error!(%err, "WS connection error");
                                    errored = true;
                                    break
                                }
                            },
                            // dispatcher has gone away, or shutdown was received
                            None => {
                                break
                            },
                        }
                    },
                    // Send a ping to the server, if no other messages have been
                    // sent in the last 10 seconds.
                    _ = &mut keepalive => {
                        // Still expecting a pong from the previous ping,
                        // meaning connection is errored.
                        if expecting_pong {
                            error!("WS server missed a pong");
                            errored = true;
                            break
                        }
                        // Reset the keepalive timer.
                        keepalive.set(sleep(Duration::from_secs(KEEPALIVE)));
                        if let Err(err) = self.socket.send(Message::Ping(Default::default())).await {
                            error!(%err, "WS connection error");
                            errored = true;
                            break
                        }
                        // Expecting to receive a pong before the next
                        // keepalive timer resolves.
                        expecting_pong = true;
                    }
                    resp = self.socket.next() => {
                        match resp {
                            Some(Ok(item)) => {
                                if item.is_pong() {
                                    expecting_pong = false;
                                }
                                errored = self.handle(item).is_err();
                                if errored { break }
                            },
                            Some(Err(err)) => {
                                error!(%err, "WS connection error");
                                errored = true;
                                break
                            }
                            None => {
                                error!("WS server has gone away");
                                errored = true;
                                break
                            },
                        }
                    }
                }
            }
            if errored {
                self.interface.close_with_error();
            }
        };
        fut.spawn_task()
    }
}
