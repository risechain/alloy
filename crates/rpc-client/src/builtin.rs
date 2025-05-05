use alloy_json_rpc::RpcError;
use alloy_transport::{BoxTransport, TransportConnect, TransportError, TransportErrorKind};
use std::str::FromStr;

#[cfg(any(feature = "ws", feature = "ipc"))]
use alloy_pubsub::{PubSubConnect, RetryConfig};

/// Connection string for built-in transports.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BuiltInConnectionString {
    /// HTTP transport.
    #[cfg(any(feature = "reqwest", feature = "hyper"))]
    Http(url::Url),
    /// WebSocket transport.
    #[cfg(feature = "ws")]
    Ws {
        /// The URL to connect to.
        url: url::Url,
        /// The optional authorization header to use.
        auth: Option<alloy_transport::Authorization>,
        /// Optional configuration for retry behavior.
        retry_config: Option<RetryConfig>,
    },
    /// IPC transport.
    #[cfg(feature = "ipc")]
    Ipc(std::path::PathBuf),
}

impl TransportConnect for BuiltInConnectionString {
    fn is_local(&self) -> bool {
        match self {
            #[cfg(any(feature = "reqwest", feature = "hyper"))]
            Self::Http(url) => alloy_transport::utils::guess_local_url(url),
            #[cfg(feature = "ws")]
            Self::Ws { url, .. } => alloy_transport::utils::guess_local_url(url),
            #[cfg(feature = "ipc")]
            Self::Ipc(_) => true,
            #[cfg(not(any(
                feature = "reqwest",
                feature = "hyper",
                feature = "ws",
                feature = "ipc"
            )))]
            _ => false,
        }
    }

    async fn get_transport(&self) -> Result<BoxTransport, TransportError> {
        self.connect_boxed().await
    }
}

impl BuiltInConnectionString {
    /// Connect with the given connection string.
    ///
    /// # Notes
    ///
    /// - If `hyper` feature is enabled
    /// - WS will extract auth, however, auth is disabled for wasm.
    pub async fn connect_boxed(&self) -> Result<BoxTransport, TransportError> {
        // NB:
        // HTTP match will always produce hyper if the feature is enabled.
        // WS match arms are fall-through. Auth arm is disabled for wasm.
        match self {
            // reqwest is enabled, hyper is not
            #[cfg(all(not(feature = "hyper"), feature = "reqwest"))]
            Self::Http(url) => {
                Ok(alloy_transport::Transport::boxed(
                    alloy_transport_http::Http::<reqwest::Client>::new(url.clone()),
                ))
            }

            // hyper is enabled, reqwest is not
            #[cfg(feature = "hyper")]
            Self::Http(url) => Ok(alloy_transport::Transport::boxed(
                alloy_transport_http::HyperTransport::new_hyper(url.clone()),
            )),

            #[cfg(all(not(target_family = "wasm"), feature = "ws"))]
            Self::Ws { url, auth, retry_config } => {
                let mut connector = alloy_transport_ws::WsConnect::new(url.clone());

                // Apply authentication if available
                if let Some(auth) = auth {
                    #[cfg(not(target_family = "wasm"))]
                    {
                        connector = connector.with_auth(auth.clone());
                    }
                }

                // Apply retry configuration if available
                if let Some(config) = retry_config {
                    connector = connector.with_retry_config(*config);
                }

                connector.into_service().await.map(alloy_transport::Transport::boxed)
            }

            #[cfg(feature = "ipc")]
            Self::Ipc(path) => alloy_transport_ipc::IpcConnect::new(path.to_owned())
                .into_service()
                .await
                .map(alloy_transport::Transport::boxed),

            #[cfg(not(any(
                feature = "reqwest",
                feature = "hyper",
                feature = "ws",
                feature = "ipc"
            )))]
            _ => Err(TransportErrorKind::custom_str(
                "No transports enabled. Enable one of: reqwest, hyper, ws, ipc",
            )),
        }
    }

    /// Tries to parse the given string as an HTTP URL.
    #[cfg(any(feature = "reqwest", feature = "hyper"))]
    pub fn try_as_http(s: &str) -> Result<Self, TransportError> {
        let url = if s.starts_with("localhost:") || s.parse::<std::net::SocketAddr>().is_ok() {
            let s = format!("http://{s}");
            url::Url::parse(&s)
        } else {
            url::Url::parse(s)
        }
        .map_err(TransportErrorKind::custom)?;

        let scheme = url.scheme();
        if scheme != "http" && scheme != "https" {
            let msg = format!("invalid URL scheme: {scheme}; expected `http` or `https`");
            return Err(TransportErrorKind::custom_str(&msg));
        }

        Ok(Self::Http(url))
    }

    /// Tries to parse the given string as a WebSocket URL.
    #[cfg(feature = "ws")]
    pub fn try_as_ws(s: &str) -> Result<Self, TransportError> {
        let url = if s.starts_with("localhost:") || s.parse::<std::net::SocketAddr>().is_ok() {
            let s = format!("ws://{s}");
            url::Url::parse(&s)
        } else {
            url::Url::parse(s)
        }
        .map_err(TransportErrorKind::custom)?;

        let scheme = url.scheme();
        if scheme != "ws" && scheme != "wss" {
            let msg = format!("invalid URL scheme: {scheme}; expected `ws` or `wss`");
            return Err(TransportErrorKind::custom_str(&msg));
        }

        let auth = alloy_transport::Authorization::extract_from_url(&url);

        Ok(Self::Ws { url, auth, retry_config: None })
    }

    /// Tries to parse the given string as an IPC path, returning an error if
    /// the path does not exist.
    #[cfg(feature = "ipc")]
    pub fn try_as_ipc(s: &str) -> Result<Self, TransportError> {
        let s = s.strip_prefix("file://").or_else(|| s.strip_prefix("ipc://")).unwrap_or(s);

        // Check if it exists.
        let path = std::path::Path::new(s);
        let _meta = path.metadata().map_err(|e| {
            let msg = format!("failed to read IPC path {}: {e}", path.display());
            TransportErrorKind::custom_str(&msg)
        })?;

        Ok(Self::Ipc(path.to_path_buf()))
    }

    /// Replace the retry_config with a new one
    #[cfg(feature = "ws")]
    pub fn with_retry_config(self, config: RetryConfig) -> Self {
        match self {
            Self::Ws { url, auth, .. } => Self::Ws { url, auth, retry_config: Some(config) },
            _ => self,
        }
    }
}

