use anyhow::{anyhow, Error, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use bs58;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use serde_json::{json, Value};
use solana_program::pubkey::Pubkey;
use std::str::FromStr;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::{Message, WebSocketConfig}, tungstenite::handshake::client::Request};
use url::Url;
use http::{HeaderMap, HeaderValue};
use uuid;

use crate::config::{ATA_PROGRAM_ID, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID};
use crate::chainstack_simple;

/// Creates a fresh WebSocket connection with custom headers to prevent history accumulation
async fn connect_fresh(url_str: &str) -> Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>> {
    // Parse the URL
    let url = Url::parse(url_str)?;
    
    // Create a fresh URL with a unique query parameter to force a new connection
    let mut fresh_url = url.clone();
    fresh_url.query_pairs_mut()
        .append_pair("fresh_session", &uuid::Uuid::new_v4().to_string());
    
    // Create a fresh URL string
    let fresh_url_str = fresh_url.to_string();
    
    // Use a custom WebSocket config with larger message size limits
    let config = WebSocketConfig {
        max_message_size: Some(64 << 20),     // 64 MB
        max_frame_size: Some(16 << 20),       // 16 MB
        accept_unmasked_frames: false,
        max_send_queue: Some(32),             // Maximum pending messages in outgoing queue
    };
    
    // Connect using the standard method but with our custom config
    // This handles all the protocol headers correctly
    let (stream, _) = tokio_tungstenite::connect_async_with_config(&fresh_url_str, Some(config)).await?;
    
    info!("Established fresh WebSocket connection with no history");
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
fn parse_create_instruction(data: &[u8]) -> Option<TokenData> {
    if data.len() < 8 {
        debug!(
            "Data too short to be a valid instruction: {} bytes",
            data.len()
        );
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
        None => {
            debug!("Failed to parse token name");
            return None;
        }
    };

    let symbol = match read_string(data, &mut offset) {
        Some(symbol) => symbol,
        None => {
            debug!("Failed to parse token symbol");
            return None;
        }
    };

    let uri = match read_string(data, &mut offset) {
        Some(uri) => uri,
        None => {
            debug!("Failed to parse token URI");
            return None;
        }
    };

    let mint = match read_pubkey(data, &mut offset) {
        Some(mint) => mint,
        None => {
            debug!("Failed to parse mint address");
            return None;
        }
    };

    let bonding_curve = match read_pubkey(data, &mut offset) {
        Some(bonding_curve) => bonding_curve,
        None => {
            debug!("Failed to parse bonding curve address");
            return None;
        }
    };

    let user = match read_pubkey(data, &mut offset) {
        Some(user) => user,
        None => {
            debug!("Failed to parse user address");
            return None;
        }
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
                if let Err(_) = url.set_username(&username) {
                    warn!("Failed to set username for WebSocket URL");
                }
                if let Err(_) = url.set_password(Some(&password)) {
                    warn!("Failed to set password for WebSocket URL");
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

    // Set to "confirmed" commitment for faster detection
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
                } else {
                    if !quiet_mode {
                        info!("Status: Processed {} messages, found {} new tokens. Monitoring indefinitely.",
                              message_count, token_creations);
                    }
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
                                // Parse message as JSON
                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                    if let Some(token_data) = process_websocket_message(&json, &mut token_creations, quiet_mode) {
                                        // Return immediately with the token to prevent delay
                                        token_data_list.push(token_data);

                                        // Return the token data immediately instead of waiting
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
fn process_websocket_message(
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
            if let Some(params) = message.get("params") {
                if let Some(result) = params.get("result") {
                    if let Some(value) = result.get("value") {
                        // Extract log data
                        if let Some(logs) = value.get("logs").and_then(|v| v.as_array()) {
                            if !quiet_mode {
                                info!("Log entry count: {}", logs.len());
                            }

                            // ONLY check for "Create" instruction in logs - ignore Buy events as requested
                            let contains_create = logs.iter().any(|log| {
                                log.as_str().map_or(false, |s| {
                                    s.contains("Program log: Instruction: Create")
                                })
                            });

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
                                                if let Some(data_part) =
                                                    log_str.strip_prefix("Program data: ")
                                                {
                                                    program_data_base64 = data_part.to_string();

                                                    // Decode the base64 data
                                                    if let Ok(decoded_data) =
                                                        BASE64.decode(data_part)
                                                    {
                                                        if !quiet_mode {
                                                            info!("Successfully decoded program data ({} bytes)", decoded_data.len());
                                                        }

                                                        // Parse the token details
                                                        if let Some(mut token_data) =
                                                            parse_create_instruction(&decoded_data)
                                                        {
                                                            // Set the transaction signature
                                                            token_data.tx_signature =
                                                                signature.to_string();

                                                            // Only proceed with valid tokens
                                                            // Check if the mint address ends with "pump" which indicates a valid token
                                                            if token_data.mint.ends_with("pump")
                                                                || token_data.name != "Unknown"
                                                            {
                                                                // Create a clone for async liquidity checking
                                                                let token_data_clone =
                                                                    token_data.clone();

                                                                // Spawn a task to check liquidity asynchronously
                                                                tokio::spawn(async move {
                                                                    // Get MIN_LIQUIDITY from environment variable
                                                                    let min_liquidity_str =
                                                                        std::env::var(
                                                                            "MIN_LIQUIDITY",
                                                                        )
                                                                        .unwrap_or_else(|_| {
                                                                            "4.0".to_string()
                                                                        });
                                                                    let min_liquidity =
                                                                        min_liquidity_str
                                                                            .parse::<f64>()
                                                                            .unwrap_or(4.0);

                                                                    // Use the token_detector function for liquidity check
                                                                    match crate::token_detector::check_token_liquidity(
                                                                        &token_data_clone.mint,
                                                                        &token_data_clone.bonding_curve,
                                                                        min_liquidity
                                                                    ).await {
                                                                        Ok((has_liquidity, sol_amount)) => {
                                                                            // Log the token creation with liquidity info
                                                                            let check_mark = if has_liquidity { "✅" } else { "❌" };
                                                                            
                                                                            info!("🪙 NEW TOKEN CREATED! {} (mint: {}) 💰 {:.2} SOL {}", 
                                                                                token_data_clone.name, 
                                                                                token_data_clone.mint, 
                                                                                sol_amount, 
                                                                                check_mark);
                                                                        },
                                                                        Err(e) => {
                                                                            // Log error and show token with 0 SOL
                                                                            if !quiet_mode {
                                                                                info!("Error checking liquidity: {}", e);
                                                                            }
                                                                            info!("🪙 NEW TOKEN CREATED! {} (mint: {}) 💰 0.00 SOL ❌", 
                                                                                token_data_clone.name, 
                                                                                token_data_clone.mint);
                                                                        }
                                                                    }
                                                                });

                                                                // Only show detailed logs if not in quiet mode
                                                                if !quiet_mode {
                                                                    // Display the extracted token details
                                                                    info!("=== NEWLY MINTED TOKEN DETAILS ===");
                                                                    info!(
                                                                        "Token Name: {}",
                                                                        token_data.name
                                                                    );
                                                                    info!(
                                                                        "Token Symbol: {}",
                                                                        token_data.symbol
                                                                    );
                                                                    info!(
                                                                        "Token URI: {}",
                                                                        token_data.uri
                                                                    );
                                                                    info!(
                                                                        "Mint Address: {}",
                                                                        token_data.mint
                                                                    );
                                                                    info!(
                                                                        "Bonding Curve Address: {}",
                                                                        token_data.bonding_curve
                                                                    );
                                                                    info!(
                                                                        "Creator Address: {}",
                                                                        token_data.user
                                                                    );

                                                                    // Calculate associated bonding curve
                                                                    if let Ok(mint_pubkey) =
                                                                        Pubkey::from_str(
                                                                            &token_data.mint,
                                                                        )
                                                                    {
                                                                        if let Ok(
                                                                            bonding_curve_pubkey,
                                                                        ) = Pubkey::from_str(
                                                                            &token_data
                                                                                .bonding_curve,
                                                                        ) {
                                                                            let associated_bonding_curve = find_associated_bonding_curve(&mint_pubkey, &bonding_curve_pubkey);
                                                                            info!("Associated Bonding Curve: {}", associated_bonding_curve);
                                                                        }
                                                                    }

                                                                    info!(
                                                                        "Transaction Signature: {}",
                                                                        signature
                                                                    );

                                                                    // Print a separator for readability
                                                                    info!("==================================================================");
                                                                }

                                                                return Some(token_data);
                                                            } else if !quiet_mode {
                                                                debug!("Filtered out invalid token with mint: {}", token_data.mint);
                                                            }
                                                        } else {
                                                            if !quiet_mode {
                                                                debug!("Failed to parse token data from decoded program data");
                                                            }
                                                        }
                                                    } else {
                                                        if !quiet_mode {
                                                            debug!("Failed to decode base64 program data");
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
    // Create a channel for sending token data
    let (tx, rx) = mpsc::channel(100);
    
    // Get the WebSocket endpoint with a fresh session ID to avoid accumulated history
    let wss_endpoint = std::env::var("WSS_ENDPOINT")
        .unwrap_or_else(|_| chainstack_simple::get_fresh_wss_url());
    
    info!("Starting WebSocket listener with fresh connection: {}", wss_endpoint);
    info!("Using processed commitment level for fastest token detection");
    
    // Start the WebSocket listener in a separate task
    tokio::spawn(async move {
        let mut consecutive_failures = 0;
        let initial_backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(60);
        let mut current_backoff = initial_backoff;
        let backoff_factor = 2.0;
        
        loop {
            match run_websocket_listener(&wss_endpoint, &tx, quiet_mode).await {
                Ok(_) => {
                    // Reset backoff on success
                    current_backoff = initial_backoff;
                    consecutive_failures = 0;
                    info!("WebSocket listener completed successfully");
                },
                Err(e) => {
                    consecutive_failures += 1;
                    error!("WebSocket listener error: {}. Reconnection attempt #{}", e, consecutive_failures);
                    
                    // Calculate exponential backoff
                    if consecutive_failures > 1 {
                        let backoff_secs = current_backoff.as_secs() * backoff_factor as u64;
                        current_backoff = std::cmp::min(Duration::from_secs(backoff_secs), max_backoff);
                    }
                    
                    info!("Reconnecting to WebSocket in {} seconds...", current_backoff.as_secs());
                    tokio::time::sleep(current_backoff).await;
                }
            }
        }
    });
    
    Ok(rx)
}

/// Run the WebSocket listener and send tokens through the channel
async fn run_websocket_listener(
    endpoint: &str,
    tx: &mpsc::Sender<TokenData>,
    quiet_mode: bool,
) -> Result<()> {
    // Parse the WebSocket URL
    let url = Url::parse(endpoint)?;
    
    // Connect to the WebSocket using our fresh connection method
    let ws_stream = connect_fresh(endpoint).await?;
    info!("WebSocket connection established with fresh session");
    
    // Split the WebSocket stream
    let (mut write, mut read) = ws_stream.split();
    
    // Subscribe to the Pump.fun program logs
    let subscribe_msg = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "logsSubscribe",
        "params": [
            {
                "mentions": [PUMP_PROGRAM_ID.to_string()]
            },
            {
                "commitment": "processed"
            }
        ]
    });
    
    // Send the subscription request
    write.send(Message::Text(subscribe_msg.to_string())).await?;
    info!("Sent subscription request for Pump.fun program logs");
    
    // Process incoming messages
    while let Some(msg_result) = read.next().await {
        match msg_result {
            Ok(msg) => {
                if let Message::Text(text) = msg {
                    // Parse the message as JSON
                    if let Ok(json) = serde_json::from_str::<Value>(&text) {
                        // Process the message to extract token data
                        if let Some(token_data) = process_websocket_message(&json, &mut 0, quiet_mode) {
                            // Send the token data through the channel
                            if let Err(e) = tx.send(token_data).await {
                                error!("Failed to send token data through channel: {}", e);
                            }
                        }
                    }
                }
            },
            Err(e) => {
                return Err(anyhow!("WebSocket error: {}", e));
            }
        }
    }
    
    Ok(())
}
