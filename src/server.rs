//! Unix socket server for datacube
//!
//! Handles client connections and dispatches requests to providers.

use crate::config::Config;
use crate::proto::{
    ActivateRequest, ActivateResponse, ListProvidersResponse, QueryRequest,
    QueryResponse,
};
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
    Activate = 3,
    ActivateResponse = 4,
    ListProviders = 5,
    ListProvidersResponse = 6,
}

impl TryFrom<u8> for MessageType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(MessageType::Query),
            2 => Ok(MessageType::QueryResponse),
            3 => Ok(MessageType::Activate),
            4 => Ok(MessageType::ActivateResponse),
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
            Ok(MessageType::Query) => {
                handle_query(&body, &manager, max_results).await
            }
            Ok(MessageType::Activate) => {
                handle_activate(&body, &manager).await
            }
            Ok(MessageType::ListProviders) => {
                handle_list_providers(&body, &manager).await
            }
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

    debug!("Query: '{}' (providers: {:?})", request.query, request.providers);

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

/// Handle an activate request
async fn handle_activate(body: &[u8], manager: &ProviderManager) -> Option<(MessageType, Vec<u8>)> {
    let request = match ActivateRequest::decode(body) {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to decode ActivateRequest: {}", e);
            return None;
        }
    };

    let Some(proto_item) = request.item else {
        error!("ActivateRequest missing item");
        return Some((
            MessageType::ActivateResponse,
            ActivateResponse {
                success: false,
                error: "Missing item".to_string(),
            }
            .encode_to_vec(),
        ));
    };

    // Convert proto Item to our Item
    let item = crate::providers::Item {
        id: proto_item.id,
        text: proto_item.text,
        subtext: proto_item.subtext,
        icon: proto_item.icon,
        provider: proto_item.provider,
        score: proto_item.score,
        exec: proto_item.exec,
        metadata: proto_item.metadata,
        actions: proto_item
            .actions
            .into_iter()
            .map(|a| crate::providers::Action {
                id: a.id,
                name: a.name,
                icon: a.icon,
            })
            .collect(),
    };

    debug!("Activate: {} from {}", item.text, item.provider);

    let action_id = if request.action_id.is_empty() {
        None
    } else {
        Some(request.action_id.as_str())
    };
    let result = manager.activate(&item, action_id).await;

    let response = match result {
        Ok(()) => ActivateResponse {
            success: true,
            error: String::new(),
        },
        Err(e) => ActivateResponse {
            success: false,
            error: e.to_string(),
        },
    };

    Some((MessageType::ActivateResponse, response.encode_to_vec()))
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
