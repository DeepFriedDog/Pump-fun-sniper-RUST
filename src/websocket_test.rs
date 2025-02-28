use anyhow::{Result, Error, anyhow};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use bs58;
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use serde_json::{json, Value};
use solana_program::pubkey::Pubkey;
use std::str::FromStr;
use std::time::{Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;
use chrono::Utc;

use crate::config::{ATA_PROGRAM_ID, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID};

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
            data[*offset + 3]
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
    
    // Connect to the WebSocket
    let (mut ws_stream, _) = connect_async(endpoint).await?;
    
    // Pump.fun program ID for token creation
    let pump_program_id = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
    
    // Set to "processed" commitment for faster detection
    let subscription_request = json!({
        "id": 1,
        "jsonrpc": "2.0",
        "method": "logsSubscribe",
        "params": [
            {"mentions": [pump_program_id]},
            {"commitment": "processed"}
        ]
    }).to_string();
    
    info!("Sending subscription request: {}", subscription_request);
    ws_stream.send(Message::Text(subscription_request)).await?;
    
    info!("WebSocket connection established, waiting for token creation events...");
    
    // Create tracking variables
    let mut message_count = 0;
    let mut token_creations = 0;
    let start_time = Instant::now();
    
    // Get the test duration from env or default to 2 minutes
    let test_duration = std::env::var("MONITOR_DURATION")
        .unwrap_or_else(|_| "120".to_string())
        .parse::<u64>()
        .unwrap_or(120);
    
    let timeout_duration = Duration::from_secs(test_duration);
    
    // Status update interval - every 30 seconds
    let mut status_interval = tokio::time::interval(Duration::from_secs(30));
    
    // Vector to store token data
    let mut token_data_list = Vec::new();
    
    // Log initial status
    info!("Status: WebSocket test running. Will listen for {} seconds", 
          test_duration);
          
    // Process messages
    loop {
        tokio::select! {
            // Check for timeout
            _ = tokio::time::sleep(timeout_duration.saturating_sub(start_time.elapsed())) => {
                info!("Test timeout reached after {} seconds. Exiting...", test_duration);
                break;
            }
            
            // Status update interval
            _ = status_interval.tick() => {
                let elapsed = start_time.elapsed();
                let remaining = timeout_duration.saturating_sub(elapsed);
                let mins = remaining.as_secs() / 60;
                let secs = remaining.as_secs() % 60;
                
                info!("Status: Processed {} messages, found {} new tokens. Test ends in {}m{}s", 
                      message_count, token_creations, mins, secs);
            }
            
            // Process messages
            result = ws_stream.next() => {
                match result {
                    Some(Ok(msg)) => {
                        let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                        
                        match msg {
                            Message::Text(text) => {
                                message_count += 1;
                                // Debug level for raw messages to reduce log spam
                                debug!("[{}] Raw WebSocket message received: {}", timestamp, text);
                                
                                // Parse and process message
                                if let Ok(json_msg) = serde_json::from_str::<serde_json::Value>(&text) {
                                    if let Some(token_data) = process_websocket_message(&json_msg, &mut token_creations) {
                                        // Just collect the token data, don't take any action
                                        token_data_list.push(token_data);
                                    }
                                } else {
                                    debug!("Failed to parse WebSocket message as JSON");
                                }
                            }
                            Message::Binary(data) => {
                                message_count += 1;
                                debug!("[{}] Binary WebSocket message received: {} bytes", timestamp, data.len());
                            }
                            Message::Ping(data) => {
                                // Automatically respond to ping with pong
                                debug!("[{}] WebSocket ping received", timestamp);
                                if let Err(e) = ws_stream.send(Message::Pong(data)).await {
                                    error!("Failed to send pong response: {}", e);
                                }
                            }
                            Message::Pong(_) => {
                                debug!("[{}] WebSocket pong received", timestamp);
                            }
                            Message::Close(frame) => {
                                info!("[{}] WebSocket close frame received: {:?}", timestamp, frame);
                                break;
                            }
                            _ => {
                                debug!("[{}] Other WebSocket message type received", timestamp);
                            }
                        }
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error: {}", e);
                        break;
                    }
                    None => {
                        info!("WebSocket connection closed by server");
                        break;
                    }
                }
            }
        }
    }
    
    // Close connection by sending a close frame
    if let Err(e) = ws_stream.send(Message::Close(None)).await {
        warn!("Failed to send close frame: {}", e);
    }
    
    info!("WebSocket test complete. Processed {} messages, found {} new tokens.", 
         message_count, token_creations);
    
    Ok(token_data_list)
}

