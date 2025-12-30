//! Datacube CLI - Command-line client for testing datacube
//!
//! Usage:
//!   datacube-cli query "firefox"
//!   datacube-cli query "=2+2"
//!   datacube-cli providers
//!   datacube-cli activate <item-json>

use clap::{Parser, Subcommand};
use datacube::proto::{
    ActivateRequest, ActivateResponse, Item, ListProvidersRequest, ListProvidersResponse,
    QueryRequest, QueryResponse,
};
use prost::Message;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "datacube-cli")]
#[command(author, version, about = "CLI client for datacube")]
struct Args {
    /// Socket path
    #[arg(short, long)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Query for items
    Query {
        /// Search query
        query: String,

        /// Maximum results
        #[arg(short, long, default_value = "10")]
        max: i32,

        /// Specific providers to query (comma-separated)
        #[arg(short, long)]
        providers: Option<String>,

        /// Output results as JSON (one object per line)
        #[arg(short, long)]
        json: bool,
    },

    /// List available providers
    Providers,

    /// Activate an item (pipe JSON item to stdin)
    Activate {
        /// Action ID (optional)
        #[arg(short, long)]
        action: Option<String>,

        /// Output result as JSON
        #[arg(short, long)]
        json: bool,
    },
}

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

fn get_socket_path(arg: Option<PathBuf>) -> PathBuf {
    arg.unwrap_or_else(|| {
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
        PathBuf::from(runtime_dir).join("datacube.sock")
    })
}

fn send_message(stream: &mut UnixStream, msg_type: MessageType, body: &[u8]) -> std::io::Result<()> {
    let mut header = vec![msg_type as u8];
    header.extend_from_slice(&(body.len() as u32).to_be_bytes());
    stream.write_all(&header)?;
    stream.write_all(body)?;
    stream.flush()
}

fn recv_message(stream: &mut UnixStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    stream.read_exact(&mut header)?;

    let msg_type = header[0];
    let length = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;

    let mut body = vec![0u8; length];
    stream.read_exact(&mut body)?;

    Ok((msg_type, body))
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let socket_path = get_socket_path(args.socket);

    let mut stream = UnixStream::connect(&socket_path)
        .map_err(|e| anyhow::anyhow!("Failed to connect to {:?}: {}", socket_path, e))?;

    match args.command {
        Commands::Query { query, max, providers, json } => {
            let providers_list: Vec<String> = providers
                .map(|p| p.split(',').map(String::from).collect())
                .unwrap_or_default();

            let request = QueryRequest {
                query: query.clone(),
                max_results: max,
                providers: providers_list,
                exact: false,
            };

            send_message(&mut stream, MessageType::Query, &request.encode_to_vec())?;

            let (_, body) = recv_message(&mut stream)?;
            let response = QueryResponse::decode(body.as_slice())?;

            if json {
                // Output each item as a JSON object on its own line (NDJSON format)
                for item in response.items.iter() {
                    let json_item = serde_json::json!({
                        "id": item.id,
                        "text": item.text,
                        "subtext": item.subtext,
                        "icon": item.icon,
                        "provider": item.provider,
                        "score": item.score,
                        "exec": item.exec,
                        "metadata": item.metadata,
                        "actions": item.actions.iter().map(|a| serde_json::json!({
                            "id": a.id,
                            "name": a.name,
                            "icon": a.icon
                        })).collect::<Vec<_>>()
                    });
                    println!("{}", json_item);
                }
            } else {
                println!("Query: '{}' (qid: {})", response.query, response.qid);
                println!("Results: {}", response.items.len());
                println!();

                for (i, item) in response.items.iter().enumerate() {
                    println!(
                        "{}. {} [{}]",
                        i + 1,
                        item.text,
                        item.provider
                    );
                    if !item.subtext.is_empty() {
                        println!("   {}", item.subtext);
                    }
                    println!("   Score: {:.2}, Icon: {}", item.score, item.icon);
                    if !item.exec.is_empty() {
                        println!("   Exec: {}", item.exec);
                    }
                    if !item.actions.is_empty() {
                        let action_names: Vec<_> = item.actions.iter().map(|a| &a.name).collect();
                        println!("   Actions: {:?}", action_names);
                    }
                    println!();
                }
            }
        }

        Commands::Providers => {
            let request = ListProvidersRequest {};
            send_message(&mut stream, MessageType::ListProviders, &request.encode_to_vec())?;

            let (_, body) = recv_message(&mut stream)?;
            let response = ListProvidersResponse::decode(body.as_slice())?;

            println!("Providers:");
            for provider in response.providers {
                println!(
                    "  - {} (prefix: '{}', enabled: {})",
                    provider.name,
                    if provider.prefix.is_empty() { "none" } else { &provider.prefix },
                    provider.enabled
                );
                println!("    {}", provider.description);
            }
        }

        Commands::Activate { action, json } => {
            // Read JSON item from stdin
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input)?;

            let item: serde_json::Value = serde_json::from_str(&input)?;

            let proto_item = Item {
                id: item["id"].as_str().unwrap_or("").to_string(),
                text: item["text"].as_str().unwrap_or("").to_string(),
                subtext: item["subtext"].as_str().unwrap_or("").to_string(),
                icon: item["icon"].as_str().unwrap_or("").to_string(),
                provider: item["provider"].as_str().unwrap_or("").to_string(),
                score: item["score"].as_f64().unwrap_or(0.0) as f32,
                exec: item["exec"].as_str().unwrap_or("").to_string(),
                metadata: item["metadata"]
                    .as_object()
                    .map(|m| {
                        m.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                            .collect()
                    })
                    .unwrap_or_default(),
                actions: vec![],
            };

            let request = ActivateRequest {
                item: Some(proto_item),
                action_id: action.unwrap_or_default(),
            };

            send_message(&mut stream, MessageType::Activate, &request.encode_to_vec())?;

            let (_, body) = recv_message(&mut stream)?;
            let response = ActivateResponse::decode(body.as_slice())?;

            if json {
                let result = serde_json::json!({
                    "success": response.success,
                    "error": response.error
                });
                println!("{}", result);
            } else if response.success {
                println!("Activated successfully");
            } else {
                eprintln!("Activation failed: {}", response.error);
                std::process::exit(1);
            }
        }
    }

    Ok(())
}
