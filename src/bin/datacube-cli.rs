//! Datacube CLI - Command-line client for testing datacube
//!
//! Usage:
//!   datacube-cli query "firefox"
//!   datacube-cli query "=2+2"
//!   datacube-cli providers

use clap::{Parser, Subcommand};
use datacube::proto::{ListProvidersRequest, ListProvidersResponse, QueryRequest, QueryResponse};
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
}

/// Message types for the protocol
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
enum MessageType {
    Query = 1,
    #[allow(dead_code)]
    QueryResponse = 2,
    ListProviders = 5,
    #[allow(dead_code)]
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
        Commands::Query {
            query,
            max,
            providers,
            json,
        } => {
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
                // Output as a valid JSON array
                let items: Vec<_> = response.items.iter().map(|item| {
                    serde_json::json!({
                        "id": item.id,
                        "text": item.text,
                        "subtext": item.subtext,
                        "icon": item.icon,
                        "icon_path": item.icon_path,
                        "provider": item.provider,
                        "score": item.score,
                        "metadata": item.metadata,
                    })
                }).collect();
                println!("{}", serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_string()));
            } else {
                println!("Query: '{}' (qid: {})", response.query, response.qid);
                println!("Results: {}", response.items.len());
                println!();

                for (i, item) in response.items.iter().enumerate() {
                    println!("{}. {} [{}]", i + 1, item.text, item.provider);
                    if !item.subtext.is_empty() {
                        println!("   {}", item.subtext);
                    }
                    println!("   Score: {:.2}, Icon: {}", item.score, item.icon);
                    if !item.icon_path.is_empty() {
                        println!("   Icon path: {}", item.icon_path);
                    }
                    if !item.metadata.is_empty() {
                        println!("   Metadata: {:?}", item.metadata);
                    }
                    println!();
                }
            }
        }

        Commands::Providers => {
            let request = ListProvidersRequest {};
            send_message(
                &mut stream,
                MessageType::ListProviders,
                &request.encode_to_vec(),
            )?;

            let (_, body) = recv_message(&mut stream)?;
            let response = ListProvidersResponse::decode(body.as_slice())?;

            println!("Providers:");
            for provider in response.providers {
                println!(
                    "  - {} (prefix: '{}', enabled: {})",
                    provider.name,
                    if provider.prefix.is_empty() {
                        "none"
                    } else {
                        &provider.prefix
                    },
                    provider.enabled
                );
                println!("    {}", provider.description);
            }
        }
    }

    Ok(())
}
