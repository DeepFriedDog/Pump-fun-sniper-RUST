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
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use tokio::sync::mpsc;
use tokio::time::sleep;
use crate::chainstack_simple;
use lazy_static::lazy_static;

// Import from config
use crate::config::{ATA_PROGRAM_ID, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID};

/// Derives the bonding curve address for a given mint
pub fn get_bonding_curve_address(mint: &Pubkey) -> (Pubkey, u8) {
    let seeds = &[
        b"bonding-curve",
        mint.as_ref(),
    ];
    
    Pubkey::find_program_address(seeds, &PUMP_PROGRAM_ID)
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

/// Parses the create instruction data
pub fn parse_create_instruction(data: &[u8]) -> Option<DetectorTokenData> {
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
    Some(DetectorTokenData {
        name,
        symbol, 
        uri,
        mint,
        bonding_curve,
        user,
        tx_signature: String::new(),
    })
}

// Define our own TokenData rather than importing from crate::api
#[derive(Debug, Clone)]
pub struct DetectorTokenData {
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub mint: String,
    pub bonding_curve: String,
    pub user: String,
    pub tx_signature: String,
}

// Convert TokenData to the existing NewToken structure for compatibility
impl From<DetectorTokenData> for NewToken {
    fn from(token: DetectorTokenData) -> Self {
        NewToken {
            token_name: token.name,
            token_symbol: token.symbol,
            mint_address: token.mint,
            creator_address: token.user,
            transaction_signature: token.tx_signature,
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
                        if let Err(e) = process_message(&text, WEBSOCKET_MESSAGES.clone()).await {
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

async fn process_message(text: &str, queue: Arc<Mutex<std::collections::VecDeque<String>>>) -> Result<(), Box<dyn std::error::Error>> {
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
                                            // Save mint and bonding curve before token_data is moved or cloned
                                            let mint_str = token_data.mint.clone();
                                            let bonding_curve_str = token_data.bonding_curve.clone();
                                            let token_data_clone = token_data.clone();

                                            // Spawn a task to check liquidity asynchronously
                                            tokio::spawn(async move {
                                                // Get MIN_LIQUIDITY from environment variable
                                                let min_liquidity_str = std::env::var("MIN_LIQUIDITY").unwrap_or_else(|_| "4.0".to_string());
                                                let min_liquidity = min_liquidity_str.parse::<f64>().unwrap_or(4.0);
                                                
                                                // Check actual liquidity
                                                match check_token_liquidity(
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
                                                        debug!("Error checking liquidity: {}", e);
                                                        info!("🪙 NEW TOKEN CREATED! {} (mint: {}) 💰 0.00 SOL ❌", 
                                                            token_data_clone.name, 
                                                            token_data_clone.mint);
                                                    }
                                                }
                                            });
                                            
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
                                            if let Some(token) = chainstack_simple::process_notification(text) {
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
                                                    // Save mint and bonding curve before token_data is moved or cloned
                                                    let mint_str = token_data.mint.clone();
                                                    let bonding_curve_str = token_data.bonding_curve.clone();
                                                    let token_data_clone = token_data.clone();

                                                    // Spawn a task to check liquidity asynchronously
                                                    tokio::spawn(async move {
                                                        // Get MIN_LIQUIDITY from environment variable
                                                        let min_liquidity_str = std::env::var("MIN_LIQUIDITY").unwrap_or_else(|_| "4.0".to_string());
                                                        let min_liquidity = min_liquidity_str.parse::<f64>().unwrap_or(4.0);
                                                        
                                                        // Check actual liquidity
                                                        match check_token_liquidity(
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
                                                                debug!("Error checking liquidity: {}", e);
                                                                info!("🪙 NEW TOKEN CREATED! {} (mint: {}) 💰 0.00 SOL ❌", 
                                                                    token_data_clone.name, 
                                                                    token_data_clone.mint);
                                                            }
                                                        }
                                                    });
                                                    
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
                                                    if let Some(token) = chainstack_simple::process_notification(text) {
                                                        info!("Token processed by chainstack: {} ({})",
                                                              token.token_name, token.mint_address);
                                                    }
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
pub fn test_parse_instruction(base64_data: &str) -> Option<DetectorTokenData> {
    match BASE64.decode(base64_data) {
        Ok(decoded) => parse_create_instruction(&decoded),
        Err(_) => None,
    }
}

/// Check token liquidity by examining the balance of the associated bonding curve
pub async fn check_token_liquidity(
    mint: &str, 
    bonding_curve: &str,
    liquidity_threshold: f64,
) -> Result<(bool, f64), Box<dyn std::error::Error>> {
    use solana_client::rpc_client::RpcClient;
    use solana_sdk::pubkey::Pubkey;
    use std::str::FromStr;

    // Only use Chainstack endpoint for RPC calls
    let rpc_url = std::env::var("CHAINSTACK_ENDPOINT")
        .unwrap_or_else(|_| "https://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string());
    
    // Validate that the URL has a proper schema
    let rpc_url = if !rpc_url.starts_with("http") {
        format!("https://{}", rpc_url)
    } else {
        rpc_url
    };
    
    debug!("Using Chainstack RPC URL for liquidity check: {}", rpc_url);
    
    // Create a Solana RPC client with processed commitment level
    let client = RpcClient::new_with_commitment(
        rpc_url,
        solana_sdk::commitment_config::CommitmentConfig::processed()
    );
    
    // The rent exempt minimum amount in SOL
    const RENT_EXEMPT_MINIMUM: f64 = 0.00203928;
    
    // Convert mint address to public key
    let mint_pubkey = Pubkey::from_str(mint)?;
    
    // IMPROVEMENT: First try to directly get the primary bonding curve address
    let (primary_bonding_curve, _) = get_bonding_curve_address(&mint_pubkey);
    debug!("Checking primary bonding curve: {}", primary_bonding_curve);
    
    // Check the primary bonding curve first (it's the one that matters most)
    match client.get_account(&primary_bonding_curve) {
        Ok(account) => {
            // Get total balance
            let total_balance = account.lamports as f64 / 1_000_000_000.0; // Convert lamports to SOL
            
            // Subtract rent exempt minimum to get actual liquidity
            let actual_liquidity = (total_balance - RENT_EXEMPT_MINIMUM).max(0.0);
            
            debug!("Primary bonding curve has {} SOL (after subtracting {} SOL rent)",
                   actual_liquidity, RENT_EXEMPT_MINIMUM);
            
            return Ok((actual_liquidity >= liquidity_threshold, actual_liquidity));
        },
        Err(_) => {
            debug!("Primary bonding curve not found, checking alternative method...");
        }
    }

    // If we get here, we need to try the old method as a fallback
    
    // Convert bonding curve to public key
    let bonding_curve_pubkey = match Pubkey::from_str(bonding_curve) {
        Ok(pubkey) => pubkey,
        Err(e) => {
            debug!("Error parsing bonding curve pubkey: {}", e);
            // Return 0.0 SOL if we can't parse the bonding curve
            return Ok((false, 0.0));
        }
    };
    
    // Calculate the associated bonding curve address
    let associated_bonding_curve = find_associated_bonding_curve(&mint_pubkey, &bonding_curve_pubkey);
    
    // Get account info to check liquidity
    debug!("Checking associated account {} for liquidity (threshold: {} SOL)", associated_bonding_curve, liquidity_threshold);
    match client.get_account(&associated_bonding_curve) {
        Ok(account) => {
            let balance = account.lamports as f64 / 1_000_000_000.0; // Convert lamports to SOL
            debug!("Found account with {} SOL ({} lamports)", balance, account.lamports);
            Ok((balance >= liquidity_threshold, balance))
        },
        Err(e) => {
            debug!("Error retrieving associated account {}: {}", associated_bonding_curve, e);
            
            // We already tried the primary bonding curve above, so this is a real error
            debug!("No valid bonding curve account found");
            
            // Return the original error
            Err(format!("Failed to get account info for associated bonding curve: {}", e).into())
        }
    }
}

/// Check only the primary bonding curve account liquidity and subtract rent exemption
/// This is faster than the original function as it only checks one account
pub async fn check_token_primary_liquidity(
    mint: &str,
    liquidity_threshold: f64,
) -> Result<(bool, f64), Box<dyn std::error::Error>> {
    use solana_client::rpc_client::RpcClient;
    use solana_sdk::pubkey::Pubkey;
    use std::str::FromStr;

    // The rent exempt minimum amount in SOL
    const RENT_EXEMPT_MINIMUM: f64 = 0.00203928;

    // Only use Chainstack endpoint for RPC calls
    let rpc_url = std::env::var("CHAINSTACK_ENDPOINT")
        .unwrap_or_else(|_| "https://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string());
    
    // Validate that the URL has a proper schema
    let rpc_url = if !rpc_url.starts_with("http") {
        format!("https://{}", rpc_url)
    } else {
        rpc_url
    };
    
    debug!("Using Chainstack RPC URL for liquidity check: {}", rpc_url);
    
    // Create a Solana RPC client with processed commitment level
    let client = RpcClient::new_with_commitment(
        rpc_url,
        solana_sdk::commitment_config::CommitmentConfig::processed()
    );
    
    // Convert mint address to public key
    let mint_pubkey = Pubkey::from_str(mint)?;
    
    // Derive the bonding curve address from the mint
    let (bonding_curve_pubkey, _) = get_bonding_curve_address(&mint_pubkey);
    
    debug!("Checking liquidity for mint: {}", mint);
    debug!("Derived bonding curve: {}", bonding_curve_pubkey);
    
    // Get primary bonding curve account info to check liquidity
    match client.get_account(&bonding_curve_pubkey) {
        Ok(account) => {
            // Get total balance
            let total_balance = account.lamports as f64 / 1_000_000_000.0; // Convert lamports to SOL
            
            // Subtract rent exempt minimum to get actual liquidity
            let actual_liquidity = (total_balance - RENT_EXEMPT_MINIMUM).max(0.0);
            
            debug!("Found liquidity: {} SOL (after rent exemption)", actual_liquidity);
            
            // Check if actual liquidity meets threshold
            Ok((actual_liquidity >= liquidity_threshold, actual_liquidity))
        },
        Err(e) => {
            warn!("Failed to get bonding curve account info: {}", e);
            Err(format!("Failed to get account info: {}", e).into())
        }
    }
}

// Define our own NewToken structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewToken {
    pub token_name: String,
    pub token_symbol: String,
    pub mint_address: String,
    pub creator_address: String,
    pub transaction_signature: String,
    pub timestamp: i64,
}

// Define a constant for WebSocket messages
lazy_static! {
    pub static ref WEBSOCKET_MESSAGES: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
} 