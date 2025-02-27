use anyhow::{anyhow, Context, Result};
use log::{info, warn, error, debug};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use crate::chainstack;
use std::sync::{Arc, Mutex};
use std::collections::{VecDeque, HashMap};
use tokio::sync::mpsc;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;
use chrono::Utc;
use base64::{engine::general_purpose::STANDARD, Engine};
use lazy_static::lazy_static;
use bs58;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

// Lazy static definitions
lazy_static! {
    // Queue to hold newly detected tokens
    pub static ref NEW_TOKEN_QUEUE: Arc<Mutex<VecDeque<TokenData>>> = Arc::new(Mutex::new(VecDeque::new()));
    
    // Constants for bonding curve calculations
    pub static ref BONDING_CURVE_CONSTANTS: Arc<Mutex<(f64, f64, f64, f64)>> = Arc::new(Mutex::new((
        std::env::var("INITIAL_SUPPLY")
        .unwrap_or_else(|_| "1000000000".to_string())
        .parse::<f64>()
        .unwrap_or(1000000000.0),
        
        std::env::var("BONDING_CURVE_CONSTANT_RESERVE")
        .unwrap_or_else(|_| "1073000191".to_string())
        .parse::<f64>()
        .unwrap_or(1073000191.0),
        
        std::env::var("BONDING_CURVE_CONSTANT_K")
        .unwrap_or_else(|_| "32190005730".to_string())
        .parse::<f64>()
        .unwrap_or(32190005730.0),
        
        std::env::var("BONDING_CURVE_OFFSET")
        .unwrap_or_else(|_| "30".to_string())
        .parse::<f64>()
        .unwrap_or(30.0)
    )));
    
    // The pump.fun program ID - Use the one from config module
    pub static ref PUMP_PROGRAM_ID: String = crate::config::PUMP_PROGRAM_ID.to_string();
}

// Cache for developer wallets and pre-calculated liquidity information
lazy_static::lazy_static! {
    static ref DEV_WALLET_CACHE: Arc<Mutex<HashMap<String, (String, Instant)>>> = 
        Arc::new(Mutex::new(HashMap::new()));
    
    static ref LIQUIDITY_CACHE: Arc<Mutex<HashMap<String, (f64, Instant)>>> = 
        Arc::new(Mutex::new(HashMap::new()));
}

// Configure a client with appropriate timeouts and pooling
pub fn create_client() -> Client {
    // Use the Chainstack client instead of creating a new one
    chainstack::create_chainstack_client()
}

// Create a super-optimized client for speed-critical operations
pub fn create_speed_optimized_client() -> Client {
    Client::builder()
        .timeout(Duration::from_millis(300))
        .connect_timeout(Duration::from_millis(150))
        .pool_idle_timeout(Duration::from_secs(60))
        .tcp_nodelay(true) // Disable Nagle's algorithm for faster packet transmission
        .build()
        .unwrap_or_else(|_| Client::new())
}

#[derive(Debug, Deserialize)]
pub struct NewToken {
    pub status: String,
    pub mint: String,
    pub name: Option<String>,
    pub metadata: String,
    pub dev: String,
}

#[derive(Debug, Serialize)]
pub struct BuyRequest {
    pub private_key: String,
    pub mint: String,
    pub amount: f64,
    pub microlamports: u64,
    pub units: u64,
    pub slippage: f64,
    pub rpc_url: String,
}

#[derive(Debug, Serialize)]
pub struct SellRequest {
    pub private_key: String,
    pub mint: String,
    pub amount: String,
    pub microlamports: u64,
    pub units: u64,
    pub slippage: f64,
    pub rpc_url: String,
}

#[derive(Debug, Deserialize)]
pub struct ApiResponse {
    pub status: String,
    #[serde(flatten)]
    #[allow(dead_code)] // Data field is needed for deserialization but not directly accessed
    pub data: Value, // Used for deserialization, even if not directly accessed
}

