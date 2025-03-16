use crate::chainstack_simple;
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use bs58;
use futures_util::{SinkExt, StreamExt};
use lazy_static::lazy_static;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solana_program::pubkey::Pubkey;
use std::collections::{HashMap, VecDeque};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;
use tokio::time::timeout;

// Import from config
use crate::config::{ATA_PROGRAM_ID, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID};

/// Derives the bonding curve address for a given mint
pub fn get_bonding_curve_address(mint: &Pubkey) -> (Pubkey, u8) {
    let seeds = &[b"bonding-curve", mint.as_ref()];

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
        debug!(
            "Data too short to be a valid instruction: {} bytes",
            data.len()
        );
        return None;
    }

    let mut offset = 8; // Skip discriminator

    let read_string = |data: &[u8], offset: &mut usize| -> Option<String> {
        if *offset + 4 > data.len() {
            debug!(
                "Offset out of bounds when reading string length: offset={}, len={}",
                *offset,
                data.len()
            );
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
            debug!(
                "String content would exceed data bounds: offset={}, length={}, data_len={}",
                *offset,
                length,
                data.len()
            );
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
            debug!(
                "Offset out of bounds when reading pubkey: offset={}, len={}",
                *offset,
                data.len()
            );
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

    // Exponential backoff settings
    let mut retry_attempts = 0;
    let max_retries = 10; // Allow more retries before giving up
    let initial_backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);
    let mut current_backoff = initial_backoff;
    let backoff_factor = 1.5;

    loop {
        // Check if we should stop listening due to a position being taken
        if std::env::var("_STOP_WEBSOCKET_LISTENER").map(|v| v == "true").unwrap_or(false) {
            info!("🛑 Detected _STOP_WEBSOCKET_LISTENER flag. Stopping token detection until position is closed.");
            // Wait for the flag to be cleared
            while std::env::var("_STOP_WEBSOCKET_LISTENER").map(|v| v == "true").unwrap_or(false) {
                tokio::time::sleep(Duration::from_secs(5)).await;
                info!("Waiting for current position to close before resuming token detection...");
            }
            info!("Token detection lock released. Resuming detection.");
        }

        match connect_to_websocket(&wss_endpoint).await {
            Ok(_) => {
                // Connection closed normally, reset backoff
                info!("WebSocket connection closed normally, reconnecting...");
                retry_attempts = 0;
                current_backoff = initial_backoff;
            }
            Err(e) => {
                retry_attempts += 1;
                error!("WebSocket connection error (attempt {}/{}): {}", 
                       retry_attempts, max_retries, e);
                
                if retry_attempts >= max_retries {
                    error!("Maximum retry attempts reached. Waiting longer before trying again.");
                    // Reset retry counter but use max backoff
                    retry_attempts = 0;
                    tokio::time::sleep(max_backoff).await;
                    continue;
                }

                warn!("Reconnecting in {} seconds...", current_backoff.as_secs());
                
                // Update backoff for next attempt with exponential increase
                let next_backoff = current_backoff.as_secs_f64() * backoff_factor;
                current_backoff = std::cmp::min(
                    Duration::from_secs_f64(next_backoff),
                    max_backoff
                );
            }
        }

        tokio::time::sleep(current_backoff).await;
    }
}

