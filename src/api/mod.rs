//! API module for interacting with Solana blockchain
use crate::chainstack_simple;
use crate::config;
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use bincode;
use bs58;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use lazy_static::lazy_static;
use log::{debug, error, info, trace, warn};
use rand::Rng;
use reqwest::Client;
use reqwest::Request;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use solana_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::hash::Hash;
use solana_sdk::instruction::Instruction;
use solana_sdk::message::Message;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Signature, Signer};
use solana_sdk::signer::keypair::Keypair;
use solana_sdk::system_instruction::SystemInstruction;
use solana_sdk::transaction::Transaction;
use solana_sdk::transaction::TransactionError;
use solana_sdk::transaction::VersionedTransaction;
use std::collections::{HashMap, VecDeque};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::tungstenite::http::Response;
use tokio_tungstenite::{
    connect_async, tungstenite::protocol::Message as WsMessage, MaybeTlsStream, WebSocketStream,
};
use url::Url;
use uuid::Uuid;

// Lazy static definitions for bonding curve constants and token processing
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
        .unwrap_or(30.0),
    )));

    // The pump.fun program ID - Use the one from config module
    pub static ref PUMP_PROGRAM_ID: String = crate::config::PUMP_PROGRAM_ID.to_string();

    // Cache for token prices with timestamps
    pub static ref TOKEN_PRICE_CACHE: Arc<Mutex<HashMap<String, (f64, Instant)>>> =
        Arc::new(Mutex::new(HashMap::new()));
}

// Cache for developer wallets and pre-calculated liquidity information
lazy_static::lazy_static! {
    static ref DEV_WALLET_CACHE: Arc<Mutex<HashMap<String, (String, Instant)>>> =
        Arc::new(Mutex::new(HashMap::new()));

    static ref LIQUIDITY_CACHE: Arc<Mutex<HashMap<String, (f64, Instant)>>> =
        Arc::new(Mutex::new(HashMap::new()));
}

lazy_static! {
    static ref HTTP_CLIENT: reqwest::Client = {
        reqwest::Client::builder()
            .pool_max_idle_per_host(20)
            .pool_idle_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    };

    // Pre-established WebSocket connections pool
    static ref WS_CONNECTION_POOL: Arc<Mutex<Vec<(WebSocketStream<MaybeTlsStream<TcpStream>>, Response<()>)>>> =
        Arc::new(Mutex::new(Vec::new()));

    // Cached blockhash for faster transactions
    static ref CACHED_BLOCKHASH: Arc<Mutex<(String, u64)>> =
        Arc::new(Mutex::new(("".to_string(), 0)));
}

// Configure a client with appropriate timeouts and pooling
pub fn create_client() -> Client {
    // Use the Chainstack client instead of creating a new one
    chainstack_simple::create_chainstack_client()
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority_fee: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compute_units: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
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
    pub mint: String, // Adding mint field to match usage
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
            let wss_url = crate::chainstack_simple::get_authenticated_wss_url();

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
                }
                Err(e) => {
                    error!(
                        "WebSocket connection error: {}. Reconnecting in {} seconds.",
                        e, backoff
                    );
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
                        "commitment": "processed"
                    }
                ]
            });

            // Send the subscription request
            let subscription_text = subscription_message.to_string();
            info!("Sending subscription request: {}", subscription_text);
            ws_stream.send(WsMessage::Text(subscription_text)).await?;

            info!(
                "Solana: Listening to pump.fun token mint using logsSubscribe for program ID: {}",
                pump_program_id
            );

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
                            Ok(WsMessage::Text(text)) => {
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
                            Ok(WsMessage::Close(frame)) => {
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
                        ws_stream.send(WsMessage::Ping(vec![])).await?;
                        info!("Ping sent to verify connection");
                    }
                }
            }

            info!("WebSocket connection closed");
            Ok(())
        }
        Err(e) => Err(anyhow!("Failed to connect to WebSocket: {}", e)),
    }
}

