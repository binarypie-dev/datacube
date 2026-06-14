//! Unix socket server for datacube
//!
//! Handles client connections and dispatches requests to providers.

use crate::config::Config;
use crate::proto::{ListProvidersResponse, QueryRequest, QueryResponse};
use crate::providers::ProviderManager;
use prost::Message;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, error, info, warn};

/// Message types for the protocol
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
enum MessageType {
    Query = 1,
    QueryResponse = 2,
    ListProviders = 5,
    ListProvidersResponse = 6,
}

impl TryFrom<u8> for MessageType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(MessageType::Query),
            2 => Ok(MessageType::QueryResponse),
            5 => Ok(MessageType::ListProviders),
            6 => Ok(MessageType::ListProvidersResponse),
            _ => Err(()),
        }
    }
}

/// The datacube server
pub struct Server {
    config: Config,
    provider_manager: Arc<ProviderManager>,
}

impl Server {
    /// Create a new server with the given configuration
    pub fn new(config: Config, provider_manager: ProviderManager) -> Self {
        Self {
            config,
            provider_manager: Arc::new(provider_manager),
        }
    }

    /// Run the server
    pub async fn run(&self) -> anyhow::Result<()> {
        let socket_path = &self.config.socket_path;

        // Remove existing socket file if it exists
        if socket_path.exists() {
            std::fs::remove_file(socket_path)?;
        }

        // Ensure parent directory exists
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(socket_path)?;
        info!("Server listening on {:?}", socket_path);

        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let manager = Arc::clone(&self.provider_manager);
                    let max_results = self.config.max_results;

                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, manager, max_results).await {
                            error!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                }
            }
        }
    }
}

/// Handle a single client connection
async fn handle_connection(
    mut stream: UnixStream,
    manager: Arc<ProviderManager>,
    max_results: usize,
) -> anyhow::Result<()> {
    debug!("New client connection");

    loop {
        // Read message type (1 byte) and length (4 bytes big-endian)
        let mut header = [0u8; 5];
        match stream.read_exact(&mut header).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                debug!("Client disconnected");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }

        let msg_type = header[0];
        let length = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;

        // Read message body
        let mut body = vec![0u8; length];
        stream.read_exact(&mut body).await?;

        // Process message based on type
        let response = match MessageType::try_from(msg_type) {
            Ok(MessageType::Query) => handle_query(&body, &manager, max_results).await,
            Ok(MessageType::ListProviders) => handle_list_providers(&body, &manager).await,
            Ok(other) => {
                warn!("Unexpected message type: {:?}", other);
                continue;
            }
            Err(_) => {
                warn!("Unknown message type: {}", msg_type);
                continue;
            }
        };

        // Send response
        if let Some((resp_type, data)) = response {
            let mut response_header = vec![resp_type as u8];
            response_header.extend_from_slice(&(data.len() as u32).to_be_bytes());
            stream.write_all(&response_header).await?;
            stream.write_all(&data).await?;
            stream.flush().await?;
        }
    }
}

/// Handle a query request
async fn handle_query(
    body: &[u8],
    manager: &ProviderManager,
    default_max_results: usize,
) -> Option<(MessageType, Vec<u8>)> {
    let request = match QueryRequest::decode(body) {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to decode QueryRequest: {}", e);
            return None;
        }
    };

    debug!(
        "Query: '{}' (providers: {:?})",
        request.query, request.providers
    );

    let max_results = if request.max_results > 0 {
        request.max_results as usize
    } else {
        default_max_results
    };

    let items = manager
        .query(&request.query, max_results, &request.providers)
        .await;

    let response = QueryResponse {
        query: request.query,
        items: items.into_iter().map(Into::into).collect(),
        qid: uuid::Uuid::new_v4().to_string(),
    };

    Some((MessageType::QueryResponse, response.encode_to_vec()))
}

/// Handle a list providers request
async fn handle_list_providers(
    _body: &[u8],
    manager: &ProviderManager,
) -> Option<(MessageType, Vec<u8>)> {
    let providers = manager.list_providers().await;

    let response = ListProvidersResponse {
        providers: providers.into_iter().map(Into::into).collect(),
    };

    Some((MessageType::ListProvidersResponse, response.encode_to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::ListProvidersRequest;
    use crate::providers::CalculatorProvider;
    use std::time::Duration;
    use tokio::net::UnixStream;

    async fn spawn_calculator_server() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("datacube-it-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("datacube.sock");

        let mut config = Config::default();
        config.socket_path = socket.clone();
        // Keep the test hermetic: don't scan the host for applications.
        config.providers.applications.enabled = false;

        let manager = ProviderManager::new();
        manager.register(CalculatorProvider::new()).await;

        let server = Server::new(config, manager);
        tokio::spawn(async move {
            let _ = server.run().await;
        });

        // Wait for the socket to be bound.
        for _ in 0..200 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        socket
    }

    async fn write_frame(stream: &mut UnixStream, msg_type: u8, body: &[u8]) {
        let mut header = vec![msg_type];
        header.extend_from_slice(&(body.len() as u32).to_be_bytes());
        stream.write_all(&header).await.unwrap();
        stream.write_all(body).await.unwrap();
        stream.flush().await.unwrap();
    }

    async fn read_frame(stream: &mut UnixStream) -> (u8, Vec<u8>) {
        let mut header = [0u8; 5];
        stream.read_exact(&mut header).await.unwrap();
        let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
        let mut body = vec![0u8; len];
        stream.read_exact(&mut body).await.unwrap();
        (header[0], body)
    }

    #[tokio::test]
    async fn query_round_trip_over_socket() {
        let socket = spawn_calculator_server().await;
        let mut stream = UnixStream::connect(&socket).await.expect("connect");

        let request = QueryRequest {
            query: "=2+2".to_string(),
            max_results: 10,
            providers: vec![],
            exact: false,
        };
        write_frame(
            &mut stream,
            MessageType::Query as u8,
            &request.encode_to_vec(),
        )
        .await;

        let (msg_type, body) = read_frame(&mut stream).await;
        assert_eq!(msg_type, MessageType::QueryResponse as u8);

        let response = QueryResponse::decode(body.as_slice()).unwrap();
        assert!(
            !response.items.is_empty(),
            "calculator should return a result"
        );
        assert_eq!(response.items[0].text, "4");
        assert_eq!(response.items[0].provider, "calculator");
        assert!(!response.qid.is_empty());

        let _ = std::fs::remove_dir_all(socket.parent().unwrap());
    }

    #[tokio::test]
    async fn list_providers_over_socket() {
        let socket = spawn_calculator_server().await;
        let mut stream = UnixStream::connect(&socket).await.expect("connect");

        let request = ListProvidersRequest {};
        write_frame(
            &mut stream,
            MessageType::ListProviders as u8,
            &request.encode_to_vec(),
        )
        .await;

        let (msg_type, body) = read_frame(&mut stream).await;
        assert_eq!(msg_type, MessageType::ListProvidersResponse as u8);

        let response = ListProvidersResponse::decode(body.as_slice()).unwrap();
        assert!(response.providers.iter().any(|p| p.name == "calculator"));

        let _ = std::fs::remove_dir_all(socket.parent().unwrap());
    }
}
