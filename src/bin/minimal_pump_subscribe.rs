use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use solana_program::pubkey::Pubkey;
use std::str::FromStr;
use std::time::Instant;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bs58;
use std::error::Error;
use log::{info, warn, error};

// Constants
const PUMP_PROGRAM_ID: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ATA_PROGRAM_ID: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

// Simple string representation of token data
#[derive(Debug)]
struct TokenData {
    name: String,
    symbol: String,
    uri: String,
    mint: String,
    bonding_curve: String,
    user: String,
    associated_bonding_curve: String,
}

// Finds the associated bonding curve for a given mint and bonding curve
fn find_associated_bonding_curve(mint: &Pubkey, bonding_curve: &Pubkey) -> Pubkey {
    // Create persistent Pubkeys to avoid temporary value issues
    let token_program = Pubkey::from_str(TOKEN_PROGRAM_ID).unwrap();
    let ata_program = Pubkey::from_str(ATA_PROGRAM_ID).unwrap();
    
    let seeds = &[
        bonding_curve.as_ref(),
        token_program.as_ref(),
        mint.as_ref(),
    ];
    
    let (derived_address, _) = Pubkey::find_program_address(
        seeds,
        &ata_program,
    );
    derived_address
}

// Parse create instruction data
fn parse_create_instruction(data: &[u8]) -> Option<TokenData> {
    if data.len() < 8 {
        return None;
    }
    
    let mut offset = 8; // Skip discriminator
    let mut parsed_data = TokenData {
        name: String::new(),
        symbol: String::new(),
        uri: String::new(),
        mint: String::new(),
        bonding_curve: String::new(),
        user: String::new(),
        associated_bonding_curve: String::new(),
    };
    
    // Parse string fields (name, symbol, uri)
    for field in &mut [&mut parsed_data.name, &mut parsed_data.symbol, &mut parsed_data.uri] {
        if offset + 4 > data.len() {
            return None;
        }
        
        // Read length (u32 little-endian)
        let length = u32::from_le_bytes([
            data[offset], data[offset + 1], data[offset + 2], data[offset + 3]
        ]) as usize;
        offset += 4;
        
        if offset + length > data.len() {
            return None;
        }
        
        // Read string data - assign directly to the String
        **field = match std::str::from_utf8(&data[offset..offset + length]) {
            Ok(s) => s.to_string(),
            Err(_) => return None,
        };
        offset += length;
    }
    
    // Parse public keys (mint, bonding_curve, user)
    for field in &mut [&mut parsed_data.mint, &mut parsed_data.bonding_curve, &mut parsed_data.user] {
        if offset + 32 > data.len() {
            return None;
        }
        
        **field = bs58::encode(&data[offset..offset + 32]).into_string();
        offset += 32;
    }
    
    // Calculate associated bonding curve
    let mint = Pubkey::from_str(&parsed_data.mint).ok()?;
    let bonding_curve = Pubkey::from_str(&parsed_data.bonding_curve).ok()?;
    parsed_data.associated_bonding_curve = find_associated_bonding_curve(&mint, &bonding_curve).to_string();
    
    Some(parsed_data)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Setup logging
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));
    
    info!("Starting minimal Pump.fun token subscription test");
    
    // You can replace this with your actual endpoint
    let ws_endpoint = "wss://solana-mainnet.core.chainstack.com/1b23a2c5f85ff33bdc25dbff1dd15437";
    
    let url = Url::parse(ws_endpoint)?;
    info!("Connecting to WebSocket endpoint: {}", url);
    
    // Connect to WebSocket - using the most direct approach
    let connection_start = Instant::now();
    let (mut ws_stream, _) = connect_async(url).await?;
    
    info!("Connected in {}ms", connection_start.elapsed().as_millis());
    
    // Subscribe to logs with processed commitment - most basic form
    let subscription_request = Message::Text(
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "logsSubscribe",
            "params": [
                {
                    "mentions": [PUMP_PROGRAM_ID]
                },
                {
                    "commitment": "processed"
                }
            ]
        }).to_string()
    );
    
    // Send subscription request
    let subscribe_time = Instant::now();
    ws_stream.send(subscription_request).await?;
    info!("Subscription request sent in {}ms", subscribe_time.elapsed().as_millis());
    
    // Process messages
    let _subscription_start = Instant::now();
    let mut tokens_detected = 0;
    
    info!("Listening for token creation events. Press Ctrl+C to stop.");
    
    while let Some(msg) = ws_stream.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                let data = match serde_json::from_str::<Value>(&text) {
                    Ok(data) => data,
                    Err(e) => {
                        warn!("Failed to parse message as JSON: {}", e);
                        continue;
                    }
                };
                
                // Log the message type for debugging
                if let Some(method) = data.get("method").and_then(|m| m.as_str()) {
                    if method == "logsNotification" {
                        // Extract logs from the notification
                        if let Some(logs) = data.get("params")
                            .and_then(|p| p.get("result"))
                            .and_then(|r| r.get("value"))
                            .and_then(|v| v.get("logs"))
                            .and_then(|l| l.as_array()) 
                        {
                            // Check if this is a token creation event
                            if logs.iter().any(|log| log.as_str().map_or(false, |s| s.contains("Program log: Instruction: Create"))) {
                                // Look for program data
                                let signature = data.get("params")
                                    .and_then(|p| p.get("result"))
                                    .and_then(|r| r.get("value"))
                                    .and_then(|v| v.get("signature"))
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("unknown");
                                
                                for log in logs {
                                    if let Some(log_str) = log.as_str() {
                                        if log_str.contains("Program data:") {
                                            let encoded_data = log_str.split(": ").nth(1).unwrap_or_default();
                                            
                                            // Decode program data
                                            match BASE64.decode(encoded_data) {
                                                Ok(decoded_data) => {
                                                    if let Some(token_data) = parse_create_instruction(&decoded_data) {
                                                        tokens_detected += 1;
                                                        info!("NEW TOKEN DETECTED (#{})", tokens_detected);
                                                        info!("Signature: {}", signature);
                                                        info!("Name: {}", token_data.name);
                                                        info!("Symbol: {}", token_data.symbol);
                                                        info!("URI: {}", token_data.uri);
                                                        info!("Mint: {}", token_data.mint);
                                                        info!("Bonding Curve: {}", token_data.bonding_curve);
                                                        info!("User: {}", token_data.user);
                                                        break; // Break to avoid duplicate processing
                                                    }
                                                },
                                                Err(e) => {
                                                    warn!("Failed to decode program data: {}", e);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if method == "subscription" {
                        // Ignore subscription notifications
                    } else {
                        info!("Received message type: {}", method);
                    }
                } else if let Some(id) = data.get("id") {
                    // This is likely a response to our subscription request
                    info!("Subscription confirmed with id: {}", id);
                } else {
                    // Any other message
                    info!("Received other message: {}", text);
                }
            },
            Ok(Message::Binary(_)) => {
                info!("Received binary message");
            },
            Ok(Message::Ping(_)) => {
                info!("Received ping");
            },
            Ok(Message::Pong(_)) => {
                info!("Received pong");
            },
            Ok(Message::Close(_)) => {
                info!("WebSocket closed");
                break;
            },
            Ok(Message::Frame(_)) => {
                // Handling the Frame variant to make Rust happy
                info!("Received frame message");
            },
            Err(e) => {
                error!("WebSocket error: {}", e);
                break;
            }
        }
    }
    
    info!("Test completed. Detected {} tokens.", tokens_detected);
    
    Ok(())
} 