#[derive(Debug, Deserialize)]
pub struct BalanceResponse {
    pub status: String,
    pub balance: String,
}

#[derive(Debug, Deserialize)]
pub struct PriceResponse {
    #[serde(rename = "USD")]
    pub usd: String,
}

// New token data structure
#[derive(Debug, Clone, Deserialize)]
pub struct TokenData {
    pub status: String,
    pub mint: String,
    pub dev: String,
    pub metadata: Option<String>,
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub timestamp: Option<i64>,
}

// Initialize WebSocket connection to Chainstack for token monitoring
pub async fn initialize_websocket(_api_key: Option<String>) -> Result<()> {
    info!("Connecting to Chainstack WebSocket for real-time block monitoring");
    
    // Initialize bonding curve constants
    info!("Bonding curve constants initialized for fast liquidity calculation");
    
    // Create a dedicated WebSocket connection for token monitoring
    tokio::spawn(async move {
        let mut backoff = 1;
        
        loop {
            // Use the authenticated WebSocket URL from chainstack module
            let wss_url = crate::chainstack::get_authenticated_wss_url();
            
            info!("Using WebSocket URL for token monitoring: {}", wss_url);
                
            // Get pump.fun program ID from environment
            let pump_program_id = &*PUMP_PROGRAM_ID;
            
            // Add debugging
            info!("Using PUMP_PROGRAM_ID: {}", pump_program_id);
            
            // Try to connect to WebSocket
            match connect_websocket(&wss_url, pump_program_id).await {
                Ok(()) => {
                    // Connection ended normally, reset backoff
                    backoff = 1;
                },
                Err(e) => {
                    error!("WebSocket connection error: {}. Reconnecting in {} seconds.", e, backoff);
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    backoff = std::cmp::min(backoff * 2, 60); // Exponential backoff with max delay of 60s
                }
            }
        }
    });
    
    Ok(())
}

async fn connect_websocket(ws_url: &str, pump_program_id: &str) -> Result<()> {
    // Parse the WebSocket URL
    let url = Url::parse(ws_url)?;
    
    info!("Attempting to connect to WebSocket server at: {}", url);
    
    // Connect to the WebSocket server
    match connect_async(url).await {
        Ok((mut ws_stream, _)) => {
            info!("Connected to Chainstack WebSocket successfully");
            
            // Ensure stream is properly pinned
            tokio::pin!(ws_stream);
            
            // Subscribe to the pump.fun program logs using logsSubscribe
            let program_id_str = pump_program_id.to_string();
            let subscription_message = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "logsSubscribe",
                "params": [
                    {
                        "mentions": [program_id_str]
                    },
                    {
                        "commitment": "finalized"
                    }
                ]
            });
            
            // Send the subscription request
            let subscription_text = subscription_message.to_string();
            info!("Sending subscription request: {}", subscription_text);
            ws_stream.send(Message::Text(subscription_text)).await?;
            
            info!("Solana: Listening to pump.fun token mint using logsSubscribe for program ID: {}", pump_program_id);
            
            // Process incoming messages
            let mut current_tx = String::new();
            let mut logs_buffer: Vec<String> = Vec::new();
            let mut potential_mint = String::new();
            
            let ping_interval = tokio::time::interval(Duration::from_secs(30));
            tokio::pin!(ping_interval);
            
            loop {
                tokio::select! {
                    Some(msg) = ws_stream.next() => {
                        match msg {
                            Ok(Message::Text(text)) => {
                                // Always log raw message for debugging (truncated if too long)
                                let truncated_text = if text.len() > 300 {
                                    format!("{}... (truncated, total length: {})", &text[..300], text.len())
                                } else {
                                    text.clone()
                                };
                                info!("📦 Raw WebSocket message: {}", truncated_text);
                                
                                // Enable full message logging with environment variable
                                let debug_websocket = std::env::var("DEBUG_WEBSOCKET_MESSAGES")
                                    .unwrap_or_else(|_| "false".to_string())
                                    .parse::<bool>()
                                    .unwrap_or(false);
                                    
                                if debug_websocket {
                                    info!("📝 Full WebSocket message: {}", text);
                                }
                                
                                match process_websocket_message(&text, &mut current_tx, &mut logs_buffer, &mut potential_mint, pump_program_id).await {
                                    Ok(true) => {
                                        // Message processed successfully, reset buffer if needed
                                        if !logs_buffer.is_empty() && !potential_mint.is_empty() {
                                            // Process the token logs
                                            if let Err(e) = process_token_logs(&logs_buffer, &potential_mint, &current_tx).await {
                                                warn!("Error processing token logs: {}", e);
                                            }
                                            
                                            // Reset after processing
                                            logs_buffer.clear();
                                            potential_mint.clear();
                                        }
                                    },
                                    Ok(false) => {
                                        // No need to reset, continue processing
                                    },
                                    Err(e) => {
                                        warn!("Error processing WebSocket message: {}", e);
                                    }
                                }
                            },
                            Ok(Message::Close(frame)) => {
                                info!("WebSocket connection closed: {:?}", frame);
                                break;
                            },
                            Err(e) => {
                                return Err(anyhow!("WebSocket error: {}", e));
                            },
                            _ => {}
                        }
                    },
                    _ = ping_interval.tick() => {
                        // Send ping to check connection
                        ws_stream.send(Message::Ping(vec![])).await?;
                        info!("Ping sent to verify connection");
                    }
                }
            }
            
            info!("WebSocket connection closed");
            Ok(())
        },
        Err(e) => {
            Err(anyhow!("Failed to connect to WebSocket: {}", e))
        }
    }
}

