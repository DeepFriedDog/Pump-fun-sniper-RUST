use anyhow::{Result, anyhow};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use bs58;
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use serde_json::{json, Value};
use solana_program::pubkey::Pubkey;
use std::str::FromStr;
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;

use crate::config::{ATA_PROGRAM_ID, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID};

/// Finds the associated bonding curve for a given mint and bonding curve.
fn find_associated_bonding_curve(mint: &Pubkey, bonding_curve: &Pubkey) -> Pubkey {
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

    info!("Successfully parsed token data: {} ({})", name, symbol);
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

// Struct for parsed token data
#[derive(Debug, Clone)]
struct TokenData {
    name: String,
    symbol: String,
    uri: String,
    mint: String, 
    bonding_curve: String,
    user: String,
    tx_signature: String,
}

/// Run a simple WebSocket test to verify the connection and token detection
pub async fn run_websocket_test(ws_url: &str) -> Result<()> {
    // Parse the WebSocket URL
    let url = Url::parse(ws_url)?;
    
    info!("Starting WebSocket test, connecting to: {}", url);
    
    // Pump.fun program ID
    let pump_program_id = "DEvYCrHHMJXGxRXJCkD6Gq4RA8FPC2j97AM5VnWMn7XP";
    
    // Connect to the WebSocket server
    match connect_async(url).await {
        Ok((ws_stream, _)) => {
            // Ensure stream is properly pinned
            let (ws_write, ws_read) = ws_stream.split();
            
            // Subscribe to the pump.fun program logs using logsSubscribe
            let subscription_message = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "logsSubscribe",
                "params": [
                    {
                        "mentions": [pump_program_id]
                    },
                    {
                        "commitment": "finalized"
                    }
                ]
            });
            
            // Send the subscription request
            let subscription_text = subscription_message.to_string();
            info!("Sending subscription request: {}", subscription_text);
            
            let mut write_sink = Box::pin(ws_write);
            write_sink.send(Message::Text(subscription_text)).await?;
            
            // Set up ping timer
            let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
            
            // Set up status update timer (every minute)
            let mut status_interval = tokio::time::interval(Duration::from_secs(60));
            
            // Set a deadline for the test (15 minutes)
            let deadline = std::time::Instant::now() + Duration::from_secs(900); // 15 minutes
            
            // Track stats
            let mut total_messages_received = 0;
            let mut create_instructions_found = 0;
            
            // Process incoming messages
            let mut read_stream = ws_read;
            
            info!("WebSocket connection established, waiting for messages");
            info!("Test will run for 15 minutes. Press Ctrl+C to stop.");
            
            loop {
                tokio::select! {
                    // Check for timeout
                    _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                        info!("Test timeout reached after 15 minutes");
                        break;
                    }
                    
                    // Handle WebSocket messages
                    msg = read_stream.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                total_messages_received += 1;
                                
                                // Process the WebSocket message
                                match process_websocket_message(&text, pump_program_id).await {
                                    Some(token_data) => {
                                        create_instructions_found += 1;
                                        info!("💎 TOKEN CREATED: {} ({}) - Mint: {}", 
                                            token_data.name, token_data.symbol, token_data.mint);
                                        info!("  Creator: {}", token_data.user);
                                        info!("  Transaction: {}", token_data.tx_signature);
                                    }
                                    None => {
                                        // If we want to see all messages (debugging only)
                                        if std::env::var("DEBUG_ALL_MESSAGES").unwrap_or_else(|_| "false".to_string()) == "true" {
                                            if text.len() > 200 {
                                                info!("Received message: {}... (truncated)", &text[..200]);
                                            } else {
                                                info!("Received message: {}", text);
                                            }
                                        }
                                    }
                                }
                            }
                            Some(Ok(Message::Close(frame))) => {
                                info!("WebSocket connection closed: {:?}", frame);
                                break;
                            }
                            Some(Err(e)) => {
                                warn!("WebSocket error: {}", e);
                                break;
                            }
                            Some(_) => {} // Ignore other message types
                            None => {
                                info!("WebSocket stream ended");
                                break;
                            }
                        }
                    }
                    
                    // Send periodic pings to keep connection alive
                    _ = ping_interval.tick() => {
                        write_sink.send(Message::Ping(vec![])).await?;
                        debug!("Ping sent to keep connection alive");
                    }
                    
                    // Status update
                    _ = status_interval.tick() => {
                        let elapsed = deadline.elapsed();
                        let remaining = Duration::from_secs(900).checked_sub(elapsed).unwrap_or(Duration::from_secs(0));
                        let minutes = remaining.as_secs() / 60;
                        let seconds = remaining.as_secs() % 60;
                        
                        info!("Status: Received {} messages, found {} token creations. Test ends in {}m{}s", 
                            total_messages_received, create_instructions_found, minutes, seconds);
                    }
                }
            }
            
            info!("WebSocket test completed. Total messages: {}, Token creations found: {}", 
                  total_messages_received, create_instructions_found);
            Ok(())
        }
        Err(e) => {
            Err(anyhow!("Failed to connect to WebSocket: {}", e))
        }
    }
}

async fn process_websocket_message(text: &str, pump_program_id: &str) -> Option<TokenData> {
    // Parse the message as JSON
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
        // Check if this is a subscription response
        if json.get("id").is_some() && json.get("result").is_some() {
            info!("WebSocket subscription confirmed with id: {}", json["id"]);
            return None;
        }
        
        // Check if this is a log message
        if let Some(params) = json.get("params") {
            if let Some(result) = params.get("result") {
                if let Some(value) = result.get("value") {
                    // Extract transaction signature
                    let signature = value.get("signature").and_then(|s| s.as_str()).unwrap_or("unknown");
                    
                    // Extract logs
                    if let Some(logs) = value.get("logs").and_then(|l| l.as_array()) {
                        // First check if this contains a Create instruction
                        let has_create = logs.iter().any(|log| {
                            log.as_str().map_or(false, |s| s.contains("Program log: Instruction: Create"))
                        });
                        
                        if !has_create {
                            // Skip transactions that don't have Create instructions
                            return None;
                        }
                        
                        info!("Transaction signature: {}", signature);
                        
                        // Only check for program data if we have a Create instruction
                        for log in logs {
                            if let Some(log_str) = log.as_str() {
                                if log_str.contains("Program data:") {
                                    let parts: Vec<&str> = log_str.split("Program data: ").collect();
                                    if parts.len() > 1 {
                                        let encoded_data = parts[1].trim();
                                        
                                        // Try base64 decoding first
                                        if let Ok(decoded_data) = BASE64.decode(encoded_data) {
                                            let mut token_data = parse_create_instruction(&decoded_data)?;
                                            token_data.tx_signature = signature.to_string();
                                            return Some(token_data);
                                        } else {
                                            // Try bs58 as fallback
                                            if let Ok(decoded_data) = bs58::decode(encoded_data).into_vec() {
                                                let mut token_data = parse_create_instruction(&decoded_data)?;
                                                token_data.tx_signature = signature.to_string();
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
    
    None
} 