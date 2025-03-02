#![allow(unused)]

// Module declarations
mod api;
mod checks;
mod config;
mod db;
mod error;
mod tests;
mod token_detector;
mod websocket_test;
mod chainstack_simple;
mod websocket_reconnect;

// Standard library imports
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::io::Write;

// External crate imports
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use dotenv::dotenv;
use lazy_static::lazy_static;
use log::{error, info, warn, LevelFilter};
use reqwest::Client;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solana_sdk::pubkey::Pubkey;
use tokio::sync::mpsc;
use tokio::time;

// WebSocket-related imports
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use bs58;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

// Internal module imports
use crate::websocket_test::TokenData;

// Define TokenData structure for the main module
#[derive(Debug, Serialize, Deserialize)]
struct MainTokenData {
    name: String,
    symbol: String,
    uri: String,
    mint: String,
    bonding_curve: String,
    user: String,
}

// Global state for tracking processed tokens
lazy_static! {
    static ref LAST_PROCESSED_TOKENS: Mutex<HashMap<String, Instant>> = Mutex::new(HashMap::new());
}

/// Process newly detected tokens from the WebSocket
async fn process_new_tokens_from_websocket(
    client: &Arc<reqwest::Client>,
    token_data: &websocket_test::TokenData,
    check_min_liquidity: bool,
    approved_devs_only: bool,
    snipe_by_tag: &str,
    private_key: &str,
    wallet: &str,
    amount: f64,
    slippage: f64,
) -> Result<()> {
    // Measure start time for performance tracking
    let start_time = std::time::Instant::now();
    
    // Extract data from TokenData
    let mint = &token_data.mint;
    let name = &token_data.name;
    let symbol = &token_data.symbol;
    let user = &token_data.user;  // Creator/developer address
    
    // Skip the already processed check for fresh tokens
    let now = Instant::now();
    let mut last_processed = LAST_PROCESSED_TOKENS.lock().unwrap();
    
    if let Some(last_time) = last_processed.get(mint) {
        if last_time.elapsed() < Duration::from_secs(5) {
            // Token was processed very recently, skip
            return Ok(());
        }
    }
    
    // Update the last processed time for this mint
    last_processed.insert(mint.clone(), now);
    drop(last_processed); // Release the lock
    
    // Calculate or estimate liquidity
    let liquidity = match api::calculate_liquidity_from_bonding_curve(&mint, &user, amount).await {
        Ok(liq) => liq,
        Err(_) => 0.5, // Default value if calculation fails
    };
    
    // Determine opportunity status
    let opportunity_status = "💎"; // Diamond for confirmed token
    
    // Log new token detection with the new format
    info!("🪙 NEW TOKEN CREATED! {} (mint: {}) 💰 {:.2} SOL {}", 
        name, 
        mint, 
        liquidity, 
        opportunity_status);
    
    // Check if tag matches if a tag filter is specified
    if !snipe_by_tag.trim().is_empty() {
        let token_name = name.to_lowercase();
        if !token_name.contains(&snipe_by_tag.to_lowercase()) {
            info!("Token name doesn't match the tag filter '{}', skipping", snipe_by_tag);
            return Ok(());
        }
    }
    
    // Get minimum liquidity from environment
    let min_liquidity_str = std::env::var("MIN_LIQUIDITY").unwrap_or_else(|_| "0".to_string());
    let min_liquidity = min_liquidity_str.parse::<f64>().unwrap_or(0.0);
    
    // Check if the developer is in the approved list
    let is_approved_dev = if approved_devs_only {
        match checks::check_approved_devs(user).await {
            Ok(is_approved) => {
                if is_approved {
                    info!("🚀 APPROVED DEVELOPER! {} (creator: {})", name, user);
                }
                is_approved
            },
            Err(e) => {
                warn!("Failed to check if developer is approved: {}", e);
                false
            }
        }
    } else {
        false
    };
    
    // Fast path for approved developers - skip liquidity check
    if approved_devs_only && is_approved_dev {
        info!("Skipping liquidity check and buying immediately");
        
        // Buy the token immediately using Chainstack warp transaction
        let buy_start = std::time::Instant::now();
        let buy_result = api::buy_token(
            &**client,
            private_key,
            mint,
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
            process_buy_result(buy_result, client, wallet, mint, start_time).await;
        } else {
            info!("❌ FAILED TO BUY: {} - Elapsed: {:.3}s - Reason: {}", 
                 mint, total_elapsed.as_secs_f64(), buy_result.data.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown"));
        }
        
        return Ok(());
    }
    
    // If not an approved dev and we need to check liquidity
    if check_min_liquidity {
        // Get bonding curve from token data
        let bonding_curve = &token_data.bonding_curve;
        
        // Use token_detector to check liquidity
        match token_detector::check_token_liquidity(mint, bonding_curve, min_liquidity).await {
            Ok((has_liquidity, balance)) => {
                info!("Bonding curve liquidity: {} SOL, Required: {} SOL", balance, min_liquidity);
                
                if has_liquidity {
                    info!("✅ Liquidity check PASSED - Proceeding with buy");
                    
                    // Execute buy transaction
                    let buy_start = std::time::Instant::now();
                    let buy_result = api::buy_token(
                        &**client,
                        private_key,
                        mint,
                        amount,
                        slippage
                    ).await?;
                    
                    // Process buy result
                    let buy_elapsed = buy_start.elapsed();
                    let total_elapsed = start_time.elapsed();
                    
                    if buy_result.status == "success" {
                        info!("✅ BOUGHT TOKEN: {} - Buy took: {:.3}s, Total processing: {:.3}s", 
                             mint, buy_elapsed.as_secs_f64(), total_elapsed.as_secs_f64());
                        
                        // Process the buy result
                        process_buy_result(buy_result, client, wallet, mint, start_time).await;
                    } else {
                        info!("❌ FAILED TO BUY: {} - Elapsed: {:.3}s - Reason: {}", 
                             mint, total_elapsed.as_secs_f64(), buy_result.data.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown"));
                    }
                } else {
                    info!("❌ Liquidity check FAILED - Skipping buy");
                }
            },
            Err(e) => {
                warn!("Failed to check liquidity: {}", e);
            }
        }
    } else {
        // If we're not checking liquidity, buy directly
        info!("Liquidity check disabled - Proceeding with buy");
        
        // Buy the token immediately
        let buy_start = std::time::Instant::now();
        let buy_result = api::buy_token(
            &**client,
            private_key,
            mint,
            amount,
            slippage
        ).await?;
        
        // Process buy result
        let buy_elapsed = buy_start.elapsed();
        let total_elapsed = start_time.elapsed();
        
        if buy_result.status == "success" {
            info!("✅ BOUGHT TOKEN: {} - Buy took: {:.3}s, Total processing: {:.3}s", 
                 mint, buy_elapsed.as_secs_f64(), total_elapsed.as_secs_f64());
            
            // Process the buy result
            process_buy_result(buy_result, client, wallet, mint, start_time).await;
        } else {
            info!("❌ FAILED TO BUY: {} - Elapsed: {:.3}s - Reason: {}", 
                 mint, total_elapsed.as_secs_f64(), buy_result.data.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown"));
        }
    }
    
    Ok(())
}

/// Process a successful buy result
async fn process_buy_result(
    buy_result: api::ApiResponse,
    client: &Arc<reqwest::Client>,
    wallet: &str,
    mint: &str,
    start_time: Instant
) {
    // Log the successful buy
    info!("✅ Successfully bought token: {}", mint);
    
    // Try to get the current balance
    match api::get_balance(&**client, wallet, mint).await {
        Ok(balance) => {
            info!("Current balance: {} tokens", balance);
        },
        Err(e) => {
            warn!("Failed to get balance: {}", e);
        }
    }
    
    // Try to get the current price
    match api::get_price(&**client, mint).await {
        Ok(price) => {
            info!("Current price: ${:.6}", price);
        },
        Err(e) => {
            warn!("Failed to get price: {}", e);
        }
    }
    
    // Log total processing time
    let total_elapsed = start_time.elapsed();
    info!("Total processing time: {:.3}s", total_elapsed.as_secs_f64());
}

/// Create an optimized HTTP client for fast API calls
fn create_optimized_client() -> Result<reqwest::Client> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .tcp_nodelay(true)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .pool_max_idle_per_host(10)
        .build()?;
    
    Ok(client)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize environment variables from .env file
    dotenv().ok();
    
    // Get any logging preference from command-line arguments
    let show_only_token_creation = std::env::args().any(|arg| arg == "--quiet" || arg == "-q");
    
    // Initialize logging with custom filter
    let log_filter = if show_only_token_creation {
        // Only show token creation messages and errors/warnings
        "error,warn,pumpfun_sniper::websocket_test=info"
    } else {
        // Show all info logs (default behavior)
        "info"
    };
    
    let env = env_logger::Env::default()
        .filter_or("RUST_LOG", log_filter);
    
    // Capture quiet mode flag for the formatter
    let quiet_mode = show_only_token_creation;
    
    env_logger::Builder::from_env(env)
        .format_timestamp_millis()
        .format(move |buf, record| {
            // Special formatting for token creation messages
            if record.args().to_string().contains("NEW TOKEN CREATED!") {
                // Always include timestamp for token creation messages
                writeln!(
                    buf,
                    "[{}] {}",
                    buf.timestamp_millis(),
                    record.args()
                )
            } else {
                // Standard format for other logs
                writeln!(
                    buf,
                    "[{}] {} {}",
                    buf.timestamp_millis(),
                    record.level(),
                    record.args()
                )
            }
        })
        .init();
    
    info!("Starting Pump.fun Sniper Bot...");
    
    // Check if we should extract tokens from WebSocket
    if std::env::args().any(|arg| arg == "--extract-tokens" || arg == "-e") {
        info!("Extracting token data from WebSocket messages");
        
        // Get WebSocket endpoint from environment or use authenticated URL from chainstack_simple
        let wss_endpoint = std::env::var("WSS_ENDPOINT")
            .unwrap_or_else(|_| chainstack_simple::get_authenticated_wss_url());
            
        info!("Using WebSocket endpoint: {}", wss_endpoint);

        // Run the WebSocket test to collect token data
        let token_data_list = websocket_test::run_websocket_test(&wss_endpoint).await?;
        
        // Display the results
        println!("\n{:-^80}", " TOKEN CREATION EVENTS ");
        
        if token_data_list.is_empty() {
            println!("No token creation events detected during the test period.");
        } else {
            println!("Found {} token creation events:", token_data_list.len());
            println!("\n{:<5} {:<20} {:<10} {:<44} {:<44} {:<44}", 
                     "#", "NAME", "SYMBOL", "MINT", "BONDING CURVE", "CREATOR");
            println!("{:-<170}", "");
            
            for (i, token) in token_data_list.iter().enumerate() {
                println!("{:<5} {:<20} {:<10} {:<44} {:<44} {:<44}", 
                         i+1, 
                         token.name, 
                         token.symbol, 
                         token.mint, 
                         token.bonding_curve, 
                         token.user);
            }
        }
        
        return Ok(());
    }
    
    // Check if we should monitor for new tokens via WebSocket
    if std::env::args().any(|arg| arg == "--monitor-websocket" || arg == "-m") {
        info!("Starting WebSocket monitor for new tokens with automatic buying");
        
        // Read configuration
        let check_min_liquidity = std::env::var("CHECK_MIN_LIQUIDITY")
            .unwrap_or_else(|_| "false".to_string()) == "true";
        let approved_devs_only = std::env::var("APPROVED_DEVS_ONLY")
            .unwrap_or_else(|_| "false".to_string()) == "true";
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
        
        let duration = std::env::var("MONITOR_DURATION")
            .map(|v| v.parse::<u64>().unwrap_or(3600))
            .unwrap_or(3600);
            
        // Initialize HTTP client
        let client = Arc::new(create_optimized_client()?);
        
        // Display configuration
        println!("\n{:-^100}", " PUMP.FUN TOKEN MONITOR ");
        println!("Mode: {}", if approved_devs_only { "APPROVED DEV (Auto-Buy without Liquidity Check)" } 
                               else if check_min_liquidity { "STANDARD (Verify Liquidity Before Buy)" }
                               else { "AGGRESSIVE (Buy Without Liquidity Check)" });
        println!("Amount: {} SOL", amount);
        println!("Slippage: {}%", slippage);
        if !snipe_by_tag.is_empty() {
            println!("Tag Filter: {}", snipe_by_tag);
        }
        println!("Monitor Duration: {} seconds", duration);
        println!("Press Ctrl+C at any time to stop monitoring");
        println!("{:-^100}", "");
        
        // Initialize the WebSocket - use the authenticated WebSocket URL from chainstack_simple
        let wss_endpoint = std::env::var("WSS_ENDPOINT")
            .unwrap_or_else(|_| chainstack_simple::get_authenticated_wss_url());
        
        info!("Using WebSocket endpoint: {}", wss_endpoint);
        
        // This will collect new tokens
        let token_data_list = websocket_test::run_websocket_test(&wss_endpoint).await?;
        
        // Process each token found
        if !token_data_list.is_empty() {
            for token in &token_data_list {
                // Process each new token for potential buying
                if let Err(e) = process_new_tokens_from_websocket(
                    &client,
                    token,
                    check_min_liquidity,
                    approved_devs_only,
                    &snipe_by_tag,
                    &private_key,
                    &wallet,
                    amount,
                    slippage
                ).await {
                    warn!("Error processing token {}: {}", token.mint, e);
                }
            }
        }
        
        // Display summary after monitoring completes
        println!("\n{:-^100}", " MONITORING SUMMARY ");
        println!("Total tokens detected: {}", token_data_list.len());
        
        if !token_data_list.is_empty() {
            println!("\nDetected tokens:");
            
            for (i, token) in token_data_list.iter().enumerate() {
                println!("{}. {} ({}) - Mint: {}", 
                    i+1, token.name, token.symbol, token.mint);
            }
        } else {
            println!("No tokens were detected during the monitoring period.");
        }
        
        return Ok(());
    }
    
    // Default behavior - display help
    println!("Pump.fun Sniper Bot - Available Commands:");
    println!("  --extract-tokens, -e : Extract token data from WebSocket messages");
    println!("  --monitor-websocket, -m : Monitor for new tokens via WebSocket with automatic buying");
    println!("  --quiet, -q : Only show token creation messages and suppress other logs");
    
    Ok(())
} 