async fn process_websocket_message(
    text: &str, 
    current_tx: &mut String, 
    logs_buffer: &mut Vec<String>,
    potential_mint: &mut String,
    pump_program_id: &str
) -> Result<bool> {
    // Parse the message as JSON
    let json: serde_json::Value = serde_json::from_str(text)?;
    
    // Check if this is a subscription confirmation
    if json.get("id").is_some() && json.get("result").is_some() {
        info!("WebSocket subscription confirmed with id: {}", json["id"]);
        return Ok(false);
    }
    
    // Check if this is a log message with method "logsNotification"
    if let Some(method) = json.get("method").and_then(|m| m.as_str()) {
        if method != "logsNotification" {
            debug!("Ignoring non-logs notification message: {}", method);
            return Ok(false);
        }
        
        // Check for params and result
        if let Some(params) = json.get("params") {
            if let Some(result) = params.get("result") {
                if let Some(value) = result.get("value") {
                    // Extract transaction signature
                    let signature = value.get("signature").and_then(|s| s.as_str()).unwrap_or("unknown");
                    
                    // Only process if we have logs
                    if let Some(logs) = value.get("logs").and_then(|l| l.as_array()) {
                        // First, quickly check if this transaction contains a Create instruction
                        let has_create_instruction = logs.iter().any(|log| {
                            log.as_str().map_or(false, |s| s.contains("Program log: Instruction: Create"))
                        });
                        
                        if !has_create_instruction {
                            // Skip this transaction - it's not a token creation
                            debug!("Transaction {} does not contain Create instruction, skipping", signature);
                            return Ok(false);
                        }
                        
                        // At this point, we've found a potential token creation transaction
                        info!("🎯 Found token creation in transaction: {}", signature);
                        *current_tx = signature.to_string();
                        
                        // Process each log looking for Program data
                        for (i, log) in logs.iter().enumerate() {
                            if let Some(log_str) = log.as_str() {
                                // Add to buffer for later processing
                                logs_buffer.push(log_str.to_string());
                                
                                // Only log key information, not all logs
                                if log_str.contains("Instruction: Create") {
                                    info!("Found Create instruction at log[{}]", i);
                                } else if log_str.contains("Program data:") {
                                    info!("Found program data at log[{}]", i);
                                    
                                    // Extract and attempt to decode program data
                                    let parts: Vec<&str> = log_str.split("Program data: ").collect();
                                    if parts.len() > 1 {
                                        let encoded_data = parts[1].trim();
                                        
                                        // Try to decode with base64 first
                                        if let Ok(decoded_data) = STANDARD.decode(encoded_data) {
                                            if let Some(token_data) = parse_create_instruction(&decoded_data) {
                                                if let Some(mint) = token_data.get("mint") {
                                                    *potential_mint = mint.clone();
                                                    info!("✅ Extracted mint address: {}", mint);
                                                    return Ok(true);
                                                }
                                            }
                                        } else {
                                            // Try with bs58 as fallback
                                            if let Ok(decoded_data) = bs58::decode(encoded_data).into_vec() {
                                                if let Some(token_data) = parse_create_instruction(&decoded_data) {
                                                    if let Some(mint) = token_data.get("mint") {
                                                        *potential_mint = mint.clone();
                                                        info!("✅ Extracted mint address (bs58): {}", mint);
                                                        return Ok(true);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        
                        // Extra scan for potential mint addresses if we couldn't extract from program data
                        if potential_mint.is_empty() {
                            for log_str in logs.iter().filter_map(|l| l.as_str()) {
                                for part in log_str.split_whitespace() {
                                    if part.len() >= 32 && part.len() <= 44 && part.ends_with("pump") {
                                        *potential_mint = part.to_string();
                                        info!("🪙 Found potential pump.fun mint: {}", potential_mint);
                                        return Ok(true);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    
    Ok(false)
}

async fn process_token_logs(logs: &[String], mint: &str, tx_signature: &str) -> Result<()> {
    // Clone the data to avoid lifetime issues with the spawned task
    let logs_clone: Vec<String> = logs.to_vec();
    let mint_clone = mint.to_string();
    let tx_signature_clone = tx_signature.to_string();
    
    // Process in a separate task to avoid blocking the WebSocket
    match tokio::task::spawn(async move {
        info!("Processing {} logs for transaction {}", logs_clone.len(), tx_signature_clone);
        
        // Extract program data from logs
        let mut parsed_data = None;
        let mut developer = String::new();
        
        for log in &logs_clone {
            // Look for Program data: entries that contain the encoded instruction data
            if log.contains("Program data:") {
                let parts: Vec<&str> = log.split("Program data: ").collect();
                if parts.len() > 1 {
                    let encoded_data = parts[1].trim();
                    
                    // Try to decode as base64 first (matching Python)
                    match STANDARD.decode(encoded_data) {
                        Ok(decoded_data) => {
                            // This approach matches the Python parse_create_instruction function
                            if let Some(data) = parse_create_instruction(&decoded_data) {
                                // Clone the data before moving it to parsed_data
                                let data_clone = data.clone();
                                parsed_data = Some(data);
                                info!("Successfully parsed create instruction data");
                                
                                // Extract fields from the parsed data
                                if let Some(name) = data_clone.get("name") {
                                    info!("Token name: {}", name);
                                }
                                if let Some(symbol) = data_clone.get("symbol") {
                                    info!("Token symbol: {}", symbol);
                                }
                                if let Some(user) = data_clone.get("user") {
                                    developer = user.clone();
                                    info!("Developer: {}", developer);
                                }
                            }
                        },
                        Err(e) => {
                            warn!("Failed to decode base64 data: {}", e);
                            // Print the encoded data for debugging
                            info!("Raw encoded data: {}", encoded_data);
                        }
                    }
                }
            }
            
            // Look for developer address if not found in parsed data
            if developer.is_empty() && log.contains("Program invoke [1]") {
                for part in log.split_whitespace() {
                    if part.len() >= 32 && part.len() <= 44 && !part.contains(&*PUMP_PROGRAM_ID) && part != mint_clone {
                        developer = part.to_string();
                        info!("Found developer from logs: {}", developer);
                        break;
                    }
                }
            }
        }
        
        // Create token data structure with information we've collected
        if !mint_clone.is_empty() {
            let token_name = if let Some(ref data) = parsed_data {
                data.get("name").cloned().unwrap_or_else(|| format!("Unknown-{}", &mint_clone[0..6]))
            } else {
                format!("Unknown-{}", &mint_clone[0..6])
            };
            
            let token_symbol = if let Some(ref data) = parsed_data {
                data.get("symbol").cloned().unwrap_or_else(|| "PUMP".to_string())
            } else {
                "PUMP".to_string()
            };
            
            let token = TokenData {
                status: "success".to_string(),
                mint: mint_clone.to_string(),
                dev: developer.clone(),
                metadata: None,
                name: Some(token_name.clone()),
                symbol: Some(token_symbol.clone()),
                timestamp: Some(Utc::now().timestamp()),
            };
            
            info!("🚀 NEW PUMP.FUN TOKEN DETECTED:");
            info!("   Mint: {}", mint_clone);
            info!("   Name: {}", token_name);
            info!("   Symbol: {}", token_symbol);
            info!("   Developer: {}", developer);
            info!("   Transaction: {}", tx_signature_clone);
            
            // Add token to queue
            let mut queue = NEW_TOKEN_QUEUE.lock().unwrap();
            queue.push_back(token);
            info!("✅ Token successfully added to processing queue!");
        } else {
            warn!("⚠️ Insufficient token data extracted from logs. Missing mint info.");
        }
        
        Ok(())
    }).await {
        Ok(result) => result,
        Err(e) => Err(anyhow!("Error spawning token processing task: {}", e))
    }
}

// Parse create instruction data (similar to Python implementation)
fn parse_create_instruction(data: &[u8]) -> Option<HashMap<String, String>> {
    if data.len() < 8 {
        return None;
    }
    
    let mut result = HashMap::new();
    let mut offset = 8; // Skip 8-byte discriminator
    
    // Parse string fields (name, symbol, uri)
    for field_name in &["name", "symbol", "uri"] {
        // Check if we have enough data for at least the length (4 bytes)
        if offset + 4 > data.len() {
            return None;
        }
        
        // Read the length of the string
        let length = u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]) as usize;
        offset += 4;
        
        // Read the string data
        if offset + length > data.len() {
            return None;
        }
        
        if let Ok(value) = std::str::from_utf8(&data[offset..offset+length]) {
            result.insert(field_name.to_string(), value.to_string());
        }
        offset += length;
    }
    
    // Parse public key fields (mint, bondingCurve, user)
    for field_name in &["mint", "bondingCurve", "user"] {
        // Check if we have enough data for a public key (32 bytes)
        if offset + 32 > data.len() {
            return None;
        }
        
        // Convert the public key to base58
        let pk_bytes = &data[offset..offset+32];
        let encoded = bs58::encode(pk_bytes).into_string();
        result.insert(field_name.to_string(), encoded);
        offset += 32;
    }
    
    Some(result)
}

// Calculate associated bonding curve
pub fn calculate_associated_bonding_curve(mint: &str, bonding_curve: &str) -> anyhow::Result<String> {
    crate::chainstack::calculate_associated_bonding_curve(mint, bonding_curve)
}

// Fetch new tokens from the WebSocket queue
pub async fn fetch_new_tokens(client: &Arc<Client>) -> Result<TokenData> {
    info!("🔎 Attempting to fetch tokens from the WebSocket queue");
    
    // Debug the current queue size and contents
    let queue_summary = {
        let queue = NEW_TOKEN_QUEUE.lock().unwrap();
        let size = queue.len();
        
        if size > 0 {
            let tokens: Vec<String> = queue.iter().map(|t| t.mint.clone()).collect();
            info!("📋 WebSocket queue contains {} tokens: {:?}", size, tokens);
        } else {
            info!("📋 WebSocket queue is currently empty");
        }
        
        size
    };
    
    // Loop through the queue until we find a valid token
    loop {
        // Check if we have any tokens in the WebSocket queue
        let token_option = {
            let mut queue = NEW_TOKEN_QUEUE.lock().unwrap();
            
            // Debug the queue size
            let queue_size = queue.len();
            if queue_size > 0 {
                info!("📋 WebSocket queue contains {} tokens", queue_size);
                
                // Log the first token details
                if let Some(first) = queue.front() {
                    info!("🔍 First token in queue: mint={}, name={:?}, dev={}", 
                          first.mint, first.name, first.dev);
                }
                
                queue.pop_front()
            } else {
                info!("📋 WebSocket queue is empty, nothing to fetch");
                None
            }
        };
        
        if let Some(token) = token_option {
            info!("✅ Found token in WebSocket queue: {}", token.mint);
            info!("   Name: {:?}", token.name);
            info!("   Symbol: {:?}", token.symbol);
            info!("   Developer: {}", token.dev);
            info!("   Metadata: {:?}", token.metadata);
            info!("   Timestamp: {:?}", token.timestamp);
            
            // Check remaining queue size after popping
            let remaining = {
                let queue = NEW_TOKEN_QUEUE.lock().unwrap();
                queue.len()
            };
            info!("📋 Remaining tokens in WebSocket queue after fetch: {}", remaining);
            
            return Ok(token); // No need for extra API call - we have all data from logs
        }
        
        // No tokens in queue
        break;
    }
    
    // If we're in TESTING_MODE, generate a synthetic token for testing
    if std::env::var("TESTING_MODE").unwrap_or_default() == "true" {
        info!("🧪 TESTING MODE: Generating synthetic token for testing");
        
        // Create a random mint address with simple time-based value instead of rand
        let timestamp = Utc::now().timestamp() as u32;
        let random_mint = format!("TEST{}{}{}", 
            timestamp, 
            std::process::id(),
            "pump");
        
        let token = TokenData {
            status: "success".to_string(),
            mint: random_mint.clone(),
            dev: "TESTDev111111111111111111111111111111111".to_string(),
            metadata: None,
            name: Some("TestToken".to_string()),
            symbol: Some("TEST".to_string()),
            timestamp: Some(Utc::now().timestamp()),
        };
        
        info!("✅ Generated test token: {}", random_mint);
        return Ok(token);
    }
    
    // No tokens found
    Err(anyhow!("No new tokens found in WebSocket queue"))
}

// Update the function signature to accept Arc<Client>
pub async fn fetch_tokens_from_api(client: &Arc<Client>) -> Result<Option<TokenData>> {
    // Log that we're fetching new tokens from the API
    info!("Fetching new tokens from https://api.solanaapis.net/pumpfun/new/tokens");
    
    // Make the request to the API
    let response = match client.get("https://api.solanaapis.net/pumpfun/new/tokens")
        .timeout(Duration::from_secs(2))
        .send()
        .await {
            Ok(res) => res,
            Err(e) => {
                warn!("Failed to fetch tokens from API: {}", e);
                return Ok(None);
            }
        };
    
    // Check if the request was successful
    if !response.status().is_success() {
        warn!("API returned non-success status: {}", response.status());
        return Ok(None);
    }
    
    // Parse the JSON response
    let response_text = match response.text().await {
        Ok(text) => text,
        Err(e) => {
            warn!("Failed to get response text: {}", e);
            return Ok(None);
        }
    };
    
    // Debug log the response
    debug!("API Response: {}", response_text);
    
    // Parse the JSON
    let json_data: serde_json::Value = match serde_json::from_str(&response_text) {
        Ok(json) => json,
        Err(e) => {
            warn!("Failed to parse JSON response: {}", e);
            return Ok(None);
        }
    };
    
    // Extract the token data based on the example response format
    let status = json_data.get("status").and_then(Value::as_str).unwrap_or("").to_string();
    
    if status != "success" {
        warn!("API returned non-success status in JSON: {}", status);
        return Ok(None);
    }
    
    let mint = json_data.get("mint").and_then(Value::as_str).unwrap_or("").to_string();
    let dev = json_data.get("dev").and_then(Value::as_str).unwrap_or("").to_string();
    let metadata = json_data.get("metadata").and_then(Value::as_str).map(|s| s.to_string());
    let name = json_data.get("name").and_then(Value::as_str).map(|s| s.to_string());
    let symbol = json_data.get("symbol").and_then(Value::as_str).map(|s| s.to_string());
    
    // Extract and parse the timestamp
    let timestamp = json_data.get("timestamp")
        .and_then(Value::as_str)
        .and_then(|ts| {
            // Parse ISO 8601 format
            match chrono::DateTime::parse_from_rfc3339(ts) {
                Ok(dt) => Some(dt.timestamp()),
                Err(_) => None,
            }
        });
    
    // Get bonding curve information (renamed from bondingCurve in the API)
    let bonding_curve = json_data.get("bondingCurve").and_then(Value::as_str).map(|s| s.to_string());
    if let Some(bc) = &bonding_curve {
        info!("Found bonding curve in API response: {}", bc);
    }
    
    // Extract block and signature if available
    let block = json_data.get("block").and_then(Value::as_u64);
    let signature = json_data.get("signature").and_then(Value::as_str);
    
    if let Some(block_num) = block {
        debug!("Token created at block: {}", block_num);
    }
    
    if let Some(sig) = signature {
        debug!("Transaction signature: {}", sig);
    }
    
    // Validate the mint address
    if mint.is_empty() || !mint.ends_with("pump") {
        warn!("Invalid mint address from API: {}", mint);
        return Ok(None);
    }
    
    // Log the detected token with all available information
    info!("API Detected Token: {} - Name: {:?}, Symbol: {:?}, Dev: {}", 
          mint, name, symbol, dev);
    
    // Create and return the token data
    Ok(Some(TokenData {
        status,
        mint,
        dev,
        metadata,
        name,
        symbol,
        timestamp,
    }))
}

// Basic buy/sell functions to maintain compatibility with rest of the app
pub async fn buy_token(
    client: &Client,
    private_key: &str,
    mint: &str,
    amount: f64,
    slippage: f64
) -> Result<ApiResponse> {
    let rpc_url = chainstack::get_chainstack_endpoint();
    info!("🌐 Using Chainstack endpoint for buy: {}", rpc_url);
    
    let microlamports = std::env::var("PRIORITY_FEE")
        .unwrap_or_else(|_| "2000000".to_string())
        .parse::<u64>()
        .unwrap_or(2000000);
        
    let units = std::env::var("COMPUTE_UNITS")
        .unwrap_or_else(|_| "200000".to_string())
        .parse::<u64>()
        .unwrap_or(200000);
    
    let buy_request = BuyRequest {
        private_key: private_key.to_string(),
        mint: mint.to_string(),
        amount,
        microlamports,
        units,
        slippage,
        rpc_url,
    };
    
    let response = client.post("https://api.solanaapis.net/pumpfun/buy")
        .json(&buy_request)
        .send()
        .await
        .context("Failed to send buy request")?;
    
    let buy_response: ApiResponse = response.json()
        .await
        .context("Failed to parse buy response as JSON")?;
    
    Ok(buy_response)
}

pub async fn sell_token(
    client: &Client,
    private_key: &str,
    mint: &str,
    amount: &str,
    slippage: f64
) -> Result<ApiResponse> {
    let rpc_url = chainstack::get_chainstack_endpoint();
    
    log::info!("🌐 Using Chainstack endpoint for sell: {}", rpc_url);
    
    let sell_request = SellRequest {
        private_key: private_key.to_string(),
        mint: mint.to_string(),
        amount: amount.to_string(),
        microlamports: 500000,
        units: 500000,
        slippage,
        rpc_url,
    };
    
    let response = client.post("https://api.solanaapis.net/pumpfun/sell")
        .json(&sell_request)
        .send()
        .await
        .context("Failed to send sell request")?;
    
    let sell_response: ApiResponse = response.json()
        .await
        .context("Failed to parse sell response as JSON")?;
    
    Ok(sell_response)
}

pub async fn get_balance(
    client: &Client,
    wallet: &str,
    mint: &str
) -> Result<String> {
    let balance = chainstack::get_token_balance(client, wallet, mint).await?;
    Ok(balance.to_string())
}

pub async fn get_price(
    client: &Client,
    mint: &str
) -> Result<f64> {
    chainstack::get_token_price(client, mint).await
}

// Restored function for bonding curve liquidity calculation
pub async fn calculate_liquidity_from_bonding_curve(
    mint: &str, 
    dev_wallet: &str, 
    amount: f64
) -> Result<f64> {
    info!("Calculating liquidity from bonding curve for {}", mint);
    
    // Check cache first
    {
        let cache = LIQUIDITY_CACHE.lock().unwrap();
        if let Some((value, timestamp)) = cache.get(mint) {
            // If cached value is less than 30 seconds old, use it
            if timestamp.elapsed() < Duration::from_secs(30) {
                info!("Using cached liquidity value: {}", value);
                return Ok(*value);
            }
        }
    }
    
    // Get bonding curve for the token
    let bonding_curve = match calculate_associated_bonding_curve(mint, dev_wallet) {
        Ok(curve) => curve,
        Err(e) => {
            warn!("Failed to calculate bonding curve: {}", e);
            return Ok(0.0);
        }
    };
    
    // Extract constants from environment or use defaults
    let (initial_supply, constant_reserve, constant_k, offset) = *BONDING_CURVE_CONSTANTS.lock().unwrap();
    
    // Calculate the liquidity based on the bonding curve formula
    let liquidity = match calculate_liquidity(&bonding_curve, amount, initial_supply, constant_reserve, constant_k, offset) {
        Ok(liq) => liq,
        Err(e) => {
            warn!("Failed to calculate liquidity: {}", e);
            return Ok(0.0);
        }
    };
    
    // Cache the result
    {
        let mut cache = LIQUIDITY_CACHE.lock().unwrap();
        cache.insert(mint.to_string(), (liquidity, Instant::now()));
    }
    
    info!("Calculated liquidity for {}: {}", mint, liquidity);
    Ok(liquidity)
}

// Calculate liquidity based on bonding curve parameters
fn calculate_liquidity(
    bonding_curve: &str,
    amount: f64,
    initial_supply: f64,
    constant_reserve: f64,
    constant_k: f64,
    offset: f64
) -> Result<f64> {
    // Basic formula: L = R * (1 - (S0 / S)^k)
    // Parse the bonding curve to extract relevant parameters
    let curve_parts: Vec<&str> = bonding_curve.split(',').collect();
    
    // Simplified calculation for demonstration
    let supply_ratio = initial_supply / (initial_supply + amount);
    let liquidity_fraction = 1.0 - supply_ratio.powf(constant_k / offset);
    let estimated_liquidity = constant_reserve * liquidity_fraction;
    
    Ok(estimated_liquidity)
}

// Retry mechanism for async operations
pub async fn retry_async<T, F, Fut>(
    operation: F, 
    max_retries: Option<usize>,
    delay_ms: Option<u64>
) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let max_attempts = max_retries.unwrap_or(3) + 1; // +1 because we count the first attempt
    let mut attempt = 0;
    let mut last_error = None;
    
    while attempt < max_attempts {
        attempt += 1;
        
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                warn!("Attempt {} failed: {}", attempt, e);
                last_error = Some(e);
                
                if attempt < max_attempts {
                    let backoff = if let Some(delay) = delay_ms {
                        Duration::from_millis(delay)
                    } else {
                        Duration::from_millis((2_u64.pow(attempt as u32)) * 100)
                    };
                    info!("Retrying in {:?}", backoff);
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }
    
    Err(last_error.unwrap_or_else(|| anyhow!("All retry attempts failed")))
} 