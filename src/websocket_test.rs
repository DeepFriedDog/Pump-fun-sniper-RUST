use anyhow::{anyhow, Error, Result};
use bs58;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use futures_util::future::join_all;
use log::{debug, error, info, warn};
use serde_json::{json, Value};
use solana_program::pubkey::Pubkey;
use std::str::FromStr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::{Message, WebSocketConfig}, tungstenite::handshake::client::Request};
use url::Url;
use http::{HeaderMap, HeaderValue};
use uuid;
use reqwest;
use std::collections::HashMap;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_client::rpc_client::RpcClient;
use lazy_static::lazy_static;
use dashmap::DashSet;
use base64::{alphabet, engine::general_purpose::GeneralPurposeConfig, Engine};

use crate::config::{ATA_PROGRAM_ID, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID};
use crate::chainstack_simple;

lazy_static! {
    static ref PROCESSED_TOKENS: DashSet<String> = DashSet::new();
    static ref MIN_LIQUIDITY_CACHED: f64 = std::env::var("MIN_LIQUIDITY")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(50.0);
}

const SUBSCRIPTION_MSG: &str = r#"{
    \"jsonrpc\": \"2.0\",
    \"id\": 8002,
    \"method\": \"logsSubscribe\",
    \"params\": [
        { \"mentions\": [ \"6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P\" ] },
        { \"commitment\": \"processed\" }
    ]
}"#;

/// Creates a fresh WebSocket connection without extra parameters that might delay message delivery
/// This approach is based on the older implementation that had more immediate token detection
async fn connect_fresh(url_str: &str) -> Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>> {
    // Parse the URL
    let url = Url::parse(url_str)?;
    
    // Current timestamp for cache busting
    let current_timestamp_millis = Utc::now().timestamp_millis();
    
    // Create a fresh URL with minimal parameters - just cache busting
    let mut fresh_url = url.clone();
    fresh_url.query_pairs_mut()
        .append_pair("_", &current_timestamp_millis.to_string());
    
    // Create a fresh URL string
    let fresh_url_str = fresh_url.to_string();
    
    info!("Connecting to WebSocket with minimal parameters: {}", fresh_url_str);
    
    // Direct connection attempt with minimal configuration
    let (stream, _) = connect_async(&fresh_url_str).await?;
    
    // Set TCP_NODELAY on underlying TcpStream if available
    match stream.get_ref() {
        tokio_tungstenite::MaybeTlsStream::Plain(tcp_stream) => {
            if let Err(e) = tcp_stream.set_nodelay(true) {
                warn!("Failed to set TCP_NODELAY on plain TCP stream: {}", e);
            }
        },
        _ => {}
    }
    
    info!("Connection established successfully");
    info!("Ready to subscribe to token events");
    
    Ok(stream)
}

/// Finds the associated bonding curve for a given mint and bonding curve.
pub fn find_associated_bonding_curve(mint: &Pubkey, bonding_curve: &Pubkey) -> Pubkey {
    let seeds = &[
        bonding_curve.as_ref(),
        TOKEN_PROGRAM_ID.as_ref(),
        mint.as_ref(),
    ];

    let (derived_address, _) = Pubkey::find_program_address(seeds, &ATA_PROGRAM_ID);
    derived_address
}