/// Process a WebSocket message to extract token data if it's a token creation event
fn process_websocket_message(message: &serde_json::Value, token_creation_count: &mut usize) -> Option<TokenData> {
    // Check if this is a subscription confirmation
    if let Some(id) = message.get("id").and_then(|v| v.as_i64()) {
        info!("WebSocket subscription confirmed with id: {}", id);
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
                            info!("Log entry count: {}", logs.len());
                            
                            // ONLY check for "Create" instruction in logs - ignore Buy events as requested
                            let contains_create = logs.iter()
                                .any(|log| log.as_str()
                                    .map_or(false, |s| s.contains("Program log: Instruction: Create")));
                            
                            if contains_create {
                                // Found a token creation event!
                                *token_creation_count += 1;
                                
                                if let Some(signature) = value.get("signature").and_then(|v| v.as_str()) {
                                    info!("🪙 NEW TOKEN CREATED! Transaction signature: {}", signature);
                                    
                                    // Extract and parse program data
                                    let mut program_data_base64 = String::new();
                                    
                                    // Try to find and parse program data
                                    for log in logs {
                                        if let Some(log_str) = log.as_str() {
                                            if log_str.contains("Program data:") {
                                                info!("Program data found: {}", log_str);
                                                
                                                // Extract base64 data part
                                                if let Some(data_part) = log_str.strip_prefix("Program data: ") {
                                                    program_data_base64 = data_part.to_string();
                                                    
                                                    // Decode the base64 data
                                                    if let Ok(decoded_data) = BASE64.decode(data_part) {
                                                        info!("Successfully decoded program data ({} bytes)", decoded_data.len());
                                                        
                                                        // Parse the token details
                                                        if let Some(mut token_data) = parse_create_instruction(&decoded_data) {
                                                            // Set the transaction signature
                                                            token_data.tx_signature = signature.to_string();
                                                            
                                                            // Display the extracted token details
                                                            info!("=== NEWLY MINTED TOKEN DETAILS ===");
                                                            info!("Token Name: {}", token_data.name);
                                                            info!("Token Symbol: {}", token_data.symbol);
                                                            info!("Token URI: {}", token_data.uri);
                                                            info!("Mint Address: {}", token_data.mint);
                                                            info!("Bonding Curve Address: {}", token_data.bonding_curve);
                                                            info!("Creator Address: {}", token_data.user);
                                                            
                                                            // Calculate associated bonding curve
                                                            if let Ok(mint_pubkey) = Pubkey::from_str(&token_data.mint) {
                                                                if let Ok(bonding_curve_pubkey) = Pubkey::from_str(&token_data.bonding_curve) {
                                                                    let associated_bonding_curve = find_associated_bonding_curve(&mint_pubkey, &bonding_curve_pubkey);
                                                                    info!("Associated Bonding Curve: {}", associated_bonding_curve);
                                                                }
                                                            }
                                                            
                                                            info!("Transaction Signature: {}", signature);
                                                            
                                                            // Print a separator for readability
                                                            info!("==================================================================");
                                                            
                                                            return Some(token_data);
                                                        } else {
                                                            warn!("Failed to parse token data from decoded program data");
                                                        }
                                                    } else {
                                                        warn!("Failed to decode base64 program data");
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    
                                    // Print a separator for readability
                                    info!("==================================================================");
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