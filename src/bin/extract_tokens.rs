use anyhow::Result;
use base64::prelude::*;
use bs58;
use clap::Parser;
use dotenv::dotenv;
use futures_util::SinkExt;
use futures_util::StreamExt;
use log::{debug, error, info, warn};
use serde_json::{json, Value};
use solana_program::pubkey::Pubkey;
use std::env;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

// Import the websocket modules
use pumpfun_sniper::chainstack;
use pumpfun_sniper::websocket_reconnect;

// Constants for program IDs
const PUMP_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ATA_PROGRAM_ID: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize environment variables from .env file
    dotenv().ok();

    // Setup logging
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    // Check if DEBUG_WEBSOCKET_MESSAGES is set to true
    let debug_websocket = env::var("DEBUG_WEBSOCKET_MESSAGES")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase()
        == "true";

    // Get the WebSocket endpoint
    let chainstack_wss_endpoint = env::var("CHAINSTACK_WSS_ENDPOINT")
        .expect("CHAINSTACK_WSS_ENDPOINT environment variable is required");

    info!("Starting token extraction...");
    info!(
        "Using Chainstack WebSocket endpoint: {}",
        chainstack_wss_endpoint
    );

    // Setup max idle time before reconnection
    let max_idle_time = Duration::from_secs(60);
    info!(
        "Max idle time before reconnection: {} seconds",
        max_idle_time.as_secs()
    );

    // Connect to WebSocket with our enhanced token detection
    extract_tokens_with_websocket(&chainstack_wss_endpoint, !debug_websocket, max_idle_time).await?;

    info!("Token extraction completed successfully");
    Ok(())
}