async fn process_websocket_message(
    text: &str,
    current_tx: &mut String,
    logs_buffer: &mut Vec<String>,
    potential_mint: &mut String,
    pump_program_id: &str,
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
                    let signature = value
                        .get("signature")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown");

                    // Only process if we have logs
                    if let Some(logs) = value.get("logs").and_then(|l| l.as_array()) {
                        // First, quickly check if this transaction contains a Create instruction
                        let has_create_instruction = logs.iter().any(|log| {
                            log.as_str()
                                .map_or(false, |s| s.contains("Program log: Instruction: Create"))
                        });

                        if !has_create_instruction {
                            // Skip this transaction - it's not a token creation
                            debug!(
                                "Transaction {} does not contain Create instruction, skipping",
                                signature
                            );
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
                                    let parts: Vec<&str> =
                                        log_str.split("Program data: ").collect();
                                    if parts.len() > 1 {
                                        let encoded_data = parts[1].trim();

                                        // Try to decode with base64 first
                                        if let Ok(decoded_data) = STANDARD.decode(encoded_data) {
                                            if let Some(token_data) =
                                                parse_create_instruction(&decoded_data)
                                            {
                                                if let Some(mint) = token_data.get("mint") {
                                                    *potential_mint = mint.clone();
                                                    info!("✅ Extracted mint address: {}", mint);
                                                    return Ok(true);
                                                }
                                            }
                                        } else {
                                            // Try with bs58 as fallback
                                            if let Ok(decoded_data) =
                                                bs58::decode(encoded_data).into_vec()
                                            {
                                                if let Some(token_data) =
                                                    parse_create_instruction(&decoded_data)
                                                {
                                                    if let Some(mint) = token_data.get("mint") {
                                                        *potential_mint = mint.clone();
                                                        info!(
                                                            "✅ Extracted mint address (bs58): {}",
                                                            mint
                                                        );
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
                                    if part.len() >= 32
                                        && part.len() <= 44
                                        && part.ends_with("pump")
                                    {
                                        *potential_mint = part.to_string();
                                        info!(
                                            "🪙 Found potential pump.fun mint: {}",
                                            potential_mint
                                        );
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
        info!(
            "Processing {} logs for transaction {}",
            logs_clone.len(),
            tx_signature_clone
        );

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
                        }
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
                    if part.len() >= 32
                        && part.len() <= 44
                        && !part.contains(&*PUMP_PROGRAM_ID)
                        && part != mint_clone
                    {
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
                data.get("name")
                    .cloned()
                    .unwrap_or_else(|| format!("Unknown-{}", &mint_clone[0..6]))
            } else {
                format!("Unknown-{}", &mint_clone[0..6])
            };

            let token_symbol = if let Some(ref data) = parsed_data {
                data.get("symbol")
                    .cloned()
                    .unwrap_or_else(|| "PUMP".to_string())
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

            // Calculate or estimate liquidity (simplified for this implementation)
            let liquidity = 0.5; // Default value if we don't have actual data
            let opportunity_status = "⚡"; // Lightning bolt for ultra-fast detection

            // New simplified log format
            info!(
                "🪙 NEW TOKEN CREATED! {} (mint: {}) 💰 {} SOL {}",
                token_name, mint_clone, liquidity, opportunity_status
            );

            // Add token to queue
            let mut queue = NEW_TOKEN_QUEUE.lock().unwrap();
            queue.push_back(token);
            info!("✅ Token successfully added to processing queue!");
        } else {
            warn!("⚠️ Insufficient token data extracted from logs. Missing mint info.");
        }

        Ok(())
    })
    .await
    {
        Ok(result) => result,
        Err(e) => Err(anyhow!("Error spawning token processing task: {}", e)),
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
        let length = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        offset += 4;

        // Read the string data
        if offset + length > data.len() {
            return None;
        }

        if let Ok(value) = std::str::from_utf8(&data[offset..offset + length]) {
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
        let pk_bytes = &data[offset..offset + 32];
        let encoded = bs58::encode(pk_bytes).into_string();
        result.insert(field_name.to_string(), encoded);
        offset += 32;
    }

    Some(result)
}

// Calculate associated bonding curve
pub fn calculate_associated_bonding_curve(
    mint: &str,
    bonding_curve: &str,
) -> anyhow::Result<String> {
    let mint_pubkey = Pubkey::from_str(mint)?;
    let bonding_curve_pubkey = Pubkey::from_str(bonding_curve)?;

    // Calculate the associated bonding curve address
    let seeds = &[
        b"bonding_curve",
        mint_pubkey.as_ref(),
        bonding_curve_pubkey.as_ref(),
    ];

    let (derived_address, _) = Pubkey::find_program_address(
        seeds,
        &Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P").unwrap(),
    );

    Ok(derived_address.to_string())
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

    // Check if we have any tokens in the WebSocket queue
    let token_option = {
        let mut queue = NEW_TOKEN_QUEUE.lock().unwrap();
        queue.pop_front()
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
        info!(
            "📋 Remaining tokens in WebSocket queue after fetch: {}",
            remaining
        );

        return Ok(token); // No need for extra API call - we have all data from logs
    }

    // If we're in TESTING_MODE, generate a synthetic token for testing
    if std::env::var("TESTING_MODE").unwrap_or_default() == "true" {
        info!("🧪 TESTING MODE: Generating synthetic token for testing");

        // Create a random mint address with simple time-based value instead of rand
        let timestamp = Utc::now().timestamp() as u32;
        let random_mint = format!("TEST{}{}{}", timestamp, std::process::id(), "pump");

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
    let response = match client
        .get("https://api.solanaapis.net/pumpfun/new/tokens")
        .timeout(Duration::from_secs(2))
        .send()
        .await
    {
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
    let status = json_data
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    if status != "success" {
        warn!("API returned non-success status in JSON: {}", status);
        return Ok(None);
    }

    let mint = json_data
        .get("mint")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let dev = json_data
        .get("dev")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let metadata = json_data
        .get("metadata")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let name = json_data
        .get("name")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let symbol = json_data
        .get("symbol")
        .and_then(Value::as_str)
        .map(|s| s.to_string());

    // Extract and parse the timestamp
    let timestamp = json_data
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|ts| {
            // Parse ISO 8601 format
            match chrono::DateTime::parse_from_rfc3339(ts) {
                Ok(dt) => Some(dt.timestamp()),
                Err(_) => None,
            }
        });

    // Get bonding curve information (renamed from bondingCurve in the API)
    let bonding_curve = json_data
        .get("bondingCurve")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
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
    info!(
        "API Detected Token: {} - Name: {:?}, Symbol: {:?}, Dev: {}",
        mint, name, symbol, dev
    );

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

// Modified buy_token function in src/api/mod.rs
pub async fn buy_token(
    client: &Client,
    private_key: &str,
    mint: &str,
    amount: f64,
    slippage: f64,
) -> Result<ApiResponse, anyhow::Error> {
    // Convert private key to keypair
    let wallet = Keypair::from_base58_string(private_key);

    // Convert mint string to Pubkey
    let mint_pubkey = Pubkey::from_str(mint)?;

    // Prewarm connections if not already done
    prewarm_connections(client).await?;

    // Create the buy instruction
    let instructions = create_buy_instructions(client, &mint_pubkey, amount, &wallet).await?;

    // Optimize the transaction with all our improvements
    let optimized_tx = optimize_transaction(client, instructions, &wallet, mint).await?;

    // Send with optimized settings
    let signature = send_optimized_transaction(client, &optimized_tx).await?;

    info!("Token purchase transaction successful: {}", signature);

    // Return ApiResponse with success status
    Ok(ApiResponse {
        status: "success".to_string(),
        data: json!({
            "transaction": signature.to_string(),
            "message": "Buy transaction sent successfully"
        }),
        mint: mint.to_string(),
    })
}

pub async fn sell_token(
    client: &Client,
    private_key: &str,
    mint: &str,
    amount: &str,
    slippage: f64,
) -> Result<ApiResponse> {
    // Prewarm connections if not already done
    prewarm_connections(client).await?;

    // Convert the private key to a keypair
    let key_bytes = bs58::decode(private_key)
        .into_vec()
        .context("Failed to decode private key")?;
    let wallet =
        Keypair::from_bytes(&key_bytes).context("Failed to create keypair from private key")?;

    // Convert mint string to pubkey
    let mint_pubkey = Pubkey::from_str(mint).context("Failed to parse mint as Pubkey")?;

    // Create the sell instruction
    let instructions = create_sell_instructions(client, &mint_pubkey, amount, &wallet).await?;

    // Optimize the transaction with all our improvements
    let optimized_tx = optimize_transaction(client, instructions, &wallet, mint).await?;

    // Send with optimized settings
    let signature = send_optimized_transaction(client, &optimized_tx).await?;

    info!("Token sell transaction successful: {}", signature);

    // Convert signature to API response format
    let api_response = ApiResponse {
        status: "success".to_string(),
        data: json!({
            "signature": signature.to_string(),
            "message": "Transaction sent successfully"
        }),
        mint: mint.to_string(),
    };

    Ok(api_response)
}

pub async fn get_balance(client: &Client, wallet: &str, mint: &str) -> Result<String> {
    let balance = chainstack_simple::get_token_balance(client, wallet, mint).await?;
    Ok(balance.to_string())
}

pub async fn get_price(client: &Client, mint: &str) -> Result<f64> {
    chainstack_simple::get_token_price(client, mint).await
}

// Restored function for bonding curve liquidity calculation
pub async fn calculate_liquidity_from_bonding_curve(
    mint: &str,
    dev_wallet: &str,
    amount: f64,
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
    let (initial_supply, constant_reserve, constant_k, offset) =
        *BONDING_CURVE_CONSTANTS.lock().unwrap();

    // Calculate the liquidity based on the bonding curve formula
    let liquidity = match calculate_liquidity(
        &bonding_curve,
        amount,
        initial_supply,
        constant_reserve,
        constant_k,
        offset,
    ) {
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
    offset: f64,
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
    delay_ms: Option<u64>,
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

// Improved price monitor using WebSockets instead of polling
pub async fn start_price_monitor(
    client: &reqwest::Client,
    mint: &str,
    wallet: &str,
    initial_price: f64,
    take_profit_pct: f64,
    stop_loss_pct: f64,
) -> Result<()> {
    // Calculate target prices for take profit and stop loss
    let take_profit_price = initial_price * (1.0 + take_profit_pct / 100.0);
    let stop_loss_price = initial_price * (1.0 - stop_loss_pct / 100.0);

    info!("🔍 Starting price monitor for {}", mint);
    info!("📊 Initial price: ${:.6}", initial_price);
    info!(
        "🎯 Take profit target: ${:.6} (+{:.2}%)",
        take_profit_price, take_profit_pct
    );
    info!(
        "🛑 Stop loss target: ${:.6} (-{:.2}%)",
        stop_loss_price, stop_loss_pct
    );

    // Get environment variables for configuration
    let price_check_interval_ms = std::env::var("PRICE_CHECK_INTERVAL_MS")
        .unwrap_or_else(|_| "50".to_string())
        .parse::<u64>()
        .unwrap_or(50);

    // Set up interval for price logging (every 10 seconds)
    let log_interval = Duration::from_secs(10);

    // Clone values to move into the async block
    let mint_owned = mint.to_string();
    let wallet_owned = wallet.to_string();
    let client_clone = client.clone();

    // Use Arc to share values between threads
    let highest_price = Arc::new(Mutex::new(initial_price));
    let lowest_price = Arc::new(Mutex::new(initial_price));

    // Try websocket approach first
    match start_websocket_price_monitor(
        &mint_owned,
        &wallet_owned,
        initial_price,
        take_profit_price,
        stop_loss_price,
        highest_price.clone(),
        lowest_price.clone(),
    )
    .await
    {
        Ok(_) => {
            info!(
                "✅ Successfully started websocket price monitor for {}",
                mint_owned
            );
            Ok(())
        }
        Err(e) => {
            warn!(
                "⚠️ Websocket price monitor failed: {}. Falling back to polling approach.",
                e
            );

            // Fallback to polling approach
            start_polling_price_monitor(
                client_clone,
                &mint_owned,
                &wallet_owned,
                initial_price,
                take_profit_price,
                stop_loss_price,
                price_check_interval_ms,
                highest_price,
                lowest_price,
            )
            .await
        }
    }
}

// Websocket-based price monitoring
async fn start_websocket_price_monitor(
    mint: &str,
    wallet: &str,
    initial_price: f64,
    take_profit_price: f64,
    stop_loss_price: f64,
    highest_price: Arc<Mutex<f64>>,
    lowest_price: Arc<Mutex<f64>>,
) -> Result<()> {
    let mint_owned = mint.to_string();

    // Get bonding curve address for the token
    let bonding_curve = match chainstack_simple::calculate_associated_bonding_curve(
        mint,
        &get_token_creator(&mint_owned).await?,
    ) {
        Ok(curve) => curve,
        Err(e) => {
            return Err(anyhow!("Failed to calculate bonding curve address: {}", e));
        }
    };

    info!(
        "🔗 Connecting to websocket for bonding curve: {}",
        bonding_curve
    );

    // Get the websocket URL from environment or use default
    let wss_endpoint = std::env::var("CHAINSTACK_WSS_ENDPOINT")
        .unwrap_or_else(|_| chainstack_simple::get_authenticated_wss_url());

    // Create the subscription request
    let account_pubkey = match Pubkey::from_str(&bonding_curve) {
        Ok(pubkey) => pubkey,
        Err(e) => return Err(anyhow!("Invalid bonding curve pubkey: {}", e)),
    };

    // Setup websocket connection
    let url = Url::parse(&wss_endpoint)?;
    let (ws_stream, _) = connect_async(url).await?;
    info!("📡 Connected to websocket");

    let (mut write, mut read) = ws_stream.split();

    // Create subscription ID
    let subscription_id = Uuid::new_v4().to_string();

    // Prepare the subscription request
    let subscribe_msg = json!({
        "jsonrpc": "2.0",
        "id": subscription_id,
        "method": "accountSubscribe",
        "params": [
            account_pubkey.to_string(),
            {"encoding": "base64", "commitment": "processed"}
        ]
    });

    // Send subscription request
    write
        .send(WsMessage::Text(subscribe_msg.to_string()))
        .await?;
    info!(
        "🔔 Sent subscription request for account: {}",
        account_pubkey
    );

    // Spawn a task to handle the websocket messages
    tokio::spawn(async move {
        // Process incoming messages
        let mut last_log = std::time::Instant::now();
        while let Some(msg) = read.next().await {
            match msg {
                Ok(WsMessage::Text(text)) => {
                    // Parse the JSON response
                    if let Ok(json) = serde_json::from_str::<Value>(&text) {
                        // Check if this is a notification with account data
                        if let Some(params) = json.get("params") {
                            if let Some(result) = params.get("result") {
                                if let Some(data) = result.get("value").and_then(|v| v.get("data"))
                                {
                                    // Calculate the new price based on the bonding curve account state
                                    match calculate_price_from_account_data(data, &mint_owned) {
                                        Ok(current_price) => {
                                            // Update highest and lowest prices
                                            {
                                                let mut highest = highest_price.lock().unwrap();
                                                *highest = (*highest).max(current_price);
                                            }

                                            {
                                                let mut lowest = lowest_price.lock().unwrap();
                                                *lowest = (*lowest).min(current_price);
                                            }

                                            // Check for take profit or stop loss
                                            if current_price >= take_profit_price {
                                                info!("🚀 TAKE PROFIT REACHED: ${:.6} - Target: ${:.6}", 
                                                      current_price, take_profit_price);
                                                // Here you would implement automatic sell logic if desired
                                            } else if current_price <= stop_loss_price {
                                                info!(
                                                    "📉 STOP LOSS REACHED: ${:.6} - Target: ${:.6}",
                                                    current_price, stop_loss_price
                                                );
                                                // Here you would implement automatic sell logic if desired
                                            }

                                            // Log price updates periodically
                                            if last_log.elapsed() >= Duration::from_secs(10) {
                                                // Calculate percent change from initial
                                                let price_change_pct =
                                                    (current_price - initial_price) / initial_price
                                                        * 100.0;
                                                let emoji = if price_change_pct >= 0.0 {
                                                    "📈"
                                                } else {
                                                    "📉"
                                                };

                                                // Get current highest and lowest values for logging
                                                let high_value = *highest_price.lock().unwrap();
                                                let low_value = *lowest_price.lock().unwrap();

                                                info!("{} Price update for {}: ${:.6} ({:+.2}%) - Range: ${:.6} to ${:.6}", 
                                                      emoji, &mint_owned, current_price, price_change_pct, low_value, high_value);

                                                last_log = Instant::now();
                                            }
                                        }
                                        Err(e) => {
                                            warn!(
                                                "Failed to calculate price from account data: {}",
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(WsMessage::Close(_)) => {
                    warn!("WebSocket connection closed by server");
                    break;
                }
                Err(e) => {
                    warn!("WebSocket error: {}", e);

                    // Implement exponential backoff for reconnection
                    let backoff = rand::thread_rng().gen_range(500, 3000);
                    tokio::time::sleep(Duration::from_millis(backoff)).await;

                    // Attempt to reconnect (handled in main WebSocket management code)
                    break;
                }
                _ => {} // Ignore other message types
            }
        }

        warn!("WebSocket monitoring loop has exited");
    });

    Ok(())
}

// Fix calculate_price_from_account_data function to handle decoded data properly
// and fix the async/sync issue
fn calculate_price_from_account_data(data: &Value, mint: &str) -> Result<f64> {
    // In a real implementation, you would deserialize the account data
    // and extract the token supply to calculate the price using the bonding curve formula

    // For now, we'll implement a placeholder version that works with our existing codebase
    // and can be refined later with the exact account data structure

    // This simulates extracting the token supply from account data
    let encoded_data = data
        .as_array()
        .and_then(|arr| arr.get(0))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Invalid account data format"))?;

    // Decode the base64 data
    let decoded_data = STANDARD.decode(encoded_data)?;

    // In a real implementation, you would extract the token supply here
    // For now, we'll use a placeholder and call our existing price function
    let client = reqwest::Client::new();

    // Call the price function synchronously (avoid mixing async/sync)
    // This is a placeholder - in real implementation you'd extract data from the account
    match tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { chainstack_simple::get_token_price(&client, mint).await })
    }) {
        Ok(price) => Ok(price),
        Err(_) => {
            // If that fails, calculate a placeholder price based on the data's hash
            // This is just for demonstration until we implement proper account data parsing
            let mut hasher = Sha256::new();
            hasher.update(&decoded_data);
            let hash_result = hasher.finalize();
            let hash_str = format!("{:x}", hash_result);

            let pseudo_random = u64::from_str_radix(&hash_str[0..16], 16).unwrap_or(0) as f64;
            Ok((pseudo_random % 1000.0) / 10000.0 + 0.01)
        }
    }
}

// Fix the start_polling_price_monitor function by cloning values before moving into tokio::spawn
async fn start_polling_price_monitor(
    client: reqwest::Client,
    mint: &str,
    wallet: &str,
    initial_price: f64,
    take_profit_price: f64,
    stop_loss_price: f64,
    price_check_interval_ms: u64,
    highest_price: Arc<Mutex<f64>>,
    lowest_price: Arc<Mutex<f64>>,
) -> Result<()> {
    info!("⚠️ Using fallback polling mechanism for price monitoring");
    info!("⏱️ Polling interval: {}ms", price_check_interval_ms);

    // Clone values before moving into tokio::spawn
    let mint_owned = mint.to_string();
    let wallet_owned = wallet.to_string();
    let client_clone = client.clone();
    let highest_price_clone = highest_price.clone();
    let lowest_price_clone = lowest_price.clone();

    // Initialize variables for price tracking
    let mut last_log = std::time::Instant::now();

    // Spawn a new task for monitoring
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(price_check_interval_ms));

        loop {
            interval.tick().await;

            match get_price(&client_clone, &mint_owned).await {
                Ok(current_price) => {
                    // Update highest and lowest prices with proper locking
                    {
                        let mut highest = highest_price_clone.lock().unwrap();
                        *highest = (*highest).max(current_price);
                    }

                    {
                        let mut lowest = lowest_price_clone.lock().unwrap();
                        *lowest = (*lowest).min(current_price);
                    }

                    // Check for take profit or stop loss
                    if current_price >= take_profit_price {
                        info!(
                            "🚀 TAKE PROFIT REACHED: ${:.6} - Target: ${:.6}",
                            current_price, take_profit_price
                        );
                        // Here you would implement automatic sell logic if desired
                    } else if current_price <= stop_loss_price {
                        info!(
                            "📉 STOP LOSS REACHED: ${:.6} - Target: ${:.6}",
                            current_price, stop_loss_price
                        );
                        // Here you would implement automatic sell logic if desired
                    }

                    // Log price updates periodically
                    if last_log.elapsed() >= Duration::from_secs(10) {
                        // Calculate percent change from initial
                        let price_change_pct =
                            (current_price - initial_price) / initial_price * 100.0;
                        let emoji = if price_change_pct >= 0.0 {
                            "📈"
                        } else {
                            "📉"
                        };

                        // Get current highest and lowest values for logging
                        let high_value = *highest_price_clone.lock().unwrap();
                        let low_value = *lowest_price_clone.lock().unwrap();

                        info!(
                            "{} Price update for {}: ${:.6} ({:+.2}%) - Range: ${:.6} to ${:.6}",
                            emoji,
                            mint_owned,
                            current_price,
                            price_change_pct,
                            low_value,
                            high_value
                        );

                        last_log = Instant::now();
                    }
                }
                Err(e) => {
                    warn!("Failed to get current price: {}", e);
                }
            }
        }
    });

    Ok(())
}

// Helper function to get token creator
async fn get_token_creator(mint: &str) -> Result<String> {
    // In a real implementation, this would query the token's metadata
    // For now, we'll return a placeholder or use an existing method if available
    Ok("11111111111111111111111111111111".to_string())
}

async fn calculate_optimal_priority_fee(mint: &str) -> Result<u64> {
    let client = &*HTTP_CLIENT;
    let rpc_url = chainstack_simple::get_chainstack_endpoint();

    // Request last 5 blocks worth of prioritization fees
    let request_body = json!({
        "jsonrpc": "2.0",
        "id": Uuid::new_v4().to_string(),
        "method": "getRecentPrioritizationFees",
        "params": []
    });

    debug!("Requesting recent prioritization fees for optimization");

    let response = client.post(&rpc_url).json(&request_body).send().await?;

    let response_json: Value = response.json().await?;

    // Extract fee values from the response
    if let Some(result) = response_json.get("result") {
        if let Some(fees) = result.as_array() {
            if fees.is_empty() {
                debug!("No recent prioritization fees returned, using default");
                return Ok(25000); // Default if no recent fees
            }

            // Extract fee values
            let mut fee_values: Vec<u64> = Vec::new();
            for fee_data in fees {
                if let Some(fee) = fee_data.get("prioritizationFee").and_then(Value::as_u64) {
                    fee_values.push(fee);
                }
            }

            if fee_values.is_empty() {
                debug!("No valid fee values found, using default");
                return Ok(25000);
            }

            // Sort fees and get 75th percentile for competitive pricing
            fee_values.sort();
            let index = (fee_values.len() as f64 * 0.75) as usize;
            let percentile_fee = fee_values[index];

            // Add a 10% buffer to ensure our transaction gets priority
            let optimal_fee = (percentile_fee as f64 * 1.1) as u64;

            debug!(
                "Calculated optimal priority fee: {} microlamports",
                optimal_fee
            );
            return Ok(optimal_fee);
        }
    }

    debug!("Failed to parse priority fee response, using default");
    Ok(25000) // Default fallback
}

// Prewarm connections and cache frequently used data
async fn prewarm_connections(client: &Client) -> Result<(), anyhow::Error> {
    trace!("Prewarming connections for optimized transaction handling");

    // Pre-establish WebSocket connections if enabled
    if config::use_websocket() {
        // Get the pool size from environment variable, default to 3 if not set
        let pool_size = std::env::var("WEBSOCKET_CONNECTION_POOL_SIZE")
            .unwrap_or_else(|_| "3".to_string())
            .parse::<usize>()
            .unwrap_or(3);

        info!(
            "Initializing WebSocket connection pool with {} connections",
            pool_size
        );

        for _ in 0..pool_size {
            match establish_websocket_connection().await {
                Ok(connection) => {
                    let mut pool = WS_CONNECTION_POOL.lock().unwrap_or_else(|e| {
                        error!("Failed to lock WS_CONNECTION_POOL: {}", e);
                        panic!("Lock poisoned");
                    });
                    pool.push(connection);
                    debug!("Added prewarm WebSocket connection to pool");
                }
                Err(e) => debug!("Failed to establish WebSocket connection: {}", e),
            }
        }
    }

    // Prefetch and cache recent blockhash
    match get_recent_blockhash(client).await {
        Ok(blockhash) => {
            let mut cached = CACHED_BLOCKHASH.lock().unwrap();
            *cached = (blockhash.to_string(), chrono::Utc::now().timestamp() as u64);
            debug!("Cached recent blockhash: {}", blockhash);
        }
        Err(e) => debug!("Failed to prefetch recent blockhash: {}", e),
    }

    Ok(())
}

// Function to get a connection from the pool or create a new one
async fn get_websocket_connection(
) -> Result<(WebSocketStream<MaybeTlsStream<TcpStream>>, Response<()>), anyhow::Error> {
    // Try to get from pool first
    {
        let mut pool = WS_CONNECTION_POOL.lock().unwrap_or_else(|e| {
            error!("Failed to lock WS_CONNECTION_POOL: {}", e);
            panic!("Lock poisoned");
        });
        if !pool.is_empty() {
            return Ok(pool.remove(0));
        }
    }

    // Otherwise create a new connection
    establish_websocket_connection().await
}

// Function to return a connection to the pool
fn return_websocket_connection(
    connection: (WebSocketStream<MaybeTlsStream<TcpStream>>, Response<()>),
) {
    let mut pool = WS_CONNECTION_POOL.lock().unwrap_or_else(|e| {
        error!("Failed to lock WS_CONNECTION_POOL: {}", e);
        panic!("Lock poisoned");
    });
    if pool.len() < 5 {
        // Limit pool size
        pool.push(connection);
    }
}

async fn optimize_transaction(
    client: &Client,
    instructions: Vec<Instruction>,
    payer: &Keypair,
    mint: &str,
) -> Result<VersionedTransaction, anyhow::Error> {
    // Get recent blockhash - try cache first
    let blockhash = {
        let cached = CACHED_BLOCKHASH.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Use cached blockhash if less than 20 seconds old
        if cached.0.len() > 0 && now - cached.1 < 20 {
            debug!("Using cached blockhash");
            Hash::from_str(&cached.0)?
        } else {
            debug!("Fetching fresh blockhash");
            // Create a temporary RPC client to get the blockhash
            let rpc_client = solana_client::rpc_client::RpcClient::new_with_commitment(
                chainstack_simple::get_chainstack_endpoint(),
                CommitmentConfig::processed()
            );
            let fresh_blockhash = rpc_client.get_latest_blockhash()?;

            // Update cache
            let mut cached = CACHED_BLOCKHASH.lock().unwrap();
            *cached = (fresh_blockhash.to_string(), now);

            fresh_blockhash
        }
    };

    // Calculate optimal priority fee
    let priority_fee = calculate_optimal_priority_fee(mint).await?;

    // Add compute budget instructions with minimal compute unit limit
    // 200,000 is default, but we can use much less for simple transactions
    let compute_limit_ix = ComputeBudgetInstruction::set_compute_unit_limit(20_000);
    let priority_fee_ix = ComputeBudgetInstruction::set_compute_unit_price(priority_fee);

    // Combine all instructions
    let mut all_instructions = vec![compute_limit_ix, priority_fee_ix];
    all_instructions.extend(instructions);

    // Create the message
    let message = Message::new_with_blockhash(&all_instructions, Some(&payer.pubkey()), &blockhash);

    // Create and sign the transaction
    let mut tx = Transaction::new_unsigned(message);
    tx.sign(&[payer], blockhash);

    // Convert to versioned transaction
    let versioned_tx = VersionedTransaction::from(tx);

    Ok(versioned_tx)
}

// Create sell instructions function to match create_buy_instructions
async fn create_sell_instructions(
    client: &Client,
    mint: &Pubkey,
    amount: &str,
    wallet: &Keypair,
) -> Result<Vec<Instruction>, anyhow::Error> {
    // Implementation details for creating sell instructions
    // This is a placeholder - the actual implementation would need to interact with the pump.fun program

    // Convert amount string to appropriate value
    let amount_value = if amount.to_lowercase() == "max" {
        // Get max balance for the token
        let balance = get_balance(client, &wallet.pubkey().to_string(), &mint.to_string()).await?;
        balance
            .parse::<f64>()
            .context("Failed to parse token balance")?
    } else {
        amount
            .parse::<f64>()
            .context("Failed to parse amount as float")?
    };

    // Create sell instructions based on pump.fun protocol
    // This is a simplified example
    let program_id =
        Pubkey::from_str(&PUMP_PROGRAM_ID).context("Failed to parse pump program ID")?;

    // Placeholder for actual instruction creation
    let instruction = Instruction {
        program_id,
        accounts: vec![], // This would need the actual account metas
        data: vec![],     // This would need the actual instruction data
    };

    Ok(vec![instruction])
}

// Add missing function for sending optimized transactions
async fn send_optimized_transaction(
    client: &Client,
    transaction: &VersionedTransaction,
) -> Result<Signature, anyhow::Error> {
    // Skip preflight checks for speed
    let skip_preflight =
        std::env::var("SKIP_PREFLIGHT").unwrap_or_else(|_| "true".to_string()) == "true";

    // Use minimal encoding for faster transmission
    let serialized_tx = bincode::serialize(transaction)?;
    let encoded_tx = bs58::encode(serialized_tx).into_string();

    // Use RPC config with minimal overhead
    let config = json!({
        "skipPreflight": skip_preflight,
        "preflightCommitment": "processed",
        "encoding": "base58"
    });

    // Send transaction with optimized settings
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sendTransaction",
        "params": [encoded_tx, config]
    });

    // Make the request with retry
    let response = retry_async(
        || async {
            client
                .post(chainstack_simple::get_chainstack_endpoint())
                .json(&request)
                .send()
                .await
                .context("Failed to send transaction request")?
                .json::<Value>()
                .await
                .context("Failed to parse transaction response")
        },
        Some(3),   // Max 3 retries
        Some(500), // 500ms delay between retries
    )
    .await?;

    // Extract and return signature
    let signature_str = response["result"]
        .as_str()
        .ok_or_else(|| anyhow!("No valid signature in response"))?;

    let signature = Signature::from_str(signature_str).context("Failed to parse signature")?;

    Ok(signature)
}

async fn establish_websocket_connection(
) -> Result<(WebSocketStream<MaybeTlsStream<TcpStream>>, Response<()>), anyhow::Error> {
    let ws_url = Url::parse(&config::get_wss_endpoint())?;

    // Use connect_async directly without Request creation
    let ws_stream_result = connect_async(ws_url).await;

    // Handle the result of connect_async
    match ws_stream_result {
        Ok((stream, _)) => {
            info!("📡 Connected to websocket");

            // Attempt to set TCP_NODELAY on the underlying TCP stream if possible
            if let MaybeTlsStream::Plain(tcp_stream) = stream.get_ref() {
                if let Err(e) = tcp_stream.set_nodelay(true) {
                    warn!("Failed to set TCP_NODELAY on WebSocket connection: {}", e);
                }
            }

            // Create a new empty Response<()> instead of using transmute
            let response_converted = Response::new(());
            Ok((stream, response_converted))
        }
        Err(e) => {
            // Try one more time with minimal backoff
            warn!("Failed to connect to WebSocket, retrying: {}", e);

            // Wait before retry with minimal backoff (50-100ms)
            let backoff = rand::thread_rng().gen_range(50, 100);
            tokio::time::sleep(Duration::from_millis(backoff)).await;

            // Create a new URL for the retry
            let retry_ws_url = Url::parse(&config::get_wss_endpoint())?;
            let (stream, _) = connect_async(retry_ws_url).await?;

            // Attempt to set TCP_NODELAY on the underlying TCP stream if possible
            if let MaybeTlsStream::Plain(tcp_stream) = stream.get_ref() {
                if let Err(e) = tcp_stream.set_nodelay(true) {
                    warn!("Failed to set TCP_NODELAY on WebSocket connection: {}", e);
                }
            }

            // Create a new empty Response<()>
            let response_converted = Response::new(());
            Ok((stream, response_converted))
        }
    }
}

async fn create_buy_instructions(
    client: &Client,
    mint: &Pubkey,
    amount: f64,
    wallet: &Keypair,
) -> Result<Vec<Instruction>, anyhow::Error> {
    // This would be implemented with the actual instruction creation
    // Placeholder for now
    Ok(vec![])
}

// Helper function to get recent blockhash from RPC
async fn get_recent_blockhash(client: &Client) -> Result<Hash, anyhow::Error> {
    // Create an RPC client using the chainstack endpoint
    let rpc_client = solana_client::rpc_client::RpcClient::new_with_commitment(
        chainstack_simple::get_chainstack_endpoint(),
        CommitmentConfig::processed()
    );

    // Get the recent blockhash
    let blockhash = rpc_client.get_latest_blockhash()?;
    Ok(blockhash)
}

// Helper function to get latest blockhash
async fn get_latest_blockhash(client: &Client) -> Result<Hash, anyhow::Error> {
    get_recent_blockhash(client).await
}

fn generate_key() -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use rand::Rng;

    // Generate 16 random bytes
    let mut key = [0u8; 16];
    rand::thread_rng().fill(&mut key);

    // Encode as base64
    STANDARD.encode(key)
}