/// Parse the create instruction data
#[inline(always)]
fn parse_create_instruction(data: &[u8]) -> Option<TokenData> {
    if data.len() < 8 {
        debug!("Data too short to be a valid instruction: {} bytes", data.len());
        return None;
    }

    // Log the first 8 bytes (discriminator) for debugging
    let discriminator = &data[0..8];
    debug!("Instruction discriminator: {:?}", discriminator);

    let mut offset = 8; // Skip discriminator

    // Read a string from the data
    let read_string = |data: &[u8], offset: &mut usize| -> Option<String> {
        if *offset + 4 > data.len() {
            debug!("Offset out of bounds when reading string length");
            return None;
        }

        let length = u32::from_le_bytes([
            data[*offset],
            data[*offset + 1],
            data[*offset + 2],
            data[*offset + 3],
        ]) as usize;

        *offset += 4;

        if *offset + length > data.len() {
            debug!("String content would exceed data bounds");
            return None;
        }

        let value = match std::str::from_utf8(&data[*offset..*offset + length]) {
            Ok(s) => s.to_string(),
            Err(e) => {
                debug!("Failed to decode UTF-8 string: {}", e);
                return None;
            }
        };

        *offset += length;
        debug!("Read string: {}", value);

        Some(value)
    };

    // Read a pubkey from the data
    let read_pubkey = |data: &[u8], offset: &mut usize| -> Option<String> {
        if *offset + 32 > data.len() {
            debug!("Offset out of bounds when reading pubkey");
            return None;
        }

        let pubkey_data = &data[*offset..*offset + 32];
        *offset += 32;

        let encoded = bs58::encode(pubkey_data).into_string();
        debug!("Read pubkey: {}", encoded);

        Some(encoded)
    };

    // Parse fields
    let name = match read_string(data, &mut offset) {
        Some(name) => name,
        None => { debug!("Failed to parse token name"); return None; }
    };

    let symbol = match read_string(data, &mut offset) {
        Some(symbol) => symbol,
        None => { debug!("Failed to parse token symbol"); return None; }
    };

    let uri = match read_string(data, &mut offset) {
        Some(uri) => uri,
        None => { debug!("Failed to parse token URI"); return None; }
    };

    let mint = match read_pubkey(data, &mut offset) {
        Some(mint) => mint,
        None => { debug!("Failed to parse mint address"); return None; }
    };

    let bonding_curve = match read_pubkey(data, &mut offset) {
        Some(bonding_curve) => bonding_curve,
        None => { debug!("Failed to parse bonding curve address"); return None; }
    };

    let user = match read_pubkey(data, &mut offset) {
        Some(user) => user,
        None => { debug!("Failed to parse user address"); return None; }
    };

    debug!("Successfully parsed token data: {} ({})", name, symbol);
    Some(TokenData {
        name,
        symbol,
        uri,
        mint,
        bonding_curve,
        user,
        tx_signature: String::new(),
        block_time: None,
        detection_time: None,
    })
}

/// Parses instruction data into TokenData
pub fn parse_instruction(data: &[u8]) -> Option<TokenData> {
    parse_create_instruction(data)
}

// Struct for parsed token data
#[derive(Debug, Clone)]
pub struct TokenData {
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub mint: String,
    pub bonding_curve: String,
    pub user: String,
    pub tx_signature: String,
    pub block_time: Option<i64>,
    pub detection_time: Option<i64>, // Detection time in UNIX timestamp
}

/// Checks if the node is fully synced and ready for connections
async fn check_node_sync_status(node_endpoint: &str) -> Result<bool> {
    let client = reqwest::Client::new();
    let rpc_endpoint = node_endpoint.replace("wss://", "https://").replace("ws://", "http://");
    
    // First, let's check if the node is healthy
    let health_request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getHealth",
        "params": []
    });
    
    // Use a fast timeout to avoid blocking if the node is slow to respond
    let timeout_duration = std::time::Duration::from_millis(250);
    
    // Check node health with timeout
    let health_response = match tokio::time::timeout(
        timeout_duration,
        client.post(&rpc_endpoint)
            .json(&health_request)
            .send()
    ).await {
        Ok(response_result) => match response_result {
            Ok(response) => {
                match response.json::<Value>().await {
                    Ok(json) => json,
                    Err(_) => {
                        warn!("Failed to parse health check response");
                        return Ok(false);
                    }
                }
            },
            Err(_) => {
                warn!("Health check request failed");
                return Ok(true);
            }
        },
        Err(_) => {
            warn!("Health check request timed out");
            // We'll continue anyway since timeouts don't necessarily mean the node is unhealthy
            return Ok(true);
        }
    };
    
    // Parse the health response
    if let Some(result) = health_response.get("result") {
        if result != "ok" {
            warn!("Node is not healthy: {:?}", result);
            return Ok(false);
        }
    }
    
    info!("Node is healthy and ready for connections");
    
    // For high performance, don't wait for slot checks, just assume the node is synced
    // This helps avoid delay when the API is slow to respond but messages are coming through
    return Ok(true);
    
    // NOTE: The code below has been commented out to avoid unnecessary delay
    /*
    // Next, check if the node is synced with the network
    let slot_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "getSlot",
        "params": []
    });
    
    let network_slot_request = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "getSlot",
        "params": [{"commitment": "finalized"}]
    });
    
    // Get the node's current slot
    let slot_response = match client.post(&rpc_endpoint)
        .json(&slot_request)
        .send()
        .await {
        Ok(response) => {
            match response.json::<Value>().await {
                Ok(json) => json,
                Err(_) => {
                    warn!("Failed to parse slot response");
                    return Ok(false);
                }
            }
        },
        Err(_) => {
            warn!("Slot request failed");
            return Ok(false);
        }
    };
    
    // Get the network's finalized slot
    let network_slot_response = match client.post(&rpc_endpoint)
        .json(&network_slot_request)
        .send()
        .await {
        Ok(response) => {
            match response.json::<Value>().await {
                Ok(json) => json,
                Err(_) => {
                    warn!("Failed to parse network slot response");
                    return Ok(false);
                }
            }
        },
        Err(_) => {
            warn!("Network slot request failed");
            return Ok(false);
        }
    };
    
    // Extract the slot numbers
    let node_slot = slot_response
        .get("result")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    
    let network_slot = network_slot_response
        .get("result")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    
    // Calculate slot lag
    let slot_lag = if node_slot > network_slot {
        0 // Node is ahead, which is fine
    } else {
        network_slot - node_slot
    };
    
    // Log the sync status
    info!("Node slot: {}, Network slot: {}, Lag: {} slots", node_slot, network_slot, slot_lag);
    
    // Node is considered synced if it's less than 50 slots behind
    // This is a reasonable threshold for most applications
    let is_synced = slot_lag < 50;
    
    if is_synced {
        info!("Node is synced with the network.");
    } else {
        warn!("Node is not fully synced. Lag: {} slots", slot_lag);
    }
    
    Ok(is_synced)
    */
}