/// Extract tokens using WebSocket
async fn extract_tokens_with_websocket(
    endpoint: &str,
    quiet_mode: bool,
    max_idle_time: Duration,
) -> Result<()> {
    // Connect to WebSocket
    let mut ws_stream = websocket_reconnect::connect_with_retry(endpoint, quiet_mode).await?;

    // Subscribe to Pump.fun program
    let subscription_message = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "logsSubscribe",
        "params": [
            { "mentions": [PUMP_PROGRAM_ID] },
            { "commitment": "processed" }
        ]
    })
    .to_string();

    ws_stream.send(Message::Text(subscription_message)).await?;
    info!("Sent subscription request for Pump.fun program");

    // Set up a ping timer to keep the connection alive
    let mut ping_timer = tokio::time::interval(Duration::from_secs(30));
    let mut last_message_time = std::time::Instant::now();
    
    // Message counters for diagnostic purposes
    let mut total_messages = 0;
    let mut token_creation_events = 0;
    let mut last_stats_time = std::time::Instant::now();

    // Create a channel for shutdown notification
    let (shutdown_complete_tx, mut shutdown_complete_rx) = tokio::sync::mpsc::channel(1);
    let shutdown_complete_tx = Arc::new(Mutex::new(Some(shutdown_complete_tx)));

    // Setup Ctrl+C handler
    let ctrl_c_shutdown_tx = shutdown_complete_tx.clone();
    tokio::spawn(async move {
        if let Ok(()) = tokio::signal::ctrl_c().await {
            info!("Received Ctrl+C signal, initiating graceful shutdown");
            let tx_opt = {
                let mut guard = ctrl_c_shutdown_tx.lock().unwrap();
                guard.take()
            };
            if let Some(tx) = tx_opt {
                let _ = tx.send(()).await;
            }
        }
    });

    // Process messages
    loop {
        tokio::select! {
            // Handle WebSocket messages
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // Update activity tracking
                        last_message_time = std::time::Instant::now();
                        total_messages += 1;
                        
                        // Only log full notification data in debug mode
                        if text.contains("logsNotification") {
                            if !quiet_mode {
                                info!("Received logs notification");
                            }
                            
                            // Only parse if this might be a token creation event
                            // This is a quick check to avoid parsing all messages
                            if text.contains("Instruction: Create") {
                                // Try to parse the message as JSON
                                if let Ok(json_data) = serde_json::from_str::<Value>(&text) {
                                    // Check for Create instruction in logs
                                    if let Some(logs) = json_data
                                        .get("params")
                                        .and_then(|p| p.get("result"))
                                        .and_then(|r| r.get("value"))
                                        .and_then(|v| v.get("logs"))
                                        .and_then(|l| l.as_array())
                                    {
                                        // If we find a Create instruction, process the token
                                        if logs.iter().any(|log| {
                                            log.as_str()
                                                .map(|s| s.contains("Program log: Instruction: Create"))
                                                .unwrap_or(false)
                                        }) {
                                            token_creation_events += 1;
                                            
                                            // Extract signature and log
                                            let signature = json_data
                                                .get("params")
                                                .and_then(|p| p.get("result"))
                                                .and_then(|r| r.get("value"))
                                                .and_then(|v| v.get("signature"))
                                                .and_then(|s| s.as_str())
                                                .unwrap_or("unknown");
                                                
                                            info!("Found token creation event!");
                                            info!("Token creation event signature: {}", signature);

                                            // Look for program data in logs
                                            for log in logs {
                                                if let Some(log_str) = log.as_str() {
                                                    if log_str.starts_with("Program data:") {
                                                        match log_str.split_once(": ") {
                                                            Some((_, encoded_data)) => {
                                                                // Log program data only in verbose mode
                                                                if !quiet_mode {
                                                                    info!("Found program data: {}", encoded_data);
                                                                }
                                                                
                                                                // Parse the token data
                                                                match parse_program_data(encoded_data) {
                                                                    Ok((name, symbol, mint, bonding_curve)) => {
                                                                        info!("================== NEW TOKEN DETECTED ==================");
                                                                        info!("Token Name: {} ({})", name, symbol);
                                                                        info!("Mint Address: {}", mint);
                                                                        info!("Bonding Curve: {}", bonding_curve);
                                                                        
                                                                        // Try to find associated bonding curve
                                                                        if let (Ok(mint_pubkey), Ok(curve_pubkey)) = (
                                                                            Pubkey::from_str(&mint),
                                                                            Pubkey::from_str(&bonding_curve),
                                                                        ) {
                                                                            if let Ok(associated_curve) = find_associated_bonding_curve(
                                                                                &mint_pubkey,
                                                                                &curve_pubkey,
                                                                            ) {
                                                                                info!("Associated Bonding Curve: {}", associated_curve);
                                                                                
                                                                                // Immediately subscribe to the bonding curve account for real-time updates
                                                                                let curve_str = curve_pubkey.to_string();
                                                                                
                                                                                // Use tokio spawn to handle the subscription without blocking
                                                                                let curve_string = curve_str.clone();
                                                                                tokio::spawn(async move {
                                                                                    debug!("Attempting to subscribe to bonding curve: {}", curve_string);
                                                                                    
                                                                                    // Just use the subscribe_to_account function directly
                                                                                    if let Err(e) = pumpfun_sniper::chainstack::subscribe_to_account(&curve_string).await {
                                                                                        error!("Failed to subscribe to bonding curve account: {}", e);
                                                                                    } else {
                                                                                        info!("Subscribed to bonding curve account for real-time updates: {}", curve_string);
                                                                                    }
                                                                                });
                                                                            }
                                                                        }
                                                                        info!("========================================================");
                                                                    }
                                                                    Err(e) => {
                                                                        debug!("Failed to parse program data: {}", e);
                                                                    }
                                                                }
                                                            }
                                                            None => {}
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
                    Some(Ok(Message::Binary(_))) => {
                        debug!("Received binary message");
                    }
                    Some(Ok(Message::Ping(data))) => {
                        debug!("Received ping, responding with pong");
                        ws_stream.send(Message::Pong(data)).await?;
                    }
                    Some(Ok(Message::Pong(_))) => {
                        debug!("Received pong response");
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("WebSocket closed");
                        break;
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Frame(_))) => {
                        debug!("Received frame message (unusual)");
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error: {}", e);
                        break;
                    }
                    None => {
                        info!("WebSocket stream ended");
                        break;
                    }
                }
            },
            
            // Periodically send pings to keep the connection alive
            _ = ping_timer.tick() => {
                // Send a ping message
                let ping_message = json!({
                    "jsonrpc": "2.0",
                    "id": 99,
                    "method": "ping"
                }).to_string();
                
                if let Err(e) = ws_stream.send(Message::Text(ping_message)).await {
                    error!("Failed to send ping: {}", e);
                    break;
                }
                
                // Check for inactivity
                if last_message_time.elapsed() > max_idle_time {
                    info!("No messages received for {} seconds, reconnecting...", max_idle_time.as_secs());
                    break;
                }
                
                // Print stats every 5 minutes
                if last_stats_time.elapsed() > Duration::from_secs(300) {
                    info!("Status update - Processed {} messages, found {} token creation events", 
                          total_messages, token_creation_events);
                    last_stats_time = std::time::Instant::now();
                }
            },
            // Check for shutdown signal
            _ = shutdown_complete_rx.recv() => {
                info!("Shutdown signal received, closing WebSocket connection");
                // Close the WebSocket connection
                let _ = ws_stream.close(None).await;
                break;
            }
        }
    }

    info!("WebSocket connection closed, exiting gracefully");
    Ok(())
}

/// Parse program data from a base64 encoded string
fn parse_program_data(encoded_data: &str) -> Result<(String, String, String, String)> {
    // Try to decode base64 data first, then try bs58 if that fails
    let decoded_data = BASE64_STANDARD
        .decode(encoded_data)
        .or_else(|_| bs58::decode(encoded_data).into_vec())
        .map_err(|e| anyhow::anyhow!("Failed to decode program data: {}", e))?;

    // Check if we have enough data (at least for the 8-byte discriminator)
    if decoded_data.len() < 8 {
        return Err(anyhow::anyhow!("Program data too short"));
    }

    // Skip the 8-byte instruction discriminator
    let mut offset = 8;
    let mut name = String::new();
    let mut symbol = String::new();
    let mut _uri = String::new(); // prefix with _ to indicate it's intentionally unused
    let mut mint = String::new();
    let mut bonding_curve = String::new();

    // Parse string fields (name, symbol, uri)
    for field_name in &["name", "symbol", "uri"] {
        if offset + 4 > decoded_data.len() {
            return Err(anyhow::anyhow!(
                "Unexpected end of data parsing {}",
                field_name
            ));
        }

        // Read string length (little endian u32)
        let length = u32::from_le_bytes([
            decoded_data[offset],
            decoded_data[offset + 1],
            decoded_data[offset + 2],
            decoded_data[offset + 3],
        ]) as usize;
        offset += 4;

        if offset + length > decoded_data.len() {
            return Err(anyhow::anyhow!(
                "String data out of bounds for {}",
                field_name
            ));
        }

        // Read string data
        let str_value = String::from_utf8(decoded_data[offset..offset + length].to_vec())
            .map_err(|e| anyhow::anyhow!("Invalid UTF-8 in {}: {}", field_name, e))?;
        offset += length;

        // Set field value
        match *field_name {
            "name" => name = str_value,
            "symbol" => symbol = str_value,
            "uri" => _uri = str_value,
            _ => {}
        }
    }

    // Parse pubkey fields (mint, bondingCurve)
    for field_name in &["mint", "bondingCurve"] {
        if offset + 32 > decoded_data.len() {
            return Err(anyhow::anyhow!(
                "Unexpected end of data parsing {}",
                field_name
            ));
        }

        // Extract 32-byte pubkey
        let pubkey_bytes = &decoded_data[offset..offset + 32];
        let pubkey_str = bs58::encode(pubkey_bytes).into_string();
        offset += 32;

        // Set field value
        match *field_name {
            "mint" => mint = pubkey_str,
            "bondingCurve" => bonding_curve = pubkey_str,
            _ => {}
        }
    }

    // Skip user pubkey
    if offset + 32 <= decoded_data.len() {
        // Remove the unused assignment
    }

    Ok((name, symbol, mint, bonding_curve))
}

/// Find the associated bonding curve for a given mint and bonding curve.
fn find_associated_bonding_curve(mint: &Pubkey, bonding_curve: &Pubkey) -> Result<Pubkey> {
    let token_program = Pubkey::from_str(TOKEN_PROGRAM_ID)?;
    let ata_program = Pubkey::from_str(ATA_PROGRAM_ID)?;

    let seeds = &[
        bonding_curve.as_ref(),
        token_program.as_ref(),
        mint.as_ref(),
    ];

    let (derived_address, _) = Pubkey::find_program_address(seeds, &ata_program);

    Ok(derived_address)
}
