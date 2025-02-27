#![allow(unused)]

mod checks;
mod db;
mod tests;
mod api;
mod chainstack;
mod config;
mod token_detector;
mod websocket_test;

use anyhow::{Context, Result};
use chrono::{NaiveDateTime, TimeZone, Utc};
use dotenv::dotenv;
use log::{info, error, warn, LevelFilter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::collections::HashMap;

// Add lazy_static to Cargo.toml with: cargo add lazy_static
use lazy_static::lazy_static;

// Import the rustc_hash for faster HashMap implementation
use rustc_hash::FxHashMap;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use bs58;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use solana_program::pubkey::Pubkey;
use std::str::FromStr;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

#[derive(Debug, Serialize, Deserialize)]
struct TokenData {
    name: String,
    symbol: String,
    uri: String,
    mint: String, 
    bonding_curve: String,
    user: String,
}

/// Finds the associated bonding curve for a given mint and bonding curve.
fn find_associated_bonding_curve(mint: &Pubkey, bonding_curve: &Pubkey) -> Pubkey {
    let seeds = &[
        bonding_curve.as_ref(),
        config::TOKEN_PROGRAM_ID.as_ref(),
        mint.as_ref(),
    ];
    
    let (derived_address, _) = Pubkey::find_program_address(seeds, &config::ATA_PROGRAM_ID);
    derived_address
}

/// Parses the create instruction data
fn parse_create_instruction(data: &[u8]) -> Option<TokenData> {
    if data.len() < 8 {
        return None;
    }

    let mut offset = 8; // Skip discriminator

    let read_string = |data: &[u8], offset: &mut usize| -> Option<String> {
        if *offset + 4 > data.len() {
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
            return None;
        }
        
        let value = std::str::from_utf8(&data[*offset..*offset + length]).ok()?;
        *offset += length;
        
        Some(value.to_string())
    };

    let read_pubkey = |data: &[u8], offset: &mut usize| -> Option<String> {
        if *offset + 32 > data.len() {
            return None;
        }
        
        let pubkey_data = &data[*offset..*offset + 32];
        *offset += 32;
        
        Some(bs58::encode(pubkey_data).into_string())
    };

    // Parse fields
    let name = read_string(data, &mut offset)?;
    let symbol = read_string(data, &mut offset)?;
    let uri = read_string(data, &mut offset)?;
    let mint = read_pubkey(data, &mut offset)?;
    let bonding_curve = read_pubkey(data, &mut offset)?;
    let user = read_pubkey(data, &mut offset)?;

    Some(TokenData {
        name,
        symbol, 
        uri,
        mint,
        bonding_curve,
        user,
    })
}

fn process_message(text: &str) -> Result<(), Box<dyn std::error::Error>> {
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
                    // Look for program data
                    for log in logs {
                        if let Some(log_str) = log.as_str() {
                            if let Some(program_data_idx) = log_str.find("Program data: ") {
                                let encoded_data = &log_str[program_data_idx + 14..]; // "Program data: ".len() = 14
                                
                                // Try to decode the base64 data
                                match BASE64.decode(encoded_data) {
                                    Ok(decoded_data) => {
                                        if let Some(token_data) = parse_create_instruction(&decoded_data) {
                                            println!("Signature: {}", signature);
                                            println!("Name: {}", token_data.name);
                                            println!("Symbol: {}", token_data.symbol);
                                            println!("URI: {}", token_data.uri);
                                            println!("Mint: {}", token_data.mint);
                                            println!("Bonding Curve: {}", token_data.bonding_curve);
                                            println!("User: {}", token_data.user);
                                            
                                            // Calculate associated bonding curve
                                            if let (Ok(mint), Ok(bonding_curve)) = (
                                                Pubkey::from_str(&token_data.mint),
                                                Pubkey::from_str(&token_data.bonding_curve)
                                            ) {
                                                let associated_curve = find_associated_bonding_curve(&mint, &bonding_curve);
                                                println!("Associated Bonding Curve: {}", associated_curve);
                                                println!("##########################################################################################");
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        println!("Failed to decode: {}", log_str);
                                        println!("Error: {}", e);
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

#[allow(unused)]
lazy_static! {
    static ref HTTP_CLIENT: Client = {
        reqwest::ClientBuilder::new()
            .pool_max_idle_per_host(20)  // Increased from default
            .pool_idle_timeout(Duration::from_secs(60))
            .tcp_nodelay(true)  // Enable TCP_NODELAY for lower latency
            .tcp_keepalive(Some(Duration::from_secs(15)))  // Keep connections alive
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .http2_keep_alive_interval(Duration::from_secs(5))  // HTTP/2 keep-alive
            .http2_keep_alive_timeout(Duration::from_secs(20))  // HTTP/2 keep-alive timeout
            .http2_adaptive_window(true)  // Adaptive flow control for HTTP/2
            .build()
            .expect("Failed to build HTTP client")
    };

    #[allow(unused)]
    static ref LAST_PROCESSED_TOKENS: Arc<Mutex<FxHashMap<String, Instant>>> = Arc::new(Mutex::new(FxHashMap::default()));
}

// Initialize environment logger
fn setup_logger() -> Result<()> {
    // Configure logger with immediate flushing to solve buffering issues
    env_logger::Builder::new()
        .filter_level(LevelFilter::Info)
        .format_timestamp_millis()
        .write_style(env_logger::WriteStyle::Always)
        .init();
    
    Ok(())
}

// Create a client optimized for Chainstack WebSocket connections
fn create_optimized_client() -> Result<Client> {
    Ok(chainstack::create_chainstack_client())
}

/// Process the result of a buy operation
#[inline]
async fn process_buy_result(
    buy_response: api::ApiResponse,
    client: &reqwest::Client,
    wallet: &str,
    mint: &str,
    start_time: std::time::Instant,
) {
    if buy_response.status == "success" {
        let mut usd_price = 0.0;
        let mut tokens = "0".to_string();
        
        // Clone the values to avoid lifetime issues with tokio::spawn
        let client_clone1 = client.clone();
        let mint_clone1 = mint.to_string();
        
        let client_clone2 = client.clone();
        let wallet_clone = wallet.to_string();
        let mint_clone2 = mint.to_string();
        
        // Fetch price and balance in parallel after buying - with short timeout
        let price_future = tokio::spawn(async move {
            chainstack::get_token_price(&client_clone1, &mint_clone1).await
        });
        
        let balance_future = tokio::spawn(async move {
            chainstack::get_token_balance(&client_clone2, &wallet_clone, &mint_clone2).await
        });
        
        // Wait for both futures with a timeout to ensure we don't block for too long
        let (price_result, balance_result) = tokio::join!(
            tokio::time::timeout(Duration::from_millis(500), price_future),
            tokio::time::timeout(Duration::from_millis(500), balance_future)
        );
        
        // Process price result
        match price_result {
            Ok(Ok(Ok(price))) => {
                usd_price = price;
            },
            _ => {
                // Use fallback price if fetch failed
                usd_price = 0.0001; // Fallback value
            }
        }
        
        // Process balance result
        match balance_result {
            Ok(Ok(Ok(balance))) => {
                tokens = balance.to_string();
            },
            _ => {
                // Use fallback
                tokens = "0".to_string();
            }
        }
        
        // Calculate total elapsed time from detection to completion
        let total_elapsed = start_time.elapsed();
        info!("⚡ Buy completed in {:.3}s - Getting {} tokens at ${:.10}", 
             total_elapsed.as_secs_f64(), tokens, usd_price);
        
        // Record trade in database in background task
        let mint_owned = mint.to_string(); // Create a new clone directly from mint
        tokio::spawn(async move {
            if let Err(e) = db::insert_trade(&mint_owned, &tokens, usd_price) {
                error!("Failed to insert trade into DB: {}", e);
            }
        });
    }
}

/// Monitor token prices and sell based on conditions - more frequent updates
async fn monitor_tokens(
    client: Arc<reqwest::Client>,
    private_key: &str,
    slippage: f64,
    take_profit: f64,
    stop_loss: f64,
    timeout_minutes: f64,
) -> Result<()> {
    // Get the price check interval from environment or use default
    let price_check_interval = std::env::var("PRICE_CHECK_INTERVAL_MS")
        .unwrap_or_else(|_| "100".to_string())
        .parse::<u64>()
        .unwrap_or(100);
        
    info!("Token monitor started with price check frequency of {}ms...", price_check_interval);
    
    // Main monitoring loop - fixed interval timer
    let mut interval = tokio::time::interval(Duration::from_millis(price_check_interval));
    
    // Track last logged prices to avoid redundant logs
    let mut last_logged = std::time::Instant::now();
    let log_interval = Duration::from_secs(5); // Log price updates every 5 seconds
    
    // Cache for storing pending trades to reduce DB calls
    let mut trade_cache = Vec::new();
    let mut last_db_check = std::time::Instant::now();
    let db_check_interval = Duration::from_secs(1);
    
    loop {
        // Wait for the next tick
        interval.tick().await;
        
        // Get all pending trades (limit DB queries by caching)
        let pending_trades = if last_db_check.elapsed() >= db_check_interval || trade_cache.is_empty() {
            // Time to refresh the cache
            match db::get_pending_trades() {
                Ok(trades) => {
                    trade_cache = trades.clone();
                    last_db_check = std::time::Instant::now();
                    trades
                },
                Err(e) => {
                    error!("Error fetching pending trades: {}", e);
                    continue;
                }
            }
        } else {
            // Use cached trades
            trade_cache.clone()
        };

        // If we have no trades, continue to next tick
        if pending_trades.is_empty() {
            continue;
        }
        
        // Should we log this price update?
        let should_log = last_logged.elapsed() >= log_interval;
        if should_log {
            last_logged = std::time::Instant::now();
        }
        
        // Process each trade in parallel using tokio tasks
        let futures = pending_trades.into_iter().map(|trade| {
            let client_clone = client.clone();
            let private_key = private_key.to_string();
            let slippage = slippage;
            
            tokio::spawn(async move {
                if let Some(id) = trade.id {
                    // Calculate elapsed time
                    let mut elapsed_minutes = 0.0;
                    
                    // Parse the DB timestamp in UTC
                    let purchase_time_known = match trade.created_at.split_whitespace().collect::<Vec<&str>>() {
                        parts if parts.len() >= 2 => {
                            let date_str = parts[0];
                            let time_str = parts[1];
                            
                            if let Ok(naive_dt) = NaiveDateTime::parse_from_str(
                                &format!("{} {}", date_str, time_str), 
                                "%Y-%m-%d %H:%M:%S"
                            ) {
                                let current_time_utc = Utc::now();
                                let purchase_time_utc = Utc.from_utc_datetime(&naive_dt);
                                elapsed_minutes = (current_time_utc.timestamp() - purchase_time_utc.timestamp()) as f64 / 60.0;
                                true
                            } else {
                                false
                            }
                        },
                        _ => false
                    };
                    
                    // Get current price using Chainstack - with short timeout
                    let price_result = tokio::time::timeout(
                        Duration::from_millis(200),
                        chainstack::get_token_price(&client_clone, &trade.mint)
                    ).await;
                    
                    let usd_price = match price_result {
                        Ok(Ok(price)) => price,
                        _ => trade.current_price // Use last known price if fetch fails
                    };
                    
                    // Update price in DB only if it's changed significantly
                    if (usd_price - trade.current_price).abs() / trade.current_price > 0.005 { // 0.5% change
                        let _ = db::update_trade_price(id, usd_price);
                    }
                    
                    // Calculate price change
                    let price_change = ((usd_price - trade.buy_price) / trade.buy_price) * 100.0;
                    
                    // Only log price updates at the log interval
                    if should_log {
                        info!("Token: {}, Price: {:.10}, Change: {:.2}%, Elapsed: {:.2}min",
                            trade.mint, usd_price, price_change, elapsed_minutes);
                    }
                    
                    // Check if valid timeout
                    let is_valid_timeout = elapsed_minutes > 0.0 && elapsed_minutes < 24.0 * 60.0;
                    
                    // Check whether to sell based on conditions
                    if (take_profit > 0.0 && price_change >= take_profit) || // Take profit
                       (stop_loss > 0.0 && price_change <= -stop_loss) || // Stop loss
                       (timeout_minutes > 0.0 && elapsed_minutes >= timeout_minutes && is_valid_timeout) // Timeout
                    {
                        // Log which condition triggered the sell
                        let sell_reason = if take_profit > 0.0 && price_change >= take_profit {
                            info!("💰 TAKE PROFIT: {} - {:.2}% >= {:.2}%", 
                                 trade.mint, price_change, take_profit);
                            "take_profit"
                        } else if stop_loss > 0.0 && price_change <= -stop_loss {
                            info!("🛑 STOP LOSS: {} - {:.2}% <= -{:.2}%", 
                                 trade.mint, price_change, stop_loss);
                            "stop_loss"
                        } else {
                            info!("⏱️ TIMEOUT: {} - {:.2}min >= {:.2}min", 
                                 trade.mint, elapsed_minutes, timeout_minutes);
                            "timeout"
                        };
                        
                        // Execute sell using warp transaction for speed
                        let sell_start = std::time::Instant::now();
                        
                        if let Ok(sell_response) = chainstack::sell_token(
                            &*client_clone, 
                            &private_key, 
                            &trade.mint, 
                            &trade.tokens, 
                            slippage
                        ).await {
                            let sell_elapsed = sell_start.elapsed();
                            
                            if sell_response.status == "success" {
                                // Get final sell price
                                let final_price = match chainstack::get_token_price(&*client_clone, &trade.mint).await {
                                    Ok(price) => price,
                                    Err(_) => usd_price
                                };
                                
                                // Update the trade in DB
                                if let Err(e) = db::update_trade_sold(id, final_price) {
                                    error!("Failed to update trade as sold in DB: {}", e);
                                } else {
                                    info!("✅ SOLD TOKEN: {} - Reason: {}, Final price: {:.10}, Execution time: {:.3}s", 
                                         trade.mint, sell_reason, final_price, sell_elapsed.as_secs_f64());
                                }
                            } else {
                                warn!("❌ FAILED TO SELL: {} - Reason: {}", 
                                     trade.mint, sell_response.data.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown"));
                            }
                        }
                        
                        return Some(id); // Return the trade ID that was processed
                    }
                }
                None
            })
        }).collect::<Vec<_>>();
        
        // Wait for all price check tasks to complete
        for future in futures {
            if let Ok(Some(_)) = future.await {
                // A trade was sold, clear cache to fetch fresh data
                trade_cache.clear();
                break;
            }
        }
    }
}

/// Process new tokens from the WebSocket queue
#[inline]
async fn process_new_tokens(
    client: &Arc<reqwest::Client>,
    last_mint: &Option<String>,
    check_urls: bool,
    check_min_liquidity: bool,
    approved_devs_only: bool,
    snipe_by_tag: &str,
    private_key: &str,
    wallet: &str,
    amount: f64,
    slippage: f64,
) -> Result<Option<String>> {
    // Measure start time immediately for performance tracking
    let start_time = std::time::Instant::now();
    
    // Fetch new tokens from the WebSocket queue
    let token_data = match api::fetch_new_tokens(client).await {
        Ok(data) => data,
        Err(e) => {
            error!("Failed to fetch new tokens: {}", e);
            return Ok(None);
        }
    };
    
    // Check if the response was successful
    if token_data.status != "success" {
        return Ok(None);
    }
    
    let mint = token_data.mint.clone();
    
    // Check if the mint is the same as the last processed mint
    if let Some(last) = last_mint {
        if *last == mint {
            return Ok(None);
        }
    }
    
    // Skip the already processed check for very fresh tokens
    let now = Instant::now();
    let mut last_processed = LAST_PROCESSED_TOKENS.lock().unwrap();
    
    if let Some(last_time) = last_processed.get(&mint) {
        if last_time.elapsed() < Duration::from_secs(10) { // Reduced from 60 to 10 seconds for faster retries
            return Ok(None);
        }
    }
    
    // Update the last processed time for this mint
    last_processed.insert(mint.clone(), now);
    drop(last_processed); // Release the lock
    
    // IMPORTANT: We want to optimize for speed, so we'll make multiple requests in parallel
    
    // Log new token detection
    info!("🔍 New mint detected: {} - Processing...", mint);
    
    // Get minimum liquidity from environment
    let min_liquidity_str = std::env::var("MIN_LIQUIDITY").unwrap_or_else(|_| "0".to_string());
    let min_liquidity = min_liquidity_str.parse::<f64>().unwrap_or(0.0);
    
    // In parallel, check if the developer is in the approved list
    let dev_wallet = token_data.dev.clone();
    let is_approved_dev_future = if approved_devs_only {
        Some(tokio::spawn(async move {
            match checks::check_approved_devs(&dev_wallet).await {
                Ok(is_approved) => is_approved,
                Err(_) => false
            }
        }))
    } else {
        None
    };
    
    // Check if this is a pump.fun token based on mint suffix
    let is_pump_fun_token = mint.ends_with("pump");
    
    // Check token tags if needed
    let tag_check_passed = if !snipe_by_tag.trim().is_empty() {
        let token_name = token_data.name.clone().unwrap_or_default().to_lowercase();
        token_name.contains(&snipe_by_tag.to_lowercase())
    } else {
        true
    };
    
    // If tag check failed, return early
    if !tag_check_passed {
        return Ok(Some(mint.clone()));
    }
    
    // Await results from parallel operations
    // First, check if dev is approved - this is our fast path
    let is_approved_dev = if let Some(future) = is_approved_dev_future {
        match tokio::time::timeout(Duration::from_millis(200), future).await {
            Ok(Ok(result)) => result,
            _ => false
        }
    } else {
        false
    };
    
    // Fast path for approved developers - skip liquidity check
    if approved_devs_only && is_approved_dev {
        info!("🚀 APPROVED DEVELOPER DETECTED! Skipping liquidity check and buying immediately");
        
        // Buy the token immediately using Chainstack warp transaction
        let buy_start = std::time::Instant::now();
        let buy_result = chainstack::buy_token(
            &**client,
            private_key,
            &mint,
            amount,
            slippage
        ).await?;
        
        // Process buy result and log speed
        let buy_elapsed = buy_start.elapsed();
        let total_elapsed = start_time.elapsed();
        
        if buy_result.status == "success" {
            info!("✅ BOUGHT TOKEN: {} - Buy took: {:.3}s, Total processing: {:.3}s", 
                 mint, buy_elapsed.as_secs_f64(), total_elapsed.as_secs_f64());
            
            // Process the buy result for DB storage and price monitoring
            process_buy_result(buy_result, client, wallet, &mint, start_time).await;
        } else {
            info!("❌ FAILED TO BUY: {} - Elapsed: {:.3}s - Reason: {}", 
                 mint, total_elapsed.as_secs_f64(), buy_result.data.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown"));
        }
        
        return Ok(Some(mint.clone()));
    }
    
    // Now check liquidity if needed
    if check_min_liquidity {
        // For pump.fun tokens, calculate liquidity using the bonding curve account
        let liquidity_value = if is_pump_fun_token {
            // First, we need to find the bonding curve address
            // This might be included in the WebSocket message or we can derive it
            let bonding_curve_address = match token_data.metadata.as_deref().and_then(|m| {
                // Try to extract from metadata
                if m.contains("bonding_curve") {
                    let start = m.find("bonding_curve\":\"").map(|i| i + 16)?;
                    let end = m[start..].find("\"").map(|i| i + start)?;
                    Some(m[start..end].to_string())
                } else {
                    None
                }
            }) {
                Some(bc) => bc,
                None => {
                    // If not found in metadata, try to calculate it
                    info!("Bonding curve not found in metadata, attempting to derive it");
                    match api::calculate_associated_bonding_curve(&mint, &api::PUMP_PROGRAM_ID.clone()) {
                        Ok(bc) => bc,
                        Err(e) => {
                            warn!("Could not derive bonding curve address: {}", e);
                            "".to_string()
                        }
                    }
                }
            };
            
            if bonding_curve_address.is_empty() {
                warn!("Could not determine bonding curve address for {}", mint);
                0.0 // No liquidity if we can't determine bonding curve
            } else {
                // Calculate liquidity directly from the bonding curve account
                info!("🧮 Calculating liquidity directly from bonding curve: {}", bonding_curve_address);
                match api::calculate_liquidity_from_bonding_curve(&mint, &bonding_curve_address, min_liquidity).await {
                    Ok(liquidity) => {
                        info!("✅ Liquidity calculation successful: {} SOL", liquidity);
                        liquidity
                    },
                    Err(e) => {
                        warn!("❌ Failed to calculate liquidity from bonding curve: {}", e);
                        0.0 // No liquidity if calculation fails
                    }
                }
            }
        } else {
            // For non-pump.fun tokens, use chainstack's liquidity check
            match chainstack::check_liquidity(client, &token_data.dev, &mint).await {
                Ok((_, liquidity)) => liquidity,
                Err(e) => {
                    warn!("Failed to check token liquidity: {}", e);
                    0.0
                }
            }
        };
        
        // Check if liquidity is sufficient
        let liquidity_passes = liquidity_value >= min_liquidity;
        
        // Log liquidity check
        info!("🧮 Token: {} - Liquidity: {:.2} SOL, Required: {:.2} SOL, Passed: {}", 
              mint, liquidity_value, min_liquidity, liquidity_passes);
              
        // If liquidity is insufficient, skip this token
        if !liquidity_passes {
            return Ok(Some(mint.clone()));
        }
    }
    
    // If we got here, token passed all checks - time to buy!
    let buy_start = std::time::Instant::now();
    
    // Use warp transaction with Chainstack for ultra-fast execution
    let buy_result = chainstack::buy_token(
        &**client,
        private_key,
        &mint,
        amount,
        slippage
    ).await?;
    
    // Calculate performance metrics
    let buy_elapsed = buy_start.elapsed();
    let total_elapsed = start_time.elapsed();
    
    // Log result with timing information
    if buy_result.status == "success" {
        info!("✅ BOUGHT TOKEN: {} - Buy took: {:.3}s, Total processing: {:.3}s", 
             mint, buy_elapsed.as_secs_f64(), total_elapsed.as_secs_f64());
    } else {
        info!("❌ FAILED TO BUY: {} - Elapsed: {:.3}s - Reason: {}", 
             mint, total_elapsed.as_secs_f64(), buy_result.data.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown"));
    }
    
    // Process the buy result (update DB, fetch price, etc.)
    process_buy_result(buy_result, client, wallet, &mint, start_time).await;
    
    Ok(Some(mint.clone()))
}

// Add a function to periodically send a ping to the WebSocket server
async fn periodic_websocket_check() {
    info!("Starting periodic WebSocket connection check");
    
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
    
    loop {
        interval.tick().await;
        info!("⏱️ Periodic check: Verifying WebSocket activity...");
        
        // Access the global queue to check if we're receiving messages
        if let Ok(queue) = chainstack::WEBSOCKET_MESSAGES.try_lock() {
            info!("Current WebSocket queue size: {}", queue.len());
            
            // Log that we're actively monitoring
            info!("📡 WebSocket monitor is active and waiting for token creation events");
        } else {
            warn!("Could not access WebSocket queue for periodic check");
        }
    }
}

// Update the run_parallel_api_check function to remove the db parameter:
async fn run_parallel_api_check(client: Arc<reqwest::Client>) {
    info!("Starting parallel API token detection check...");
    
    let mut last_detected_mint = String::new();
    
    loop {
        // Fetch tokens from the API endpoint
        match api::fetch_tokens_from_api(&client).await {
            Ok(Some(token)) => {
                // Check if this is a new token (different from last detected)
                if token.mint != last_detected_mint {
                    info!("🔔 API DETECTED NEW TOKEN NOT SEEN IN WEBSOCKET: {}", token.mint);
                    
                    // Check if this token is already in our WebSocket queue
                    let in_websocket_queue = {
                        let queue = api::NEW_TOKEN_QUEUE.lock().unwrap();
                        queue.iter().any(|t| t.mint == token.mint)
                    };
                    
                    if !in_websocket_queue {
                        info!("⚠️ TOKEN {} DETECTED BY API BUT MISSING FROM WEBSOCKET", token.mint);
                        
                        // Option: Add to queue if not already there
                        // Uncomment the following block if you want to add API-detected tokens to the queue
                        /*
                        {
                            let mut queue = api::NEW_TOKEN_QUEUE.lock().unwrap();
                            queue.push_back(token.clone());
                            info!("Added API-detected token to queue: {}", token.mint);
                        }
                        */
                    }
                    
                    // Update last detected mint
                    last_detected_mint = token.mint.clone();
                }
            },
            Ok(None) => {
                // No new token or error occurred
            },
            Err(e) => {
                warn!("Error fetching tokens from API: {}", e);
            }
        }
        
        // Sleep to avoid hammering the API
        tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if it exists
    dotenv().ok();
    
    // Initialize logging with immediate flush
    setup_logger()?;
    
    // Check if we should run tests
    if std::env::args().any(|arg| arg == "--test" || arg == "-t") {
        info!("Running in test mode");
        tests::run_all_tests().await;
        return Ok(());
    }
    
    // Check if we should run the WebSocket test
    if std::env::args().any(|arg| arg == "--websocket-test" || arg == "-w") {
        info!("Running WebSocket test mode");
        let wss_endpoint = std::env::var("WSS_ENDPOINT")
            .unwrap_or_else(|_| chainstack::get_authenticated_wss_url());
        
        info!("Using WebSocket endpoint: {}", wss_endpoint);
        websocket_test::run_websocket_test(&wss_endpoint).await?;
        return Ok(());
    }
    
    // Read configuration
    let check_urls = std::env::var("CHECK_URLS").unwrap_or_else(|_| "false".to_string()) == "true";
    let snipe_by_tag = std::env::var("SNIPE_BY_TAG").unwrap_or_else(|_| "".to_string());
    let private_key = std::env::var("PRIVATE_KEY").context("Missing PRIVATE_KEY in .env")?;
    let amount = std::env::var("AMOUNT")
        .unwrap_or_else(|_| "0.1".to_string())
        .parse::<f64>()
        .context("Invalid AMOUNT value in .env")?;
    let slippage = std::env::var("SLIPPAGE")
        .unwrap_or_else(|_| "10".to_string())
        .parse::<f64>()
        .context("Invalid SLIPPAGE value in .env")?;
    let wallet = std::env::var("WALLET").context("Missing WALLET in .env")?;
    
    // Default to 1 position if not specified or invalid
    let max_positions = std::env::var("MAX_POSITIONS")
        .unwrap_or_else(|_| "1".to_string())
        .parse::<i64>()
        .unwrap_or(1);
    
    let check_min_liquidity = std::env::var("CHECK_MIN_LIQUIDITY")
        .unwrap_or_else(|_| "false".to_string()) == "true";
    let approved_devs_only = std::env::var("APPROVED_DEVS_ONLY")
        .unwrap_or_else(|_| "false".to_string()) == "true";
    let take_profit = std::env::var("TAKE_PROFIT")
        .unwrap_or_else(|_| "0".to_string())
        .parse::<f64>()
        .context("Invalid TAKE_PROFIT value in .env")?;
    let stop_loss = std::env::var("STOP_LOSS")
        .unwrap_or_else(|_| "0".to_string())
        .parse::<f64>()
        .context("Invalid STOP_LOSS value in .env")?;
    let timeout_minutes = std::env::var("TIMEOUT")
        .unwrap_or_else(|_| "0".to_string())
        .parse::<f64>()
        .context("Invalid TIMEOUT value in .env")?;
    
    // Get the polling rate from environment, default to 100ms (10 requests/second)
    let polling_rate_ms = std::env::var("POLLING_RATE_MS")
        .unwrap_or_else(|_| "100".to_string())
        .parse::<u64>()
        .unwrap_or(100);
        
    // Clean start option - reset any pending trades
    let clean_start = std::env::var("CLEAN_START")
        .unwrap_or_else(|_| "true".to_string()) == "true";
    
    // Initialize the database - reset pending trades if clean start is enabled
    db::init_db(clean_start)?;
    
    // Create optimized HTTP client with high concurrency
    let client = Arc::new(reqwest::ClientBuilder::new()
        .pool_max_idle_per_host(50)  // Increased for more concurrent connections
        .tcp_nodelay(true)  // Enable TCP_NODELAY for lower latency
        .tcp_keepalive(Some(Duration::from_secs(15)))
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(2))
        .http2_keep_alive_interval(Duration::from_secs(5))
        .http2_keep_alive_timeout(Duration::from_secs(20))
        .http2_adaptive_window(true)
        .build()?);
    
    // Initialize WebSocket connections for real-time token detection
    info!("Initializing WebSocket connections for ultra-fast token detection...");

    // Start the optimized WebSocket token detector
    let wss_endpoint = config::get_wss_endpoint();
    tokio::spawn(async move {
        if let Err(e) = token_detector::listen_for_new_tokens(wss_endpoint).await {
            error!("Error in token detector: {}", e);
        }
    });

    // Initialize Chainstack websocket connection for token monitoring
    match chainstack::setup_websocket().await {
        Ok(_) => info!("Chainstack WebSocket initialized for token monitoring"),
        Err(e) => error!("Failed to initialize Chainstack WebSocket: {}", e),
    }

    // Start the token price monitor with more frequent updates
    let client_clone = client.clone();
    let private_key_clone = private_key.clone();
    tokio::spawn(async move {
        if let Err(e) = monitor_tokens(
            client_clone,
            &private_key_clone,
            slippage,
            take_profit,
            stop_loss,
            timeout_minutes,
        ).await {
            error!("Error in token monitor: {}", e);
        }
    });
    
    // Variable to store the last processed mint
    let mut last_mint: Option<String> = None;
    
    // Flag to prevent overlapping executions
    let is_processing = Arc::new(AtomicBool::new(false));
    
    // Last time we logged position message
    let mut last_position_log = None::<std::time::Instant>;
    
    info!("🚀 PumpFun Sniper Bot started with Chainstack WebSockets and SolanaAPIs integration");
    info!("⚡ Polling new tokens every {}ms with high-frequency price updates", polling_rate_ms);
    
    // Set important environment variables if not already set
    if std::env::var("SOLANA_RPC_URL").is_err() {
        std::env::set_var("SOLANA_RPC_URL", "https://api.mainnet-beta.solana.com");
        info!("Set SOLANA_RPC_URL to default: https://api.mainnet-beta.solana.com");
    }
    
    // Start the periodic WebSocket check
    tokio::spawn(periodic_websocket_check());

    // Add our parallel API check for comparison
    let api_client = Arc::clone(&client);
    tokio::spawn(async move {
        run_parallel_api_check(api_client).await;
    });
    
    // Create signal handlers for graceful shutdown
    let (shutdown_sender, mut shutdown_receiver) = tokio::sync::mpsc::channel::<()>(1);
    
    // Set up Ctrl+C handler
    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                info!("Received Ctrl+C, initiating graceful shutdown...");
                let _ = shutdown_sender.send(()).await;
            },
            Err(e) => error!("Failed to listen for Ctrl+C: {}", e),
        }
    });

    // Main loop to keep the program running
    info!("Start monitoring for new tokens...");
    loop {
        tokio::select! {
            // Check for shutdown signal
            _ = shutdown_receiver.recv() => {
                info!("Shutdown signal received, exiting...");
                break;
            }
            
            // Periodically fetch and process tokens from WebSocket queue
            _ = tokio::time::sleep(Duration::from_millis(polling_rate_ms)) => {
                // Try to acquire the flag - ensure no overlapping executions
                if is_processing.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                    // Process tokens - acquire lock when ready
                    match process_new_tokens(
                        &client,
                        &last_mint,
                        check_urls,
                        check_min_liquidity,
                        approved_devs_only,
                        &snipe_by_tag,
                        &private_key,
                        &wallet,
                        amount,
                        slippage,
                    ).await {
                        Ok(Some(mint)) => {
                            // Update last mint
                            last_mint = Some(mint);
                        },
                        Ok(None) => {
                            // No token processed
                        },
                        Err(e) => {
                            error!("Error processing tokens: {}", e);
                        }
                    }
                    
                    // Release the flag
                    is_processing.store(false, Ordering::SeqCst);
                }
            }
        }
    }

    info!("Program shutting down...");
    Ok(())
}
