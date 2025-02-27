use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use bs58;
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use serde_json::{json, Value};
use solana_program::pubkey::Pubkey;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;

use crate::chainstack::{self, NewToken, WEBSOCKET_MESSAGES};
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

/// Parses the create instruction data
fn parse_create_instruction(data: &[u8]) -> Option<TokenData> {
    if data.len() < 8 {
        debug!("Data too short to be a valid instruction: {} bytes", data.len());
        return None;
    }

    let mut offset = 8; // Skip discriminator

    let read_string = |data: &[u8], offset: &mut usize| -> Option<String> {
        if *offset + 4 > data.len() {
            debug!("Offset out of bounds when reading string length: offset={}, len={}", *offset, data.len());
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
            debug!("String content would exceed data bounds: offset={}, length={}, data_len={}", 
                   *offset, length, data.len());
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

    let read_pubkey = |data: &[u8], offset: &mut usize| -> Option<String> {
        if *offset + 32 > data.len() {
            debug!("Offset out of bounds when reading pubkey: offset={}, len={}", *offset, data.len());
            return None;
        }
        
        let pubkey_data = &data[*offset..*offset + 32];
        *offset += 32;
        
        let encoded = bs58::encode(pubkey_data).into_string();
        debug!("Read pubkey: {}", encoded);
        
        Some(encoded)
    };

    // Parse fields
    debug!("Parsing token data starting at offset {}", offset);
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

    info!("Successfully parsed token data: {} ({})", name, symbol);
    Some(TokenData {
        name,
        symbol, 
        uri,
        mint,
        bonding_curve,
        user,
    })
}

// Struct for parsed token data
#[derive(Debug, Clone)]
struct TokenData {
    name: String,
    symbol: String,
    uri: String,
    mint: String, 
    bonding_curve: String,
    user: String,
}

// Convert TokenData to the existing NewToken structure for compatibility
impl From<TokenData> for NewToken {
    fn from(token: TokenData) -> Self {
        NewToken {
            token_name: token.name,
            token_symbol: token.symbol,
            mint_address: token.mint,
            creator_address: token.user,
            transaction_signature: String::new(), // Will be filled from log data
            timestamp: chrono::Utc::now().timestamp(),
        }
    }
}

/// Start listening for new tokens using WebSocket
pub async fn listen_for_new_tokens(wss_endpoint: String) -> Result<()> {
    info!("Starting enhanced WebSocket listener for new tokens");
    
    loop {
        match connect_to_websocket(&wss_endpoint).await {
            Ok(_) => {
                // Connection closed normally, wait before reconnecting
                warn!("WebSocket connection closed, reconnecting in 5 seconds...");
            }
            Err(e) => {
                error!("WebSocket connection error: {}", e);
                warn!("Reconnecting in 5 seconds...");
            }
        }
        
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn connect_to_websocket(wss_endpoint: &str) -> Result<()> {
    // Connect to the WebSocket server
    let url = Url::parse(wss_endpoint)?;
    info!("Connecting to {}", url);
    
    let (ws_stream, _) = connect_async(url).await?;
    info!("WebSocket connection established");
    
    let (mut write, mut read) = ws_stream.split();
    
    // Subscribe to logs
    let program_id = PUMP_PROGRAM_ID.to_string();
    info!("Monitoring program: {}", program_id);
    
    let subscription_message = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "logsSubscribe",
        "params": [
            {"mentions": [program_id]},
            {"commitment": "processed"}
        ]
    });
    
    write.send(Message::Text(subscription_message.to_string())).await?;
    info!("Listening for new token creations from program: {}", program_id);
    
    // Process subscription confirmation
    if let Some(Ok(message)) = read.next().await {
        match message {
            Message::Text(text) => {
                info!("Subscription response: {}", text);
            }
            _ => {
                warn!("Unexpected message format for subscription confirmation");
            }
        }
    }
    
    // Set up ping timer to keep connection alive
    let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
    
    // Process incoming messages
    loop {
        tokio::select! {
            // Send periodic pings to keep connection alive
            _ = ping_interval.tick() => {
                let ping_message = json!({
                    "jsonrpc": "2.0",
                    "id": 99,
                    "method": "ping"
                });
                
                if let Err(e) = write.send(Message::Text(ping_message.to_string())).await {
                    error!("Failed to send ping: {}", e);
                    return Err(anyhow!("Ping failed"));
                }
                
                debug!("Ping sent to WebSocket server");
            }
            
            // Process next message
            next_message = read.next() => {
                match next_message {
                    Some(Ok(Message::Text(text))) => {
                        debug!("Received WebSocket message");
                        // Process the message
                        if let Err(e) = process_message(&text, &WEBSOCKET_MESSAGES).await {
                            warn!("Error processing message: {}", e);
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        // Automatically respond to pings
                        if let Err(e) = write.send(Message::Pong(data)).await {
                            error!("Failed to send pong: {}", e);
                            return Err(anyhow!("Pong failed"));
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("WebSocket connection closed by server");
                        return Ok(());
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error: {}", e);
                        return Err(anyhow!("WebSocket error: {}", e));
                    }
                    None => {
                        info!("WebSocket stream ended");
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn process_message(text: &str, queue: &Arc<Mutex<std::collections::VecDeque<String>>>) -> Result<()> {
    // Parse the message
    let data: Value = serde_json::from_str(text)?;
    
    // Check if this is a logs notification
    if let Some("logsNotification") = data.get("method").and_then(Value::as_str) {
        if let Some(log_data) = data.get("params").and_then(|p| p.get("result")).and_then(|r| r.get("value")) {
            let signature = log_data.get("signature").and_then(Value::as_str).unwrap_or("Unknown");
            
            // Extract logs
            if let Some(logs) = log_data.get("logs").and_then(Value::as_array) {
                // Check if this is a create instruction
                let has_create = logs.iter().any(|log| {
                    log.as_str().map_or(false, |s| s.contains("Program log: Instruction: Create"))
                });
                
                if has_create {
                    debug!("Found Create instruction in transaction {}", signature);
                    
                    // Look for program data
                    for log in logs {
                        if let Some(log_str) = log.as_str() {
                            if log_str.contains("Program data:") {
                                debug!("Found Program data log: {}", log_str);
                                
                                // Extract the base64-encoded data - making sure to get the data part only
                                let parts: Vec<&str> = log_str.split("Program data: ").collect();
                                if parts.len() < 2 {
                                    debug!("Couldn't split Program data");
                                    continue;
                                }
                                
                                let encoded_data = parts[1].trim();
                                debug!("Extracted encoded data: {}", encoded_data);
                                
                                // Try to decode the base64 data
                                match BASE64.decode(encoded_data) {
                                    Ok(decoded_data) => {
                                        debug!("Successfully decoded Program data ({} bytes)", decoded_data.len());
                                        
                                        // Parse the instruction
                                        if let Some(token_data) = parse_create_instruction(&decoded_data) {
                                            info!("Detected new token creation:");
                                            info!("  Signature: {}", signature);
                                            info!("  Name: {}", token_data.name);
                                            info!("  Symbol: {}", token_data.symbol);
                                            info!("  Mint: {}", token_data.mint);
                                            info!("  Creator: {}", token_data.user);
                                            
                                            // Save mint and bonding curve before token_data is moved
                                            let mint_str = token_data.mint.clone();
                                            let bonding_curve_str = token_data.bonding_curve.clone();
                                            
                                            // Convert to NewToken and add to the queue
                                            let mut new_token: NewToken = token_data.into();
                                            new_token.transaction_signature = signature.to_string();
                                            
                                            // Add message to global queue for processing
                                            let mut queue_guard = queue.lock().await;
                                            queue_guard.push_back(text.to_string());
                                            info!("Added token to queue. Current queue size: {}", queue_guard.len());
                                            
                                            // IMPORTANT: Also add to the API queue that is checked by fetch_new_tokens
                                            let token_data = crate::api::TokenData {
                                                status: "success".to_string(),
                                                mint: mint_str.clone(),
                                                dev: new_token.creator_address.clone(),
                                                metadata: Some(format!("bonding_curve:{}", bonding_curve_str)),
                                                name: Some(new_token.token_name.clone()),
                                                symbol: Some(new_token.token_symbol.clone()),
                                                timestamp: Some(chrono::Utc::now().timestamp()),
                                            };
                                            
                                            let mut api_queue = crate::api::NEW_TOKEN_QUEUE.lock().unwrap();
                                            api_queue.push_back(token_data);
                                            info!("Added token to API queue. Current API queue size: {}", api_queue.len());
                                            
                                            // Calculate associated bonding curve
                                            if let (Ok(mint), Ok(bonding_curve)) = (
                                                Pubkey::from_str(&mint_str),
                                                Pubkey::from_str(&bonding_curve_str)
                                            ) {
                                                let associated_curve = find_associated_bonding_curve(&mint, &bonding_curve);
                                                info!("Associated Bonding Curve: {}", associated_curve);
                                            }
                                            
                                            // Process the token in chainstack module
                                            if let Some(token) = chainstack::process_notification(text) {
                                                info!("Token processed by chainstack: {} ({})",
                                                      token.token_name, token.mint_address);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Failed to decode base64 data: {}", e);
                                        debug!("Raw encoded data: {}", encoded_data);
                                        
                                        // Try with fallback to bs58 decoding (some chains use this)
                                        match bs58::decode(encoded_data).into_vec() {
                                            Ok(decoded_bs58) => {
                                                debug!("Decoded using bs58 instead ({} bytes)", decoded_bs58.len());
                                                if let Some(token_data) = parse_create_instruction(&decoded_bs58) {
                                                    info!("Detected new token (bs58 decoded):");
                                                    info!("  Name: {}", token_data.name);
                                                    // ... rest of token processing logic
                                                }
                                            }
                                            Err(_) => {
                                                warn!("Failed to decode with bs58 as well");
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
    
    Ok(())
}

// Test function for unit testing
#[cfg(test)]
pub fn test_parse_instruction(base64_data: &str) -> Option<TokenData> {
    match BASE64.decode(base64_data) {
        Ok(decoded) => parse_create_instruction(&decoded),
        Err(_) => None,
    }
} 