async fn connect_to_websocket(wss_endpoint: &str) -> Result<()> {
    // Connection state tracking
    let connection_start = Instant::now();
    
    // Connect to the WebSocket server
    let url = Url::parse(wss_endpoint)?;
    info!("Connecting to {}", url);

    // Add connection timeout
    let connect_future = connect_async(url.clone());
    let connection_timeout = Duration::from_secs(15);
    
    let ws_stream = match tokio::time::timeout(connection_timeout, connect_future).await {
        Ok(result) => match result {
            Ok((stream, _)) => {
                let connect_time = connection_start.elapsed();
                info!("WebSocket connection established in {:.2?}", connect_time);
                stream
            },
            Err(e) => {
                error!("Failed to connect to WebSocket at {}: {}", url, e);
                return Err(anyhow!("WebSocket connection error: {}", e));
            }
        },
        Err(_) => {
            error!("WebSocket connection timed out after {:?}", connection_timeout);
            return Err(anyhow!("WebSocket connection timed out"));
        }
    };

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

    // Add timeout for subscription request
    let subscription_timeout = Duration::from_secs(10);
    match tokio::time::timeout(
        subscription_timeout,
        write.send(Message::Text(subscription_message.to_string()))
    ).await {
        Ok(result) => match result {
            Ok(_) => {
                info!("Listening for new token creations from program: {}", program_id);
            },
            Err(e) => {
                error!("Failed to send subscription request: {}", e);
                return Err(anyhow!("Failed to send subscription request: {}", e));
            }
        },
        Err(_) => {
            error!("Subscription request timed out after {:?}", subscription_timeout);
            return Err(anyhow!("Subscription request timed out"));
        }
    }

    // Process subscription confirmation with timeout
    let confirmation_timeout = Duration::from_secs(10);
    let confirmation_result = tokio::time::timeout(
        confirmation_timeout,
        read.next()
    ).await;
    
    match confirmation_result {
        Ok(Some(Ok(message))) => {
            match message {
                Message::Text(text) => {
                    info!("Subscription response: {}", text);

                    // Parse subscription response to verify it was successful
                    match serde_json::from_str::<serde_json::Value>(&text) {
                        Ok(json) => {
                            if let Some(result) = json.get("result") {
                                info!("✅ Subscription successfully confirmed with ID: {:?}", result);
                            } else if let Some(error) = json.get("error") {
                                error!("⛔ Subscription failed: {:?}", error);
                                return Err(anyhow!("Subscription failed: {:?}", error));
                            } else {
                                warn!("⚠️ Unusual subscription response format: {}", text);
                            }
                        }
                        Err(e) => {
                            warn!("Could not parse subscription response as JSON: {}", e);
                            return Err(anyhow!("Invalid subscription response: {}", e));
                        }
                    }
                }
                _ => {
                    warn!("Unexpected message format for subscription confirmation");
                    return Err(anyhow!("Unexpected message format for subscription confirmation"));
                }
            }
        }
        Ok(Some(Err(e))) => {
            error!("Error receiving subscription confirmation: {}", e);
            return Err(anyhow!("Error receiving subscription confirmation: {}", e));
        }
        Ok(None) => {
            error!("WebSocket closed before receiving subscription confirmation");
            return Err(anyhow!("WebSocket closed before receiving subscription confirmation"));
        }
        Err(_) => {
            error!("Timed out waiting for subscription confirmation");
            return Err(anyhow!("Timed out waiting for subscription confirmation"));
        }
    }

    info!("✅ WebSocket setup complete, monitoring for new tokens...");

    // Set up ping timer to keep connection alive
    let mut ping_interval = tokio::time::interval(Duration::from_secs(5)); // Ping more frequently (5s instead of 10s)
    let mut last_ping_time = Instant::now();
    let mut last_pong_time = Instant::now();
    let pong_timeout = Duration::from_secs(10); // Lower timeout for pongs
    let mut subscribed_bonding_curves = HashMap::new();

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

                // Also send a raw ping frame, which should always receive a pong frame response
                if let Err(e) = write.send(Message::Ping(vec![1, 2, 3, 4])).await {
                    error!("Failed to send ping frame: {}", e);
                }

                debug!("Ping sent to WebSocket server");
                last_ping_time = Instant::now();

                // Check if we've received pongs recently
                let pong_delay = last_ping_time.duration_since(last_pong_time);
                if pong_delay > pong_timeout {
                    warn!("No pong response received in {} seconds, reconnecting...", pong_delay.as_secs());
                    // Force reconnection by returning from this function
                    return Ok(());
                }
            }

            // Process next message
            next_message = read.next() => {
                match next_message {
                    Some(Ok(Message::Text(text))) => {
                        debug!("Received WebSocket text message");
                        // Process the message
                        if let Err(e) = process_message(&text, WEBSOCKET_MESSAGES.clone(), &mut write, &mut subscribed_bonding_curves).await {
                            warn!("Error processing message: {}", e);
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        // Automatically respond to pings
                        if let Err(e) = write.send(Message::Pong(data)).await {
                            error!("Failed to send pong: {}", e);
                            return Err(anyhow!("Pong failed"));
                        }
                        debug!("Responded to server ping with pong");
                        last_pong_time = Instant::now();
                    }
                    Some(Ok(Message::Pong(_))) => {
                        debug!("Received pong from server");
                        last_pong_time = Instant::now();
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

async fn process_message(
    text: &str,
    queue: Arc<Mutex<VecDeque<String>>>,
    write: &mut futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, tokio_tungstenite::tungstenite::protocol::Message>,
    subscribed_bonding_curves: &mut HashMap<String, Instant>
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Parse the message as JSON
    let data: Value = serde_json::from_str(text)?;

    // Check if this is a logs notification
    if let Some("logsNotification") = data.get("method").and_then(Value::as_str) {
        if let Some(log_data) = data
            .get("params")
            .and_then(|p| p.get("result"))
            .and_then(|r| r.get("value"))
        {
            let signature = log_data
                .get("signature")
                .and_then(Value::as_str)
                .unwrap_or("Unknown");

            // Extract logs
            if let Some(logs) = log_data.get("logs").and_then(Value::as_array) {
                // Check if this is a create instruction
                let has_create = logs.iter().any(|log| {
                    log.as_str()
                        .map_or(false, |s| s.contains("Program log: Instruction: Create"))
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
                                        debug!(
                                            "Successfully decoded Program data ({} bytes)",
                                            decoded_data.len()
                                        );

                                        // Parse the instruction
                                        if let Some(token_data) = parse_create_instruction(&decoded_data)
                                        {
                                            // Filter out invalid tokens
                                            if !token_data.mint.ends_with("pump")
                                                && token_data.name == "Unknown"
                                            {
                                                debug!(
                                                    "Filtered out invalid token with mint: {}",
                                                    token_data.mint
                                                );
                                                continue;
                                            }

                                            // Save mint and bonding curve before token_data is moved or cloned
                                            let mint_str = token_data.mint.clone();
                                            let bonding_curve_str =
                                                token_data.bonding_curve.clone();
                                            let token_data_clone = token_data.clone();

                                            // IMMEDIATELY log the token detection with pending liquidity status
                                            let is_quiet_mode = std::env::args().any(|arg| arg == "--quiet");
                                            if !is_quiet_mode {
                                                info!("🪙 NEW TOKEN DETECTED! {} (mint: {}) 💰 Checking liquidity...", token_data_clone.name, token_data_clone.mint);
                                            }

                                            // IMMEDIATELY check liquidity with RPC to display the format the user wants
                                            if let (Ok(mint), Ok(bonding_curve)) = (
                                                Pubkey::from_str(&mint_str),
                                                Pubkey::from_str(&bonding_curve_str),
                                            ) {
                                                // Get min liquidity threshold
                                                let min_liquidity = std::env::var("MIN_LIQUIDITY")
                                                    .unwrap_or_else(|_| "5.0".to_string())
                                                    .parse::<f64>()
                                                    .unwrap_or(5.0);
                                                
                                                // Check liquidity via direct RPC call for immediate feedback
                                                match solana_client::rpc_client::RpcClient::new_with_timeout_and_commitment(
                                                    std::env::var("CHAINSTACK_ENDPOINT").unwrap_or_else(|_| {
                                                        "https://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string()
                                                    }),
                                                    std::time::Duration::from_millis(500),
                                                    solana_sdk::commitment_config::CommitmentConfig::processed(),
                                                ).get_account(&bonding_curve) {
                                                    Ok(account) => {
                                                        // Calculate actual liquidity after subtracting rent
                                                        const RENT_EXEMPT_MINIMUM: f64 = 0.00203928;
                                                        let total_balance = account.lamports as f64 / 1_000_000_000.0;
                                                        let actual_liquidity = (total_balance - RENT_EXEMPT_MINIMUM).max(0.0);
                                                        
                                                        // Update cache
                                                        let cache_key = format!("bonding_curve:{}:{}", bonding_curve_str, mint_str);
                                                        let primary_key = format!("primary:{}", mint_str);
                                                        {
                                                            let mut cache = LIQUIDITY_CACHE.lock().await;
                                                            cache.insert(cache_key, (actual_liquidity, Instant::now()));
                                                            cache.insert(primary_key, (actual_liquidity, Instant::now()));
                                                        }
                                                        
                                                        // Display in the requested format
                                                        let has_liquidity = actual_liquidity >= min_liquidity;
                                                        let check_mark = if has_liquidity { "✅" } else { "❌" };
                                                        
                                                        // Log in the exact format requested
                                                        info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) {:.2} SOL {}", 
                                                              token_data_clone.name, mint_str, actual_liquidity, check_mark);
                                                        
                                                        // Add the follow-up message about liquidity check
                                                        if has_liquidity {
                                                            info!("✅ Liquidity check PASSED - Proceeding with buy");
                                                        } else {
                                                            info!("❌ Liquidity check FAILED ({:.2} SOL < {:.2} SOL) - Skipping buy", 
                                                                  actual_liquidity, min_liquidity);
                                                        }

                                                        // Also subscribe to the bonding curve for real-time updates
                                                        let associated_curve = find_associated_bonding_curve(&mint, &bonding_curve);
                                                        info!("Associated Bonding Curve: {}", associated_curve);
                                                        
                                                        // Subscribe to the bonding curve to get real-time liquidity updates
                                                        subscribe_to_bonding_curve(write, &mint_str, &bonding_curve_str, subscribed_bonding_curves).await?;
                                                        
                                                        // Convert to NewToken and add to the queue IMMEDIATELY
                                                        let mut new_token: NewToken = token_data.into();
                                                        new_token.transaction_signature = signature.to_string();

                                                        // Add message to global queue for processing IMMEDIATELY
                                                        let mut queue_guard = queue.lock().await;
                                                        queue_guard.push_back(text.to_string());
                                                        info!(
                                                            "Added token to queue. Current queue size: {}",
                                                            queue_guard.len()
                                                        );

                                                        // IMPORTANT: Also add to the API queue that is checked by fetch_new_tokens
                                                        let token_data = crate::api::TokenData {
                                                            status: "success".to_string(),
                                                            mint: mint_str.clone(),
                                                            dev: new_token.creator_address.clone(),
                                                            metadata: Some(format!(
                                                                "bonding_curve:{},tx:{}",
                                                                bonding_curve_str,
                                                                signature
                                                            )),
                                                            name: Some(new_token.token_name.clone()),
                                                            symbol: Some(new_token.token_symbol.clone()),
                                                            timestamp: Some(chrono::Utc::now().timestamp()),
                                                            liquidity_status: Some(has_liquidity),   // Already have the ACTUAL liquidity value
                                                            liquidity_amount: Some(actual_liquidity), // Already have the ACTUAL liquidity value
                                                        };

                                                        let mut api_queue =
                                                            crate::api::NEW_TOKEN_QUEUE.lock().unwrap();
                                                        api_queue.push_back(token_data);
                                                        info!("Added token to API queue. Current API queue size: {}", api_queue.len());
                                                    },
                                                    Err(_) => {
                                                        // If RPC call fails, still log but with zero liquidity
                                                        info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) 0.00 SOL ❌", 
                                                              token_data_clone.name, mint_str);
                                                        info!("❌ Liquidity check FAILED (0.00 SOL < {:.2} SOL) - Skipping buy", 
                                                              min_liquidity);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        debug!("Failed to decode base64 data: {}", e);
                                        debug!("Raw encoded data: {}", encoded_data);

                                        // Try with fallback to bs58 decoding (some chains use this)
                                        match bs58::decode(encoded_data).into_vec() {
                                            Ok(decoded_bs58) => {
                                                debug!(
                                                    "Decoded using bs58 instead ({} bytes)",
                                                    decoded_bs58.len()
                                                );
                                                if let Some(token_data) =
                                                    parse_create_instruction(&decoded_bs58)
                                                {
                                                    // Filter out invalid tokens
                                                    if !token_data.mint.ends_with("pump")
                                                        && token_data.name == "Unknown"
                                                    {
                                                        debug!("Filtered out invalid token with mint: {}", token_data.mint);
                                                        continue;
                                                    }

                                                    // Save mint and bonding curve before token_data is moved or cloned
                                                    let mint_str = token_data.mint.clone();
                                                    let bonding_curve_str =
                                                        token_data.bonding_curve.clone();
                                                    let token_data_clone = token_data.clone();

                                                    // IMMEDIATELY log the token detection with pending liquidity status
                                                    let is_quiet_mode = std::env::args().any(|arg| arg == "--quiet");
                                                    if !is_quiet_mode {
                                                        info!("🪙 NEW TOKEN DETECTED! {} (mint: {}) 💰 Checking liquidity...", token_data_clone.name, token_data_clone.mint);
                                                    }

                                                    // IMMEDIATELY check liquidity with RPC to display the format the user wants
                                                    if let (Ok(mint), Ok(bonding_curve)) = (
                                                        Pubkey::from_str(&mint_str),
                                                        Pubkey::from_str(&bonding_curve_str),
                                                    ) {
                                                        // Get min liquidity threshold
                                                        let min_liquidity = std::env::var("MIN_LIQUIDITY")
                                                            .unwrap_or_else(|_| "5.0".to_string())
                                                            .parse::<f64>()
                                                            .unwrap_or(5.0);
                                                        
                                                        // Check liquidity via direct RPC call for immediate feedback
                                                        match solana_client::rpc_client::RpcClient::new_with_timeout_and_commitment(
                                                            std::env::var("CHAINSTACK_ENDPOINT").unwrap_or_else(|_| {
                                                                "https://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string()
                                                            }),
                                                            std::time::Duration::from_millis(500),
                                                            solana_sdk::commitment_config::CommitmentConfig::processed(),
                                                        ).get_account(&bonding_curve) {
                                                            Ok(account) => {
                                                                // Calculate actual liquidity after subtracting rent
                                                                const RENT_EXEMPT_MINIMUM: f64 = 0.00203928;
                                                                let total_balance = account.lamports as f64 / 1_000_000_000.0;
                                                                let actual_liquidity = (total_balance - RENT_EXEMPT_MINIMUM).max(0.0);
                                                                
                                                                // Update cache
                                                                let cache_key = format!("bonding_curve:{}:{}", bonding_curve_str, mint_str);
                                                                let primary_key = format!("primary:{}", mint_str);
                                                                {
                                                                    let mut cache = LIQUIDITY_CACHE.lock().await;
                                                                    cache.insert(cache_key, (actual_liquidity, Instant::now()));
                                                                    cache.insert(primary_key, (actual_liquidity, Instant::now()));
                                                                }
                                                                
                                                                // Display in the requested format
                                                                let has_liquidity = actual_liquidity >= min_liquidity;
                                                                let check_mark = if has_liquidity { "✅" } else { "❌" };
                                                                
                                                                // Log in the exact format requested
                                                                info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) {:.2} SOL {}", 
                                                                      token_data_clone.name, mint_str, actual_liquidity, check_mark);
                                                                
                                                                // Add the follow-up message about liquidity check
                                                                if has_liquidity {
                                                                    info!("✅ Liquidity check PASSED - Proceeding with buy");
                                                                } else {
                                                                    info!("❌ Liquidity check FAILED ({:.2} SOL < {:.2} SOL) - Skipping buy", 
                                                                          actual_liquidity, min_liquidity);
                                                                }

                                                                // Also subscribe to the bonding curve for real-time updates
                                                                let associated_curve = find_associated_bonding_curve(&mint, &bonding_curve);
                                                                info!("Associated Bonding Curve: {}", associated_curve);
                                                                
                                                                // Subscribe to the bonding curve to get real-time liquidity updates
                                                                subscribe_to_bonding_curve(write, &mint_str, &bonding_curve_str, subscribed_bonding_curves).await?;
                                                                
                                                                // Convert to NewToken and add to the queue IMMEDIATELY
                                                                let mut new_token: NewToken = token_data.into();
                                                                new_token.transaction_signature = signature.to_string();

                                                                // Add message to global queue for processing IMMEDIATELY
                                                                let mut queue_guard = queue.lock().await;
                                                                queue_guard.push_back(text.to_string());
                                                                info!(
                                                                    "Added token to queue. Current queue size: {}",
                                                                    queue_guard.len()
                                                                );

                                                                // IMPORTANT: Also add to the API queue that is checked by fetch_new_tokens
                                                                let token_data = crate::api::TokenData {
                                                                    status: "success".to_string(),
                                                                    mint: mint_str.clone(),
                                                                    dev: new_token.creator_address.clone(),
                                                                    metadata: Some(format!(
                                                                        "bonding_curve:{},tx:{}",
                                                                        bonding_curve_str,
                                                                        signature
                                                                    )),
                                                                    name: Some(new_token.token_name.clone()),
                                                                    symbol: Some(new_token.token_symbol.clone()),
                                                                    timestamp: Some(chrono::Utc::now().timestamp()),
                                                                    liquidity_status: Some(has_liquidity),   // Already have the ACTUAL liquidity value
                                                                    liquidity_amount: Some(actual_liquidity), // Already have the ACTUAL liquidity value
                                                                };

                                                                let mut api_queue =
                                                                    crate::api::NEW_TOKEN_QUEUE.lock().unwrap();
                                                                api_queue.push_back(token_data);
                                                                info!("Added token to API queue. Current API queue size: {}", api_queue.len());
                                                            },
                                                            Err(_) => {
                                                                // If RPC call fails, still log but with zero liquidity
                                                                info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) 0.00 SOL ❌", 
                                                                      token_data_clone.name, mint_str);
                                                                info!("❌ Liquidity check FAILED (0.00 SOL < {:.2} SOL) - Skipping buy", 
                                                                      min_liquidity);
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            Err(_) => {
                                                debug!("Failed to decode with bs58 as well");
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
    // Check if this is an account notification (for liquidity updates)
    else if let Some("accountNotification") = data.get("method").and_then(Value::as_str) {
        if let Some(account_info) = data
            .get("params")
            .and_then(|p| p.get("result"))
            .and_then(|r| r.get("value"))
        {
            let is_quiet_mode = std::env::args().any(|arg| arg == "--quiet");
            
            // Extract account data and lamports (SOL balance)
            let lamports = account_info.get("lamports").and_then(Value::as_u64).unwrap_or(0);
            let sol_balance = lamports as f64 / 1_000_000_000.0; // Convert lamports to SOL
            
            // The rent exempt minimum amount in SOL
            const RENT_EXEMPT_MINIMUM: f64 = 0.00203928;
            let actual_liquidity = (sol_balance - RENT_EXEMPT_MINIMUM).max(0.0);
            
            // Get the bonding curve address which is included in the subscription ID
            if let Some(subscription) = data
                .get("params")
                .and_then(|p| p.get("subscription"))
                .and_then(Value::as_str)
            {
                // The subscription ID format is "bonding_curve_{address}"
                if subscription.starts_with("bonding_curve_") {
                    let parts: Vec<&str> = subscription.split('_').collect();
                    if parts.len() >= 3 {
                        let bonding_curve = parts[2];
                        
                        // Find the token info from our tracker
                        let min_liquidity = std::env::var("MIN_LIQUIDITY")
                                .unwrap_or_else(|_| "5.0".to_string())
                                .parse::<f64>()
                                .unwrap_or(5.0);
                        
                        let has_liquidity = actual_liquidity >= min_liquidity;
                        let check_mark = if has_liquidity { "✅" } else { "❌" };
                        
                        // First collect mint address and token name
                        let mut mint_to_update = None;
                        let mut token_name = String::from("Unknown");
                        
                        // Look up matching mint in the cache
                        {
                            let cache = LIQUIDITY_CACHE.lock().await;
                            // Find associated mint by looking at the bonding curve
                            for (cache_key, _) in cache.iter() {
                                if cache_key.contains(bonding_curve) {
                                    if let Some(mint) = cache_key.split(':').last() {
                                        mint_to_update = Some(mint.to_string());
                                        // Get token name if possible
                                        if let Some(name) = get_token_name_for_mint(mint) {
                                            token_name = name;
                                        }
                                        break;
                                    }
                                }
                            }
                        }
                        
                        // Now update the cache if we found a mint
                        if let Some(mint) = mint_to_update {
                            // Log the updated liquidity in the exact format requested
                            info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) {:.2} SOL {}", 
                                  token_name, mint, actual_liquidity, check_mark);
                            
                            // Add the follow-up message about liquidity check
                            if has_liquidity {
                                info!("✅ Liquidity check PASSED - Proceeding with buy");
                            } else {
                                info!("❌ Liquidity check FAILED ({:.2} SOL < {:.2} SOL) - Skipping buy", 
                                    actual_liquidity, min_liquidity);
                            }
                            
                            // Update the cache with the new amount in a separate lock scope
                            let cache_key = format!("bonding_curve:{}:{}", bonding_curve, mint);
                            let mut cache = LIQUIDITY_CACHE.lock().await;
                            cache.insert(cache_key, (actual_liquidity, Instant::now()));
                            
                            // Also update primary liquidity cache to make it available for other functions
                            let primary_cache_key = format!("primary:{}", mint);
                            cache.insert(primary_cache_key, (actual_liquidity, Instant::now()));
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

// Helper function to get token name from mint address
fn get_token_name_for_mint(mint: &str) -> Option<String> {
    // Try to find the token name from recently detected tokens
    let api_queue = crate::api::NEW_TOKEN_QUEUE.lock().unwrap();
    for token_data in api_queue.iter() {
        if token_data.mint == mint {
            return token_data.name.clone();
        }
    }
    None
}

// Subscribe to bonding curve account changes to track liquidity in real-time
async fn subscribe_to_bonding_curve(
    write: &mut futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, tokio_tungstenite::tungstenite::protocol::Message>,
    mint: &str, 
    bonding_curve: &str,
    subscribed_curves: &mut HashMap<String, Instant>
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Check if we already subscribed to this bonding curve
    if subscribed_curves.contains_key(bonding_curve) {
        debug!("Already subscribed to bonding curve: {}", bonding_curve);
        return Ok(());
    }
    
    // Create a subscription message for the bonding curve account
    let subscription_id = format!("bonding_curve_{}", bonding_curve);
    let subscription_message = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "accountSubscribe",
        "params": [
            bonding_curve,
            {
                "encoding": "jsonParsed",
                "commitment": "processed"
            }
        ]
    });
    
    // Send the subscription request
    if let Err(e) = write.send(Message::Text(subscription_message.to_string())).await {
        warn!("Failed to subscribe to bonding curve {}: {}", bonding_curve, e);
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to subscribe to bonding curve: {}", e),
        )));
    }
    
    debug!("Subscribed to bonding curve {} for mint {}", bonding_curve, mint);
    
    // Track this subscription
    subscribed_curves.insert(bonding_curve.to_string(), Instant::now());
    
    // Also update the liquidity cache with an initial entry
    {
        let cache_key = format!("bonding_curve:{}:{}", bonding_curve, mint);
        let mut cache = LIQUIDITY_CACHE.lock().await;
        cache.insert(cache_key, (0.0, Instant::now()));
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

// Add a cache for liquidity checks to reduce redundant RPC calls
lazy_static! {
    static ref LIQUIDITY_CACHE: Arc<Mutex<HashMap<String, (f64, std::time::Instant)>>> =
        Arc::new(Mutex::new(HashMap::new()));
}

// Create or get the RPC client
async fn get_rpc_client() -> solana_client::rpc_client::RpcClient {
    use solana_client::rpc_client::RpcClient;

    // Only use Chainstack endpoint for RPC calls
    let rpc_url = std::env::var("CHAINSTACK_ENDPOINT").unwrap_or_else(|_| {
        "https://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string()
    });

    // Validate that the URL has a proper schema
    let rpc_url = if !rpc_url.starts_with("http") {
        format!("https://{}", rpc_url)
    } else {
        rpc_url
    };

    debug!("Creating RPC client with URL: {}", rpc_url);

    // Create a Solana RPC client with processed commitment level and VERY short timeout
    RpcClient::new_with_timeout_and_commitment(
        rpc_url,
        std::time::Duration::from_millis(500), // 500ms timeout for faster responsiveness
        solana_sdk::commitment_config::CommitmentConfig::processed(),
    )
}

/// New liquidity check using RPC (from commit c0b1a813cf5e3e22aebe2b233b6a2a4240ece528)
pub async fn check_token_liquidity(mint: &str, bonding_curve: &str, liquidity_threshold: f64) -> Result<(bool, f64)> {
    use solana_sdk::pubkey::Pubkey;
    use std::time::Duration;
    
    // First, check for cached primary values which are most reliable
    // These are set via the WebSocket notification handlers
    let primary_cache_key = format!("primary:{}", mint);
    {
        let cache = LIQUIDITY_CACHE.lock().await;
        if let Some((balance, timestamp)) = cache.get(&primary_cache_key) {
            if timestamp.elapsed() < Duration::from_secs(10) {
                debug!("Using cached primary liquidity for {}: {} SOL", mint, balance);
                return Ok((*balance >= liquidity_threshold, *balance));
            }
        }
    }
    
    // Next, check for bonding curve cached values
    let curve_cache_key = format!("bonding_curve:{}:{}", bonding_curve, mint);
    {
        let cache = LIQUIDITY_CACHE.lock().await;
        if let Some((balance, timestamp)) = cache.get(&curve_cache_key) {
            if timestamp.elapsed() < Duration::from_secs(10) {
                debug!("Using cached bonding curve liquidity for {}: {} SOL", mint, balance);
                return Ok((*balance >= liquidity_threshold, *balance));
            }
        }
    }
    
    // If we don't have cached values, do an RPC call as a fallback
    let client = get_rpc_client().await;
    let mint_pubkey = Pubkey::from_str(mint).map_err(|e| anyhow!("Invalid mint pubkey: {}", e))?;
    let (primary_bonding_curve, _) = get_bonding_curve_address(&mint_pubkey);
    
    // The rent exempt minimum amount in SOL
    const RENT_EXEMPT_MINIMUM: f64 = 0.00203928;
    match client.get_account(&primary_bonding_curve) {
        Ok(account) => {
            let total_balance = account.lamports as f64 / 1_000_000_000.0;
            let actual_liquidity = (total_balance - RENT_EXEMPT_MINIMUM).max(0.0);
            
            // Update both cache entries
            {
                let mut cache = LIQUIDITY_CACHE.lock().await;
                cache.insert(primary_cache_key, (actual_liquidity, std::time::Instant::now()));
                cache.insert(curve_cache_key, (actual_liquidity, std::time::Instant::now()));
            }
            
            debug!("Primary bonding curve has {} SOL (after subtracting {} SOL rent)", actual_liquidity, RENT_EXEMPT_MINIMUM);
            
            // Log in consistent format for any RPC-based checks too
            let min_liquidity = std::env::var("MIN_LIQUIDITY")
                .unwrap_or_else(|_| "5.0".to_string())
                .parse::<f64>()
                .unwrap_or(5.0);
            
            let has_liquidity = actual_liquidity >= liquidity_threshold;
            let check_mark = if has_liquidity { "✅" } else { "❌" };
            
            // Get token name if available
            let token_name = get_token_name_for_mint(mint).unwrap_or_else(|| "Unknown".to_string());
            
            // Log consistent output format 
            info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) {:.2} SOL {}", 
                  token_name, mint, actual_liquidity, check_mark);
            
            // No need to log follow-up message since main.rs will handle that based on return value
            
            Ok((actual_liquidity >= liquidity_threshold, actual_liquidity))
        },
        Err(e) => {
            // Try looking up in the queue one more time in case the token was just detected
            if let Some(name) = get_token_name_for_mint(mint) {
                info!("🔔 PROCESSED LEVEL - DETECTED NEW TOKEN: {} ({}) 0.00 SOL ❌", 
                      name, mint);
                info!("❌ Liquidity check FAILED (0.00 SOL < {:.2} SOL) - Skipping buy", 
                      liquidity_threshold);
            }
            
            Err(anyhow!("Failed to get account info for primary bonding curve: {}", e))
        }
    }
}

/// Check only the primary bonding curve account liquidity and subtract rent exemption
/// This is faster than the original function as it only checks one account
pub async fn check_token_primary_liquidity(
    mint: &str,
    liquidity_threshold: f64,
) -> Result<(bool, f64), Box<dyn std::error::Error + Send + Sync>> {
    use solana_client::rpc_client::RpcClient;
    use solana_sdk::pubkey::Pubkey;
    use std::str::FromStr;

    // Check cache first for recent results
    let cache_key = format!("primary:{}", mint);
    {
        let cache = LIQUIDITY_CACHE.lock().await;
        if let Some((balance, timestamp)) = cache.get(&cache_key) {
            // Use cached value if less than 5 seconds old
            if timestamp.elapsed() < std::time::Duration::from_secs(5) {
                debug!(
                    "Using cached primary liquidity for {}: {} SOL",
                    mint, balance
                );
                return Ok((*balance >= liquidity_threshold, *balance));
            }
        }
    }

    // Get RPC client
    let client = get_rpc_client().await;

    // The rent exempt minimum amount in SOL
    const RENT_EXEMPT_MINIMUM: f64 = 0.00203928;

    // Convert mint address to public key
    let mint_pubkey = Pubkey::from_str(mint)
        .map_err(|e| Box::<dyn std::error::Error + Send + Sync>::from(e.to_string()))?;

    // Get the primary bonding curve address
    let (primary_bonding_curve, _) = get_bonding_curve_address(&mint_pubkey);
    debug!(
        "Checking primary bonding curve for liquidity: {}",
        primary_bonding_curve
    );

    // Check the primary bonding curve
    match client.get_account(&primary_bonding_curve) {
        Ok(account) => {
            // Get total balance
            let total_balance = account.lamports as f64 / 1_000_000_000.0; // Convert lamports to SOL

            // Subtract rent exempt minimum to get actual liquidity
            let actual_liquidity = (total_balance - RENT_EXEMPT_MINIMUM).max(0.0);

            // Update the cache
            {
                let mut cache = LIQUIDITY_CACHE.lock().await;
                cache.insert(cache_key, (actual_liquidity, std::time::Instant::now()));
            }

            debug!(
                "Primary bonding curve has {} SOL (after subtracting {} SOL rent)",
                actual_liquidity, RENT_EXEMPT_MINIMUM
            );

            Ok((actual_liquidity >= liquidity_threshold, actual_liquidity))
        }
        Err(e) => {
            debug!(
                "Error retrieving primary bonding curve account {}: {}",
                primary_bonding_curve, e
            );
            Err(Box::<dyn std::error::Error + Send + Sync>::from(format!(
                "Failed to get account info for primary bonding curve: {}",
                e
            )))
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
    pub static ref WEBSOCKET_MESSAGES: Arc<Mutex<VecDeque<String>>> =
        Arc::new(Mutex::new(VecDeque::new()));
}

/// Subscribe to bonding curve address using WebSocket instead of RPC calls.
/// This function calculates the primary bonding curve address and returns it
/// so it can be used for WebSocket subscription.
pub fn get_primary_bonding_curve_for_subscription(mint: &str) -> Option<String> {
    use solana_sdk::pubkey::Pubkey;
    use std::str::FromStr;

    // Convert mint address to public key
    if let Ok(mint_pubkey) = Pubkey::from_str(mint) {
        // Get the primary bonding curve address
        let (bonding_curve, _) = get_bonding_curve_address(&mint_pubkey);

        // Return the bonding curve address as a string
        return Some(bonding_curve.to_string());
    }

    None
}

/// Public function to connect to a WebSocket and verify connection
/// This is provided for backward compatibility
pub async fn connect_websocket_simple(url: &str) -> Result<()> {
    // This just calls our internal function and returns an Ok if the connection succeeds
    connect_to_websocket(url).await
}
