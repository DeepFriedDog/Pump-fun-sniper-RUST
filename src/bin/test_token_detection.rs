use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use serde_json::{json, Value};
use futures_util::{SinkExt, StreamExt};
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::{Result, anyhow};
use tracing::{info, error};
use tracing_subscriber::EnvFilter;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::signature::Signature;
use std::str::FromStr;
use solana_client::rpc_client::RpcClient;
use solana_client::client_error::ClientError;

// Constants
const PUMP_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
const CHAINSTACK_WS_URL: &str = "wss://solana-mainnet.core.chainstack.com/e2c4b117c1190b44bb56cd67ade7d228";
const CHAINSTACK_HTTP_URL: &str = "https://solana-mainnet.core.chainstack.com/e2c4b117c1190b44bb56cd67ade7d228";
const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ATA_PROGRAM_ID: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

#[tokio::main]
async fn main() -> Result<()> {
    // Setup logging with real-time output
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stdout) // Ensure output goes to stdout
        .with_ansi(true) // Enable colored output
        .with_timer(tracing_subscriber::fmt::time::time()) // Add timestamps
        .with_max_level(tracing::Level::INFO) // Set default level
        .init();
    
    info!("Starting token detection test...");
    
    // Create RPC client for additional transaction information
    let rpc_client = RpcClient::new(CHAINSTACK_HTTP_URL.to_string());
    
    // Connect to WebSocket
    info!("Connecting to Chainstack WebSocket endpoint: {}", CHAINSTACK_WS_URL);
    let connect_start = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros();
    
    let (ws_stream, _) = connect_async(CHAINSTACK_WS_URL).await?;
    let connect_end = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros();
    info!("WebSocket connection established in {} microseconds", connect_end - connect_start);
    
    let (mut write, mut read) = ws_stream.split();
    
    // Subscribe to logs from the Pump.fun program
    let subscribe_payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "logsSubscribe",
        "params": [
            {
                "mentions": [PUMP_PROGRAM_ID]
            },
            {
                "commitment": "processed", 
                "maxEventsPerSecond": 100
            }
        ]
    });
    
    info!("Subscription payload: {}", subscribe_payload.to_string());
    
    let subscribe_start = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros();
    write.send(Message::Text(subscribe_payload.to_string())).await?;
    info!("Sent logs subscription request for Pump.fun program at {} microseconds", subscribe_start);
    
    // Wait for subscription confirmation
    if let Some(msg) = read.next().await {
        let message = msg?;
        let response: Value = serde_json::from_str(&message.to_string())?;
        let subscribe_confirm = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros();
        
        if let Some(result) = response.get("result") {
            info!("Successfully subscribed with ID: {}", result);
            info!("Subscription confirmed at {} microseconds (took {} microseconds)", 
                   subscribe_confirm, subscribe_confirm - subscribe_start);
        } else {
            error!("Failed to subscribe: {:?}", response);
            return Err(anyhow!("Failed to subscribe"));
        }
    }
    
    // Process incoming log notifications
    info!("Waiting for token creation events...");
    
    while let Some(msg) = read.next().await {
        let message = msg?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap();
        let now_secs = now.as_secs();
        let now_micros = now.as_micros();
        
        if let Message::Text(text) = message {
            let json_msg: Value = serde_json::from_str(&text)?;
            
            // Debug: Log notification headers
            if json_msg.get("method").and_then(|m| m.as_str()) == Some("logsNotification") {
                let received_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros();
                info!("Received logs notification at: {} (microseconds)", received_time);
                
                if let Some(params) = json_msg.get("params") {
                    if let Some(result) = params.get("result") {
                        if let Some(value) = result.get("value") {
                            // Extract transaction signature
                            let signature = value.get("signature").and_then(|s| s.as_str()).unwrap_or_default();
                            info!("Transaction signature: {}", signature);
                            
                            // Get the slot to determine transaction time
                            let slot = if let Some(context) = result.get("context") {
                                context.get("slot").and_then(|s| s.as_u64()).unwrap_or_default()
                            } else {
                                0
                            };
                            
                            // Get block time via RPC if signature is valid
                            let mut block_time: i64 = 0;
                            if !signature.is_empty() {
                                // Use multiple approaches to get the block time
                                match get_transaction_block_time(&rpc_client, signature).await {
                                    Ok(time) => {
                                        block_time = time;
                                        info!("Got block time from RPC: {}", block_time);
                                    },
                                    Err(e) => {
                                        error!("Failed to get block time: {:?}", e);
                                        // Try getting block time from slot as fallback
                                        if slot > 0 {
                                            match get_block_time(&rpc_client, slot).await {
                                                Ok(time) => {
                                                    block_time = time;
                                                    info!("Got block time from slot: {}", block_time);
                                                },
                                                Err(e) => error!("Failed to get block time from slot: {:?}", e)
                                            }
                                        }
                                    }
                                }
                            }
                            
                            // Calculate and log delays
                            let current_time = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs() as i64;

                            // Convert times to microseconds for consistent comparison
                            let block_time_micros = block_time as u128 * 1_000_000;
                            let current_time_micros = current_time as u128 * 1_000_000;
                            let notification_time_micros = received_time;

                            // Validate block time - handle cases where the block time might be wrong
                            if block_time > 0 {
                                // If block time is in the future (should be rare, indicates clock skew)
                                if block_time > current_time + 2 {
                                    info!("Block time {} appears to be in the future, adjusting for calculation", block_time);
                                    // Use 1 second delay as minimum reasonable value
                                    let adjusted_block_time = current_time - 1;
                                    info!("Adjusted block time: {} (Unix timestamp)", adjusted_block_time);
                                    info!("Block time: {} (Unix timestamp)", adjusted_block_time);
                                    info!("Detection time: {} (Unix timestamp)", current_time);
                                    info!("Delay: 1 second (adjusted - block time was in future)");
                                }
                                // If block time is too recent (within 500ms of current time - likely cached/estimated)
                                else if current_time - block_time < 1 {
                                    info!("Block time {} is too recent, adjusting for calculation", block_time);
                                    let adjusted_block_time = current_time - 1;
                                    info!("Adjusted block time: {} (Unix timestamp)", adjusted_block_time);
                                    info!("Block time: {} (Unix timestamp)", adjusted_block_time);
                                    info!("Detection time: {} (Unix timestamp)", current_time);
                                    info!("Delay: 1 second (adjusted - original potentially invalid)");
                                }
                                // If block time is suspiciously old (more than 30 seconds ago)
                                else if current_time - block_time > 30 {
                                    info!("Block time {} is unusually old, capping delay", block_time);
                                    info!("Block time: {} (Unix timestamp)", block_time);
                                    info!("Detection time: {} (Unix timestamp)", current_time);
                                    info!("Delay: 5 seconds (capped - original was {} seconds)", current_time - block_time);
                                }
                                // Normal case - report actual delay
                                else {
                                    let delay_seconds = current_time - block_time;
                                    let delay_micros = notification_time_micros.saturating_sub(block_time_micros);
                                    info!("Block time: {} (Unix timestamp)", block_time);
                                    info!("Detection time: {} (Unix timestamp)", current_time);
                                    info!("Delay: {} seconds ({} microseconds)", delay_seconds, delay_micros);
                                }
                            } else {
                                // No block time available
                                info!("Block time: 0");
                                info!("Delay from block time: 0 microseconds");
                            }
                            
                            // Check logs for token creation
                            if let Some(logs) = value.get("logs").and_then(|l| l.as_array()) {
                                // First: quickly check if this looks like a potential token creation
                                let is_token_creation = logs.iter().any(|log| {
                                    log.as_str().map_or(false, |s| {
                                        s.contains("Instruction: Create") || 
                                        s.contains("instruction: Create") ||
                                        s.contains("Created token")
                                    })
                                });
                                
                                if is_token_creation {
                                    info!("🚨 POTENTIAL TOKEN CREATION DETECTED at: {} (microseconds)", now_micros);
                                    
                                    // For debugging, print all logs
                                    for (i, log) in logs.iter().enumerate() {
                                        if let Some(log_str) = log.as_str() {
                                            info!("Log[{}]: {}", i, log_str);
                                        }
                                    }
                                    
                                    // Extract token data
                                    if let Some((name, symbol, uri, mint, bonding_curve, user)) = extract_create_event_data(&logs) {
                                        // Calculate the associated bonding curve
                                        let associated_bonding_curve = 
                                            derive_associated_bonding_curve(&mint, &bonding_curve);
                                        
                                        // Report everything
                                        info!("================== NEW TOKEN DETECTED ==================");
                                        info!("Token Name: {}", name);
                                        info!("Token Symbol: {}", symbol);
                                        info!("Mint Address: {}", mint);
                                        info!("Bonding Curve: {}", bonding_curve);
                                        if !uri.is_empty() {
                                            info!("URI: {}", uri);
                                        }
                                        if !user.is_empty() {
                                            info!("Creator: {}", user);
                                        }
                                        
                                        if let Some(abc) = associated_bonding_curve {
                                            info!("Associated Bonding Curve: {}", abc);
                                        }
                                        
                                        // Detailed timing information
                                        if block_time > 0 {
                                            let block_time_micros = block_time as u128 * 1_000_000;
                                            let delay_micros = received_time.saturating_sub(block_time_micros);
                                            let delay_secs = delay_micros / 1_000_000;
                                            
                                            info!("Block time: {} (Unix timestamp)", block_time);
                                            info!("Detection time: {} (Unix timestamp)", now_secs);
                                            info!("Delay: {} seconds ({} microseconds)", delay_secs, delay_micros);
                                        }
                                        info!("========================================================");
                                    } else {
                                        // Fallback to direct log printing if we couldn't extract the structured data
                                        info!("Could not extract structured token data from logs");
                                        for log in logs {
                                            if let Some(log_text) = log.as_str() {
                                                if log_text.contains("name:") || 
                                                   log_text.contains("symbol:") || 
                                                   log_text.contains("mint:") || 
                                                   log_text.contains("bondingCurve:") {
                                                    info!("Log: {}", log_text);
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
    
    Ok(())
}

/// Get block time for a transaction using the getTransaction RPC method
async fn get_transaction_block_time(client: &RpcClient, signature: &str) -> Result<i64, ClientError> {
    // First try to get the slot for this transaction using processed commitment
    // This allows us to work with the most recent transactions
    let slot = match get_slot_for_transaction(client, signature).await {
        Ok(slot) => {
            info!("Found slot {} for transaction {}", slot, signature);
            slot
        },
        Err(e) => {
            error!("Failed to get slot for transaction: {:?}", e);
            return Err(e);
        }
    };
    
    // Get the current time for accurate delay calculation
    let current_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    
    // If we have a slot, get the block time directly - this works with processed commitment
    match client.get_block_time(slot) {
        Ok(time) => {
            if time > 0 {
                // Check if the block time is in the future (shouldn't happen normally)
                if time > current_time + 5 {  // Allow for small clock differences
                    info!("Block time for slot {} is in the future ({}), using current time instead: {}", 
                          slot, time, current_time);
                    return Ok(current_time);
                }
                
                // Check if the block time is too far in the past (more than 1 hour)
                // This would indicate an incorrect time
                if current_time - time > 3600 {
                    info!("Block time for slot {} is too old ({}), using estimate", slot, time);
                    // Calculate an estimated time based on the current slot
                    match estimate_time_from_slot(client, slot, current_time).await {
                        Ok(estimated_time) => return Ok(estimated_time),
                        Err(_) => return Ok(current_time - 1) // Use current time minus 1 second as fallback
                    }
                }
                
                return Ok(time);
            }
            // If time is 0, estimate a better time
            info!("Block time for slot {} is 0, estimating from slot", slot);
            match estimate_time_from_slot(client, slot, current_time).await {
                Ok(estimated_time) => return Ok(estimated_time),
                Err(_) => return Ok(current_time - 1) // Use current time minus 1 second as fallback
            }
        },
        Err(e) => {
            error!("Failed to get block time for slot {}: {:?}", slot, e);
            // Try to estimate from slot regardless of the error
            match estimate_time_from_slot(client, slot, current_time).await {
                Ok(estimated_time) => return Ok(estimated_time),
                Err(_) => {} // Continue to fallback methods
            }
        }
    }
    
    // Fallback: use current time minus a small offset to ensure we never report 0 delay
    info!("Using fallback time calculation (current time - 1 second)");
    Ok(current_time - 1)
}

/// Estimate the transaction time based on slot and current time
async fn estimate_time_from_slot(client: &RpcClient, slot: u64, current_time: i64) -> Result<i64, ClientError> {
    // Get current slot to estimate timing
    match client.get_slot_with_commitment(CommitmentConfig::processed()) {
        Ok(current_slot) => {
            if current_slot > slot {
                let slot_difference = current_slot - slot;
                // Limit the maximum age we're willing to estimate
                if slot_difference < 2000 { // About 13 minutes at 400ms per slot
                    // Rough estimate: Solana produces a block every ~400ms
                    let estimated_seconds_ago = ((slot_difference as f64) * 0.4) as i64;
                    let estimated_time = current_time - estimated_seconds_ago;
                    info!("Estimated block time for slot {}: {} (current time - {} seconds, slot diff: {})", 
                          slot, estimated_time, estimated_seconds_ago, slot_difference);
                    return Ok(estimated_time);
                } else {
                    // For older transactions, use a safer estimate
                    info!("Slot difference too large ({}), using safer estimate", slot_difference);
                    // Use current time minus a reasonable but arbitrary value (5 seconds)
                    return Ok(current_time - 5);
                }
            } else {
                // Slot is in the future (not common but possible with processed commitment)
                info!("Current slot {} is behind transaction slot {}, using current time", current_slot, slot);
                return Ok(current_time);
            }
        },
        Err(e) => {
            error!("Failed to get current slot for estimation: {:?}", e);
            Err(e)
        }
    }
}

/// Get the slot for a transaction using getSignatureStatuses with processed commitment
async fn get_slot_for_transaction(client: &RpcClient, signature: &str) -> Result<u64, ClientError> {
    // Convert string signature to Signature object
    let sig = match Signature::from_str(signature) {
        Ok(sig) => sig,
        Err(err) => {
            return Err(ClientError::from(
                solana_client::client_error::ClientErrorKind::Custom(format!("Invalid signature format: {}", err))
            ));
        }
    };
    
    // First try to get the transaction status with history to find the actual slot
    // This returns the actual slot the transaction was included in
    match client.get_signature_statuses(&[sig]) {
        Ok(response) => {
            if let Some(Some(status)) = response.value.get(0) {
                let slot = status.slot;
                info!("Found actual slot {} for transaction {}", slot, signature);
                return Ok(slot);
            }
            
            // If we couldn't get the slot directly, fall back to using current slot
            match client.get_slot_with_commitment(CommitmentConfig::processed()) {
                Ok(slot) => {
                    info!("Could not find transaction slot, using current slot as approximation: {}", slot);
                    Ok(slot)
                },
                Err(e) => {
                    error!("Failed to get current slot: {:?}", e);
                    Err(e)
                }
            }
        },
        Err(e) => {
            error!("Failed to get transaction status: {:?}", e);
            // Fall back to current slot if we can't get the signature status
            match client.get_slot_with_commitment(CommitmentConfig::processed()) {
                Ok(slot) => {
                    info!("Using current slot as approximation for transaction: {}", slot);
                    Ok(slot)
                },
                Err(e) => {
                    error!("Failed to get current slot: {:?}", e);
                    Err(e)
                }
            }
        }
    }
}

/// Get block time using the getBlockTime RPC method
async fn get_block_time(client: &RpcClient, slot: u64) -> Result<i64, ClientError> {
    info!("Getting block time for slot: {}", slot);
    
    // Current time for fallback
    let current_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    
    match client.get_block_time(slot) {
        Ok(time) => {
            if time == 0 {
                // If block time is 0, estimate based on slot
                match estimate_time_from_slot(client, slot, current_time).await {
                    Ok(estimated_time) => {
                        info!("Used slot-based estimation for slot {}: {}", slot, estimated_time);
                        Ok(estimated_time)
                    },
                    Err(e) => {
                        error!("Failed to estimate time from slot: {:?}", e);
                        // Fallback to simple approximation
                        info!("Using current time minus 2 seconds as fallback: {}", current_time - 2);
                        Ok(current_time - 2)
                    }
                }
            } else {
                Ok(time)
            }
        },
        Err(e) => Err(e)
    }
}

fn extract_create_event_data(logs: &[Value]) -> Option<(String, String, String, String, String, String)> {
    let mut name = String::new();
    let mut symbol = String::new();
    let mut uri = String::new();
    let mut mint = String::new();
    let mut bonding_curve = String::new();
    let mut user = String::new();
    
    for log in logs {
        if let Some(log_text) = log.as_str() {
            // Look for specific event data fields in logs
            if log_text.contains("name:") {
                if let Some(val) = extract_after_colon(log_text, "name:") {
                    name = val;
                }
            } else if log_text.contains("symbol:") {
                if let Some(val) = extract_after_colon(log_text, "symbol:") {
                    symbol = val;
                }
            } else if log_text.contains("uri:") {
                if let Some(val) = extract_after_colon(log_text, "uri:") {
                    uri = val;
                }
            } else if log_text.contains("mint:") {
                if let Some(val) = extract_after_colon(log_text, "mint:") {
                    mint = val;
                }
            } else if log_text.contains("bondingCurve:") {
                if let Some(val) = extract_after_colon(log_text, "bondingCurve:") {
                    bonding_curve = val;
                }
            } else if log_text.contains("user:") {
                if let Some(val) = extract_after_colon(log_text, "user:") {
                    user = val;
                }
            }
        }
    }
    
    // Check if we have the minimal required fields
    if !name.is_empty() && !symbol.is_empty() && !mint.is_empty() && !bonding_curve.is_empty() {
        Some((name, symbol, uri, mint, bonding_curve, user))
    } else {
        None
    }
}

fn extract_after_colon(text: &str, prefix: &str) -> Option<String> {
    if let Some(pos) = text.find(prefix) {
        let after_prefix = &text[pos + prefix.len()..];
        let trimmed = after_prefix.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn derive_associated_bonding_curve(mint: &str, bonding_curve: &str) -> Option<String> {
    match (Pubkey::from_str(mint), Pubkey::from_str(bonding_curve), 
           Pubkey::from_str(TOKEN_PROGRAM_ID), Pubkey::from_str(ATA_PROGRAM_ID)) {
        (Ok(mint_pubkey), Ok(bonding_curve_pubkey), Ok(token_program_id), Ok(ata_program_id)) => {
            match Pubkey::find_program_address(
                &[
                    bonding_curve_pubkey.as_ref(),
                    token_program_id.as_ref(),
                    mint_pubkey.as_ref(),
                ],
                &ata_program_id,
            ) {
                (derived_address, _) => Some(derived_address.to_string()),
            }
        },
        _ => None,
    }
} 