impl FromStr for BuiltInConnectionString {
    type Err = RpcError<TransportErrorKind>;

    #[allow(clippy::let_and_return)]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let res = Err(TransportErrorKind::custom_str(&format!(
            "No transports enabled. Enable one of: reqwest, hyper, ws, ipc. Connection info: '{s}'"
        )));
        #[cfg(any(feature = "reqwest", feature = "hyper"))]
        let res = res.or_else(|_| Self::try_as_http(s));
        #[cfg(feature = "ws")]
        let res = res.or_else(|_| Self::try_as_ws(s));
        #[cfg(feature = "ipc")]
        let res = res.or_else(|_| Self::try_as_ipc(s));
        res
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use similar_asserts::assert_eq;
    use url::Url;

    #[test]
    fn test_parsing_urls() {
        assert_eq!(
            BuiltInConnectionString::from_str("http://localhost:8545").unwrap(),
            BuiltInConnectionString::Http("http://localhost:8545".parse::<Url>().unwrap())
        );
        assert_eq!(
            BuiltInConnectionString::from_str("localhost:8545").unwrap(),
            BuiltInConnectionString::Http("http://localhost:8545".parse::<Url>().unwrap())
        );
        assert_eq!(
            BuiltInConnectionString::from_str("https://localhost:8545").unwrap(),
            BuiltInConnectionString::Http("https://localhost:8545".parse::<Url>().unwrap())
        );
        assert_eq!(
            BuiltInConnectionString::from_str("localhost:8545").unwrap(),
            BuiltInConnectionString::Http("http://localhost:8545".parse::<Url>().unwrap())
        );
        assert_eq!(
            BuiltInConnectionString::from_str("http://127.0.0.1:8545").unwrap(),
            BuiltInConnectionString::Http("http://127.0.0.1:8545".parse::<Url>().unwrap())
        );

        assert_eq!(
            BuiltInConnectionString::from_str("http://localhost").unwrap(),
            BuiltInConnectionString::Http("http://localhost".parse::<Url>().unwrap())
        );
        assert_eq!(
            BuiltInConnectionString::from_str("127.0.0.1:8545").unwrap(),
            BuiltInConnectionString::Http("http://127.0.0.1:8545".parse::<Url>().unwrap())
        );
        assert_eq!(
            BuiltInConnectionString::from_str("http://user:pass@example.com").unwrap(),
            BuiltInConnectionString::Http("http://user:pass@example.com".parse::<Url>().unwrap())
        );
    }

    #[test]
    #[cfg(feature = "ws")]
    fn test_parsing_ws() {
        use alloy_transport::Authorization;

        assert_eq!(
            BuiltInConnectionString::from_str("ws://localhost:8545").unwrap(),
            BuiltInConnectionString::Ws {
                url: "ws://localhost:8545".parse::<Url>().unwrap(),
                auth: None,
                retry_config: None,
            }
        );
        assert_eq!(
            BuiltInConnectionString::from_str("wss://localhost:8545").unwrap(),
            BuiltInConnectionString::Ws {
                url: "wss://localhost:8545".parse::<Url>().unwrap(),
                auth: None,
                retry_config: None,
            }
        );
        assert_eq!(
            BuiltInConnectionString::from_str("ws://127.0.0.1:8545").unwrap(),
            BuiltInConnectionString::Ws {
                url: "ws://127.0.0.1:8545".parse::<Url>().unwrap(),
                auth: None,
                retry_config: None,
            }
        );

        assert_eq!(
            BuiltInConnectionString::from_str("ws://alice:pass@127.0.0.1:8545").unwrap(),
            BuiltInConnectionString::Ws {
                url: "ws://alice:pass@127.0.0.1:8545".parse::<Url>().unwrap(),
                auth: Some(Authorization::basic("alice", "pass")),
                retry_config: None,
            }
        );
    }

    #[test]
    #[cfg(feature = "ipc")]
    #[cfg_attr(windows, ignore = "TODO: windows IPC")]
    fn test_parsing_ipc() {
        use alloy_node_bindings::Anvil;

        // Spawn an Anvil instance to create an IPC socket, as it's different from a normal file.
        let temp_dir = tempfile::tempdir().unwrap();
        let ipc_path = temp_dir.path().join("anvil.ipc");
        let ipc_arg = format!("--ipc={}", ipc_path.display());
        let _anvil = Anvil::new().arg(ipc_arg).spawn();
        let path_str = ipc_path.to_str().unwrap();

        assert_eq!(
            BuiltInConnectionString::from_str(&format!("ipc://{path_str}")).unwrap(),
            BuiltInConnectionString::Ipc(ipc_path.clone())
        );

        assert_eq!(
            BuiltInConnectionString::from_str(&format!("file://{path_str}")).unwrap(),
            BuiltInConnectionString::Ipc(ipc_path.clone())
        );

        assert_eq!(
            BuiltInConnectionString::from_str(ipc_path.to_str().unwrap()).unwrap(),
            BuiltInConnectionString::Ipc(ipc_path.clone())
        );
    }

    #[test]
    #[cfg(feature = "ws")]
    fn test_ws_with_retry_settings() {
        use std::time::Duration;

        // Create a basic WebSocket connection
        let ws_string = BuiltInConnectionString::from_str("ws://localhost:8545").unwrap();
        assert_eq!(
            ws_string,
            BuiltInConnectionString::Ws {
                url: "ws://localhost:8545".parse::<Url>().unwrap(),
                auth: None,
                retry_config: None,
            }
        );

        // Test with custom RetryConfig
        let custom_config = RetryConfig { max_retries: 25, retry_interval: Duration::from_secs(7) };
        let ws_with_config = ws_string.with_retry_config(custom_config.clone());

        assert_eq!(
            ws_with_config,
            BuiltInConnectionString::Ws {
                url: "ws://localhost:8545".parse::<Url>().unwrap(),
                auth: None,
                retry_config: Some(custom_config),
            }
        );
    }
}