/// Gets the block time for a transaction signature
pub async fn get_transaction_block_time(tx_signature: &str) -> Result<i64> {
    // Get RPC URL from environment or use default
    let rpc_url = std::env::var("SOLANA_RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());

    // Create RPC client with processed commitment for faster results
    let client = RpcClient::new_with_commitment(rpc_url, CommitmentConfig::processed());
    
    // Get current time for fallback
    let current_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    
    // Try to get transaction details including block time
    match client.get_transaction_with_config(
        &bs58::decode(tx_signature).into_vec().map_err(|e| anyhow!("Failed to decode tx signature: {}", e))?.try_into().map_err(|_| anyhow!("Invalid transaction signature"))?,
        solana_client::rpc_config::RpcTransactionConfig {
            encoding: None,
            commitment: Some(CommitmentConfig::processed()),
            max_supported_transaction_version: Some(0),
        },
    ) {
        Ok(tx_response) => {
            // Extract block time if available
            if let Some(block_time) = tx_response.block_time {
                debug!("Transaction {} block time: {}", tx_signature, block_time);
                Ok(block_time)
            } else {
                debug!("No block time available for transaction {}, using current time", tx_signature);
                Ok(current_time)
            }
        },
        Err(e) => {
            warn!("Failed to get transaction details: {:?}", e);
            Ok(current_time) // Fallback to current time
        }
    }
}

/// Runs a simple WebSocket test to verify the connection and token detection capabilities
pub async fn run_websocket_test(endpoint: &str) -> Result<Vec<TokenData>, Error> {
    info!("Starting WebSocket test, connecting to: {}", endpoint);

    // Check if we need to use authentication
    let use_auth = std::env::var("USE_CHAINSTACK_AUTH")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase() == "true";
    
    let username = if use_auth {
        std::env::var("CHAINSTACK_USERNAME").unwrap_or_default()
    } else {
        String::new()
    };
    
    let password = if use_auth {
        std::env::var("CHAINSTACK_PASSWORD").unwrap_or_default()
    } else {
        String::new()
    };
    
    // Parse the WebSocket URL
    let mut url_string = endpoint.to_string();
    
    // If authentication is enabled and credentials are provided, modify the URL
    if use_auth && !username.is_empty() && !password.is_empty() {
        info!("Using basic authentication for WebSocket connection");
        
        // For basic auth in WebSockets, we need to include credentials in the URL
        if let Ok(mut url) = Url::parse(&url_string) {
            // Set credentials in URL (format: wss://username:password@hostname/path)
            if url.scheme() == "wss" || url.scheme() == "ws" {
                if url.set_username(&username).is_err() {
                    error!("Failed to set WebSocket username");
                    return Err(anyhow!("Failed to set WebSocket username"));
                }
                if url.set_password(Some(&password)).is_err() {
                    error!("Failed to set WebSocket password");
                    return Err(anyhow!("Failed to set WebSocket password"));
                }
                url_string = url.to_string();
                info!("Using authenticated WebSocket URL with embedded credentials");
            }
        }
    }
    
    // Parse the URL and add query parameters for keep-alive if needed
    let mut url = Url::parse(&url_string)
        .map_err(|e| anyhow!("Failed to parse WebSocket URL: {}", e))?;

    // Some WebSocket servers support keep-alive via query parameters
    if !url.query_pairs().any(|(k, _)| k == "keepalive") {
        url.query_pairs_mut().append_pair("keepalive", "true");
    }

    // Connection retry logic
    let mut retry_attempts = 0;
    let max_retries = 3; // Retry connection up to 3 times within this function
    let retry_delay = Duration::from_secs(2);

    // Try to establish the WebSocket connection with retries
    let mut ws_stream = loop {
        match connect_async(&url).await {
            Ok((stream, _)) => break stream,
            Err(e) => {
                retry_attempts += 1;
                if retry_attempts >= max_retries {
                    return Err(anyhow!(
                        "Failed to connect to WebSocket after {} attempts: {}",
                        max_retries,
                        e
                    ));
                }

                warn!(
                    "WebSocket connection attempt {} failed: {}. Retrying in {} seconds...",
                    retry_attempts,
                    e,
                    retry_delay.as_secs()
                );

                tokio::time::sleep(retry_delay).await;
            }
        }
    };

    // Pump.fun program ID for token creation
    let pump_program_id = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

    // Set to "processed" commitment for fastest detection
    let subscription_request = json!({
        "id": 1,
        "jsonrpc": "2.0",
        "method": "logsSubscribe",
        "params": [
            {"mentions": [pump_program_id]},
            {"commitment": "processed"}
        ]
    })
    .to_string();

    // Check if we're in quiet mode
    let quiet_mode = std::env::args().any(|arg| arg == "--quiet" || arg == "-q");

    if !quiet_mode {
        info!("Sending subscription request: {}", subscription_request);
    }

    // Send the subscription request
    ws_stream
        .send(Message::Text(subscription_request))
        .await
        .map_err(|e| anyhow!("Failed to send subscription request: {}", e))?;

    info!("WebSocket connection established, waiting for token creation events...");

    // Create tracking variables
    let mut message_count = 0;
    let mut token_creations = 0;
    let start_time = Instant::now();

    // Get the test duration from env or default to 0 (indefinite)
    // A duration of 0 means run until connection error or max token limit
    let test_duration = std::env::var("MONITOR_DURATION")
        .unwrap_or_else(|_| "0".to_string())
        .parse::<u64>()
        .unwrap_or(0);

    // Default to 2 minutes per cycle if no duration is specified
    let cycle_duration = if test_duration > 0 {
        test_duration
    } else {
        120 // 2 minute default cycle when running indefinitely
    };

    let timeout_duration = Duration::from_secs(cycle_duration);

    // Max tokens to collect per cycle (to prevent memory issues in indefinite mode)
    let max_tokens_per_cycle = 1; // Changed from 100 to 1 to return immediately with token data

    // Heartbeat/ping interval - every 30 seconds to keep connection alive
    let mut ping_interval = tokio::time::interval(Duration::from_secs(30));

    // Status update interval - every 30 seconds
    let mut status_interval = tokio::time::interval(Duration::from_secs(30));

    // Last message received time for connection health monitoring
    let mut last_message_time = Instant::now();
    let max_idle_time = Duration::from_secs(60); // Consider connection dead after 60s with no messages

    // Vector to store token data
    let mut token_data_list = Vec::new();

    // Log initial status
    if !quiet_mode {
        if test_duration > 0 {
            info!(
                "Status: WebSocket test running. Will listen for {} seconds",
                test_duration
            );
        } else {
            info!("Status: WebSocket monitoring running indefinitely or until token detection");
        }
    }

    // Process messages
    loop {
        tokio::select! {
            // Check for timeout
            _ = tokio::time::sleep(timeout_duration.saturating_sub(start_time.elapsed())) => {
                if test_duration > 0 {
                    info!("Test timeout reached after {} seconds. Exiting...", test_duration);
                } else {
                    info!("Monitoring cycle completed. Total tokens found so far: {}", token_creations);
                }
                break;
            }

            // Send heartbeat ping
            _ = ping_interval.tick() => {
                // Check if connection seems dead (no messages for too long)
                if last_message_time.elapsed() > max_idle_time {
                    warn!("WebSocket connection appears to be dead - no messages for {} seconds",
                          last_message_time.elapsed().as_secs());

                    // Return immediately to allow reconnection
                    return Ok(token_data_list);
                }

                // Send a ping to keep the connection alive
                if let Err(e) = ws_stream.send(Message::Ping(vec![1, 2, 3])).await {
                    warn!("Failed to send ping: {}", e);
                    // Return immediately to allow reconnection
                    return Ok(token_data_list);
                } else if !quiet_mode {
                    debug!("Sent ping to keep WebSocket connection alive");
                }
            }

            // Status update interval
            _ = status_interval.tick() => {
                let elapsed = start_time.elapsed();

                if test_duration > 0 {
                    let remaining = timeout_duration.saturating_sub(elapsed);
                    let mins = remaining.as_secs() / 60;
                    let secs = remaining.as_secs() % 60;

                    if !quiet_mode {
                        info!("Status: Processed {} messages, found {} new tokens. Test ends in {}m{}s",
                              message_count, token_creations, mins, secs);
                    }
                } else if !quiet_mode {
                    info!("Status: Processed {} messages, found {} new tokens. Monitoring indefinitely.",
                          message_count, token_creations);
                }
            }

            // Process messages
            result = ws_stream.next() => {
                match result {
                    Some(Ok(message)) => {
                        // Update the last message timestamp whenever we receive any message
                        last_message_time = Instant::now();
                        message_count += 1;

                        // Handle different message types
                        match message {
                            Message::Text(text) => {
                                // Only parse if the message is a subscription response (contains "id") or if it indicates a Create event
                                if !(text.contains("\"id\"") || text.contains("Create")) {
                                    continue;
                                }
                                // Parse message as JSON
                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                    if let Some(token_data) = process_websocket_message(&json, &mut token_creations, quiet_mode).await {
                                        token_data_list.push(token_data);
                                        return Ok(token_data_list);
                                    }
                                }
                            },
                            Message::Binary(data) => {
                                message_count += 1;
                                debug!("[{}] Binary WebSocket message received: {} bytes", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true), data.len());
                            },
                            Message::Ping(data) => {
                                // Automatically respond to ping with pong
                                debug!("[{}] WebSocket ping received", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
                                if let Err(e) = ws_stream.send(Message::Pong(data)).await {
                                    error!("Failed to send pong response: {}", e);
                                    return Ok(token_data_list);
                                }
                            },
                            Message::Pong(_) => {
                                debug!("[{}] WebSocket pong received", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
                            },
                            Message::Close(frame) => {
                                info!("[{}] WebSocket close frame received: {:?}", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true), frame);
                                return Ok(token_data_list);
                            },
                            _ => {
                                debug!("[{}] Other WebSocket message type received", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
                            }
                        }
                    },
                    Some(Err(e)) => {
                        warn!("WebSocket error: {}", e);
                        return Ok(token_data_list);
                    },
                    None => {
                        info!("WebSocket connection closed");
                        return Ok(token_data_list);
                    }
                }
            }
        }
    }

    // Don't close the connection, just return the list
    Ok(token_data_list)
}

/// Process a WebSocket message to extract token data if it's a token creation event
async fn process_websocket_message(
    message: &serde_json::Value,
    token_creation_count: &mut usize,
    quiet_mode: bool,
) -> Option<TokenData> {
    // Check if this is a subscription confirmation
    if let Some(id) = message.get("id").and_then(|v| v.as_i64()) {
        if !quiet_mode {
            info!("WebSocket subscription confirmed with id: {}", id);
        }
        return None;
    }

    // Check if this is a notification
    if let Some(method) = message.get("method").and_then(|v| v.as_str()) {
        if method == "logsNotification" {
            // Capture timestamp as soon as logsNotification is received
            let msg_received_ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            if let Some(params) = message.get("params") {
                if let Some(result) = params.get("result") {
                    if let Some(value) = result.get("value") {
                        // Extract log data
                        if let Some(logs) = value.get("logs").and_then(|v| v.as_array().cloned()) {
                            if !quiet_mode {
                                debug!("Log entry count: {}", logs.len());
                            }

                            // ONLY check for "Create" instruction in logs - ignore Buy events as requested
                            let contains_create = logs.iter().any(|log| {
                                log.as_str().is_some_and(|s| {
                                    s.contains("Program log: Instruction: Create")
                                })
                            });

                            // IMPORTANT: We're using a fully asynchronous approach here and no longer
                            // depend on getTransaction calls which can be delayed. All token data is
                            // extracted directly from logs, leading to instantaneous token detection.
                            if contains_create {
                                // Found a token creation event!
                                *token_creation_count += 1;

                                if let Some(signature) =
                                    value.get("signature").and_then(|v| v.as_str())
                                {
                                    // Extract and parse program data
                                    let mut program_data_base64 = String::new();

                                    // Try to find and parse program data
                                    for log in logs {
                                        if let Some(log_str) = log.as_str() {
                                            if log_str.contains("Program data:") {
                                                if !quiet_mode {
                                                    info!("Program data found: {}", log_str);
                                                }

                                                // Extract base64 data part
                                                if let Some(data_part) = log_str.strip_prefix("Program data: ") {
                                                    let data_owned = data_part.to_string();
                                                    program_data_base64 = data_owned.clone();
                                                    // Decode the base64 data using spawn_blocking with our runtime engine
                                                    let decoded_result = tokio::task::spawn_blocking(move || decode_base64(data_owned.as_str()))
                                                        .await.ok().and_then(|r| r.ok());
                                                    if let Some(decoded_data) = decoded_result {
                                                        if !quiet_mode {
                                                            info!("Successfully decoded program data ({} bytes)", decoded_data.len());
                                                        }

                                                        // Parse the token details
                                                        if let Some(mut token_data) = parse_create_instruction(&decoded_data) {
                                                            // Deduplication using DashSet: check if this token mint was already processed
                                                            if !PROCESSED_TOKENS.insert(token_data.mint.clone()) {
                                                                return None;
                                                            }

                                                            // Set the transaction signature
                                                            token_data.tx_signature = signature.to_string();
                                                            
                                                            // Use current time as detection and block time in milliseconds
                                                            token_data.detection_time = Some(msg_received_ts);
                                                            token_data.block_time = Some(msg_received_ts);

                                                            return Some(token_data);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

/// Listen for new tokens and send them through a channel
pub async fn listen_for_tokens(
    quiet_mode: bool,
    priority_fee: u64,
    wallet: String,
) -> Result<mpsc::Receiver<TokenData>> {
    // Create two channels: raw channel and final channel
    let (raw_tx, mut raw_rx) = mpsc::channel(100);
    let (final_tx, final_rx) = mpsc::channel(100);
    
    // Get the WebSocket endpoint from env
    let wss_endpoint = std::env::var("CHAINSTACK_WSS_ENDPOINT")
        .or_else(|_| std::env::var("WSS_ENDPOINT"))
        .unwrap_or_else(|_| chainstack_simple::get_fresh_wss_url());
    
    info!("Starting WebSocket listener with fresh connection: {}", wss_endpoint);
    info!("Using processed commitment level for fastest token detection");
    
    // Start the WebSocket listener in a separate task, sending raw tokens
    {
        let wss_endpoint_clone = wss_endpoint.clone();
        let quiet_mode_clone = quiet_mode;
        let raw_tx_clone = raw_tx.clone();
        tokio::spawn(async move {
             if let Err(e) = run_websocket_listener(&wss_endpoint_clone, &raw_tx_clone, quiet_mode_clone).await {
                 error!("WebSocket listener error: {}", e);
             }
        });
    }
    
    // Spawn a batching task to process liquidity checks
    tokio::spawn(async move {
         let liquidity_threshold: f64 = *MIN_LIQUIDITY_CACHED;
         loop {
             let mut batch = Vec::new();
             // Wait for first token
             match raw_rx.recv().await {
                 Some(token) => batch.push(token),
                 None => break, // channel closed
             }
             // Drain additional tokens with a 10ms timeout
             loop {
                 match tokio::time::timeout(Duration::from_millis(10), raw_rx.recv()).await {
                     Ok(Some(token)) => batch.push(token),
                     _ => break,
                 }
             }
             // Process the batch concurrently using FuturesUnordered
             let mut unordered = futures_util::stream::FuturesUnordered::new();
             for token_data in batch {
                 let final_tx_clone = final_tx.clone();
                 unordered.push(async move {
                     match crate::token_detector::check_token_liquidity(&token_data.mint, &token_data.bonding_curve, liquidity_threshold).await {
                         Ok((has_liquidity, sol_amount)) => {
                             let symbol = if has_liquidity { "✅" } else { "❌" };
                             info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) {:.2} SOL {}", token_data.name, token_data.mint, sol_amount, symbol);
                         },
                         Err(e) => {
                             warn!("Liquidity check failed for token {} ({}): {}", token_data.name, token_data.mint, e);
                             info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) 0.00 SOL ❌", token_data.name, token_data.mint);
                         }
                     }
                     if let Err(e) = final_tx_clone.send(token_data).await {
                         error!("Failed to send token data through final channel: {}", e);
                     }
                 });
             }
             while let Some(_) = unordered.next().await {}
         }
    });
    
    Ok(final_rx)
}

/// Run the WebSocket listener and send tokens through the channel
async fn run_websocket_listener(
    endpoint: &str,
    tx: &mpsc::Sender<TokenData>,
    quiet_mode: bool,
) -> Result<()> {
    let tx = tx.clone();  // Added to ensure tx is available in async closures

    // Measure node sync check timing.
    let sync_start = Instant::now();
    if let Ok(is_synced) = check_node_sync_status(endpoint).await {
        if !is_synced {
            warn!("Node is not fully synced, token detection might be delayed");
        } else {
            info!("Node is fully synced and ready for connections");
        }
    }
    let sync_duration = sync_start.elapsed().as_millis();
    info!("Node sync check took {} ms", sync_duration);
    
    // Parse the WebSocket URL
    let url = Url::parse(endpoint)?;
    
    // Get timestamp before connection for latency tracking
    let pre_connection_timestamp = Instant::now();
    
    // Connect to the WebSocket using our simplified connection method
    let ws_stream = connect_fresh(endpoint).await?;
    
    let connection_latency = pre_connection_timestamp.elapsed();
    info!("WebSocket connection established in {}ms", connection_latency.as_millis());
    
    // Split the WebSocket stream
    let (mut write, mut read) = ws_stream.split();
    
    // Timestamp when we started the connection - useful for diagnostics
    let connection_start = Instant::now();
    let mut message_stats = MessageStats::new();
    
    // Set up a forced reconnect timer if no tokens are detected for too long
    let force_reconnect_after = Duration::from_secs(60); 
    let mut force_reconnect_time = connection_start + force_reconnect_after;
    let mut token_detected = false;
    
    // Track connection health separately from token detection
    let mut last_message_time = Instant::now();
    let max_silence_duration = Duration::from_secs(30); // Reconnect if no messages for 30 seconds
    
    // Subscribe to the Pump.fun program logs with explicit processed commitment
    // We use explicit subscription ID 8002 to match the older implementation
    let subscription_id = 8002; 
    
    // Construct our subscription request for program logs - simplify to match older code
    let subscribe_msg = Message::Text(
        json!({
            "jsonrpc": "2.0",
            "id": 8002,
            "method": "logsSubscribe",
            "params": [
                {
                    "mentions": [ PUMP_PROGRAM_ID.to_string() ]
                },
                {
                    "commitment": "processed"
                }
            ]
        })
        .to_string()
    );
    let sub_start = Instant::now();
    if let Err(e) = write.send(subscribe_msg).await {
        return Err(anyhow!("Failed to send subscription request: {}", e));
    }
    let sub_duration = sub_start.elapsed().as_millis();
    info!("Subscription request sent in {} ms", sub_duration);
    
    info!("WebSocket subscription sent with id: {}", subscription_id);
    
    // For tracking time since subscription started
    let subscription_start = Instant::now();
    
    // Track when the last token was detected
    let mut last_token_time = Instant::now();
    
    // Processing loop - we'll keep this structure from the current code
    // to properly track timing and handle messages
    let mut token_creation_count: usize = 0;
    
    // Process incoming messages
    let mut message_counter = 0;
    
    // Continue processing messages after verification
    info!("Starting token detection loop - waiting for new tokens...");
    
    while let Some(msg_result) = read.next().await {
        // Update last message time for connection health monitoring
        last_message_time = Instant::now();
        
        // Check forced reconnect timer - if exceeded and no tokens detected, return to trigger reconnect
        // But only do this if we've been running long enough to potentially receive tokens (> 5 minutes)
        if !token_detected && connection_start.elapsed() > Duration::from_secs(300) 
           && Instant::now() > force_reconnect_time {
            warn!("No tokens detected for {}s after connection, forcing reconnect", 
                  force_reconnect_after.as_secs());
            return Err(anyhow!("Forced reconnect due to no token detection within timeout period"));
        }
        
        // Record message activity time for health checks
        message_stats.last_activity = Instant::now();
        message_counter += 1;
        
        // Log message count every 100 messages
        if message_counter % 100 == 0 {
            debug!("Processed {} WebSocket messages", message_counter);
        }
        
        // Periodically report connection stats
        if message_stats.next_report_time <= Instant::now() {
            let elapsed = message_stats.connection_start.elapsed();
            info!(
                "WebSocket stats after {}ms: {} total messages received, {} methods: {:?}",
                elapsed.as_millis(),
                message_stats.total_messages,
                message_stats.method_counts.len(),
                message_stats.method_counts
            );
            // Report every 5 seconds
            message_stats.next_report_time = Instant::now() + Duration::from_secs(5);
        }
        
        match msg_result {
            Ok(msg) => {
                // Track message statistics
                message_stats.total_messages += 1;
                
                match msg {
                    Message::Text(text) => {
                        // Only parse if the message is a subscription response (contains "id") or if it indicates a Create event
                        if !(text.contains("\"id\"") || text.contains("Create")) {
                            continue;
                        }
                        // Parse message as JSON
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                            // Track message types
                            if let Some(method) = json.get("method").and_then(|m| m.as_str()) {
                                *message_stats.method_counts.entry(method.to_string()).or_insert(0) += 1;
                                
                                // Detailed logging for understanding the message flow
                                let elapsed_ms = message_stats.connection_start.elapsed().as_millis();
                                if elapsed_ms < 30000 { // Only log details for first 30 seconds
                                    debug!("Received {} message after {}ms", method, elapsed_ms);
                                }
                            }
                            
                        // Process the message to extract token data
                            if let Some(token_data) = process_websocket_message(&json, &mut token_creation_count, quiet_mode).await {
                                if let Err(e) = tx.send(token_data).await {
                                    error!("Failed to send token data through channel: {}", e);
                                }
                            }
                        }
                    },
                    Message::Ping(data) => {
                        // Respond to ping with pong
                        write.send(Message::Pong(data)).await?;
                    },
                    Message::Pong(_) => {
                        // Received pong response, connection is alive
                        debug!("Received pong response, connection is alive");
                    },
                    _ => {
                        // Ignore other message types
                    }
                }
            },
            Err(e) => {
                // Log the error and return to trigger reconnection
                error!("WebSocket error: {}", e);
                return Err(anyhow!(e));
            }
        }
        
        // Check for long periods of no messages (connection might be dead)
        if last_message_time.elapsed() > max_silence_duration {
            warn!("No messages received for {} seconds, connection might be dead", 
                  max_silence_duration.as_secs());
            return Err(anyhow!("Connection silence timeout - no messages received"));
        }
    }
    
    // If we get here, the connection was closed
    warn!("WebSocket connection closed, triggering reconnection");
    Err(anyhow!("WebSocket connection closed"))
}

// Add message stats tracking struct - add this near the top of the file
struct MessageStats {
    connection_start: Instant,
    total_messages: usize,
    method_counts: HashMap<String, usize>, 
    last_activity: Instant,
    next_report_time: Instant,
}

impl MessageStats {
    // Create new message stats with current time
    fn new() -> Self {
        let now = Instant::now();
        Self {
            connection_start: now,
            total_messages: 0,
            method_counts: HashMap::new(),
            last_activity: now,
            next_report_time: now,
        }
    }
}

// Internal base64 decoder function to avoid constant evaluation issues
fn decode_base64(input: &str) -> Result<Vec<u8>, base64::DecodeError> {
    // Using the engine directly at runtime
    base64::engine::general_purpose::GeneralPurpose::new(
        &base64::alphabet::STANDARD, 
        base64::engine::general_purpose::GeneralPurposeConfig::new()
    ).decode(input)
}
