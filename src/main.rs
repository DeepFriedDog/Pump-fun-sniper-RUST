#![allow(unused)]
#![feature(proc_macro_hygiene)]

// Module declarations
mod api;
mod chainstack_simple;
mod checks;
mod config;
mod db;
mod error;
mod tests;
mod token_detector;
mod websocket_reconnect;
mod websocket_test;

// Standard library imports
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
) -> std::result::Result<(), String> {
    // Measure start time for performance tracking
    let start_time = std::time::Instant::now();

    // Extract data from TokenData
    let mint = &token_data.mint;
    let name = &token_data.name;
    let symbol = &token_data.symbol;
    let user = &token_data.user; // Creator/developer address

    // Skip the already processed check for fresh tokens
    let now = Instant::now();
    let should_process = {
        let mut last_processed = LAST_PROCESSED_TOKENS.lock().unwrap();

        if let Some(last_time) = last_processed.get(mint) {
            if last_time.elapsed() < Duration::from_secs(5) {
                // Token was processed very recently, skip
                false
            } else {
                // Update the last processed time for this mint
                last_processed.insert(mint.clone(), now);
                true
            }
        } else {
            // Token wasn't processed before, add it
            last_processed.insert(mint.clone(), now);
            true
        }
    };

    if !should_process {
        return Ok(());
    }

    // Comment out the initial inaccurate liquidity calculation that causes duplicate processing
    // let liquidity = match api::calculate_liquidity_from_bonding_curve(&mint, &user, amount).await {
    //     Ok(liq) => liq,
    //     Err(_) => 0.5, // Default value if calculation fails
    // };
    let liquidity = 0.0; // We'll use the accurate value from check_token_liquidity instead

    // Determine opportunity status
    let opportunity_status = "💎"; // Diamond for confirmed token

    // Comment out initial token logging with inaccurate liquidity
    // We'll log after the accurate check_token_liquidity call instead
    // info!("🪙 NEW TOKEN CREATED! {} (mint: {}) 💰 {:.2} SOL {}",
    //     name,
    //     mint,
    //     liquidity,
    //     opportunity_status);

    // Check if tag matches if a tag filter is specified
    if !snipe_by_tag.trim().is_empty() {
        let token_name = name.to_lowercase();
        if !token_name.contains(&snipe_by_tag.to_lowercase()) {
            info!(
                "Token name doesn't match the tag filter '{}', skipping",
                snipe_by_tag
            );
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
            }
            Err(e) => {
                warn!("Failed to check if developer is approved: {}", e);
                false
            }
        }
    } else {
        false
    };

    // Fast path for approved developers - skip liquidity check
    if approved_devs_only {
        info!("Skipping liquidity check and buying immediately");

        // Buy the token immediately using Chainstack warp transaction
        let buy_start = std::time::Instant::now();
        let buy_result = match api::buy_token(&**client, private_key, mint, amount, slippage).await
        {
            Ok(result) => result,
            Err(e) => return Err(format!("Failed to buy token: {}", e)),
        };

        // Process buy result and log speed
        let buy_elapsed = buy_start.elapsed();
        let total_elapsed = start_time.elapsed();

        if buy_result.status == "success" {
            info!(
                "✅ BOUGHT TOKEN: {} - Buy took: {:.3}s, Total processing: {:.3}s",
                mint,
                buy_elapsed.as_secs_f64(),
                total_elapsed.as_secs_f64()
            );

            // Process the buy result for DB storage and price monitoring
            if let Err(e) = process_buy_result(buy_result, client, wallet, mint, start_time).await {
                warn!("Error processing buy result: {}", e);
            }
        } else {
            info!(
                "❌ FAILED TO BUY: {} - Elapsed: {:.3}s - Reason: {}",
                mint,
                total_elapsed.as_secs_f64(),
                buy_result
                    .data
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown")
            );
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
                info!(
                    "Bonding curve liquidity: {} SOL, Required: {} SOL",
                    balance, min_liquidity
                );

                if has_liquidity {
                    info!("✅ Liquidity check PASSED - Proceeding with buy");

                    // Execute buy transaction
                    let buy_start = std::time::Instant::now();
                    let buy_result = match api::buy_token(
                        &**client,
                        private_key,
                        mint,
                        amount,
                        slippage,
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(e) => return Err(format!("Failed to buy token: {}", e)),
                    };

                    // Process buy result
                    let buy_elapsed = buy_start.elapsed();
                    let total_elapsed = start_time.elapsed();

                    if buy_result.status == "success" {
                        info!(
                            "✅ BOUGHT TOKEN: {} - Buy took: {:.3}s, Total processing: {:.3}s",
                            mint,
                            buy_elapsed.as_secs_f64(),
                            total_elapsed.as_secs_f64()
                        );

                        // Process the buy result
                        if let Err(e) =
                            process_buy_result(buy_result, client, wallet, mint, start_time).await
                        {
                            warn!("Error processing buy result: {}", e);
                        }
                    } else {
                        info!(
                            "❌ FAILED TO BUY: {} - Elapsed: {:.3}s - Reason: {}",
                            mint,
                            total_elapsed.as_secs_f64(),
                            buy_result
                                .data
                                .get("message")
                                .and_then(|m| m.as_str())
                                .unwrap_or("Unknown")
                        );
                    }
                } else {
                    info!("❌ Liquidity check FAILED - Skipping buy");
                }
            }
            Err(e) => {
                warn!("Failed to check liquidity: {}", e);
            }
        }
    } else {
        // If we're not checking liquidity, buy directly
        info!("Liquidity check disabled - Proceeding with buy");

        // Buy the token immediately
        let buy_start = std::time::Instant::now();
        let buy_result = match api::buy_token(&**client, private_key, mint, amount, slippage).await
        {
            Ok(result) => result,
            Err(e) => return Err(format!("Failed to buy token: {}", e)),
        };

        // Process buy result
        let buy_elapsed = buy_start.elapsed();
        let total_elapsed = start_time.elapsed();

        if buy_result.status == "success" {
            info!(
                "✅ BOUGHT TOKEN: {} - Buy took: {:.3}s, Total processing: {:.3}s",
                mint,
                buy_elapsed.as_secs_f64(),
                total_elapsed.as_secs_f64()
            );

            // Process the buy result
            if let Err(e) = process_buy_result(buy_result, client, wallet, mint, start_time).await {
                warn!("Error processing buy result: {}", e);
            }
        } else {
            info!(
                "❌ FAILED TO BUY: {} - Elapsed: {:.3}s - Reason: {}",
                mint,
                total_elapsed.as_secs_f64(),
                buy_result
                    .data
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown")
            );
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
    start_time: Instant,
) -> std::result::Result<(), String> {
    // Extract transaction signature from the response
    let tx_signature = buy_result
        .data
        .get("transaction")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    info!("Transaction signature: {}", tx_signature);

    // Get the token price
    match api::get_price(&**client, mint).await {
        Ok(price) => {
            info!("Token price: {} SOL", price);

            // Start price monitoring if enabled
            if std::env::var("MONITOR_PRICE").unwrap_or_else(|_| "true".to_string()) == "true" {
                let take_profit = std::env::var("TAKE_PROFIT")
                    .unwrap_or_else(|_| "100".to_string())
                    .parse::<f64>()
                    .unwrap_or(100.0);

                let stop_loss = std::env::var("STOP_LOSS")
                    .unwrap_or_else(|_| "50".to_string())
                    .parse::<f64>()
                    .unwrap_or(50.0);

                info!(
                    "Starting price monitor with take profit: {}%, stop loss: {}%",
                    take_profit, stop_loss
                );

                // Clone values for the async block
                let client_clone = client.clone();
                let mint_clone = mint.to_string();
                let wallet_clone = wallet.to_string();

                // Start price monitoring in a separate task
                tokio::spawn(async move {
                    if let Err(e) = api::start_price_monitor(
                        &client_clone,
                        &mint_clone,
                        &wallet_clone,
                        price,
                        take_profit,
                        stop_loss,
                    )
                    .await
                    {
                        warn!("Price monitoring error: {}", e);
                    }
                });
            }
        }
        Err(e) => {
            warn!("Failed to get price: {}", e);
        }
    }

    // Log total processing time
    let total_elapsed = start_time.elapsed();
    info!("Total processing time: {:.3}s", total_elapsed.as_secs_f64());

    Ok(())
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

/// Run in monitor websocket mode
async fn monitor_websocket() -> Result<()> {
    // Check if we're in quiet mode
    let is_quiet_mode = std::env::args().any(|arg| arg == "--quiet" || arg == "-q");
    
    if !is_quiet_mode {
        info!("Starting WebSocket monitor for new tokens with automatic buying");
    }

    // Read configuration
    let check_min_liquidity =
        std::env::var("CHECK_MIN_LIQUIDITY").unwrap_or_else(|_| "false".to_string()) == "true";
    let approved_devs_only =
        std::env::var("APPROVED_DEVS_ONLY").unwrap_or_else(|_| "false".to_string()) == "true";
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

    // Get duration setting - use 0 for indefinite monitoring (will run until Ctrl+C)
    let duration = std::env::var("MONITOR_DURATION")
        .map(|v| v.parse::<u64>().unwrap_or(0))
        .unwrap_or(0);

    // Initialize HTTP client
    let client = Arc::new(create_optimized_client()?);

    // Display configuration only if not in quiet mode
    if !is_quiet_mode {
        println!("\n{:-^100}", " PUMP.FUN TOKEN MONITOR ");
        println!(
            "Mode: {}",
            if approved_devs_only {
                "APPROVED DEV (Auto-Buy without Liquidity Check)"
            } else if check_min_liquidity {
                "STANDARD (Verify Liquidity Before Buy)"
            } else {
                "AGGRESSIVE (Buy Without Liquidity Check)"
            }
        );
        println!("Amount: {} SOL", amount);
        println!("Slippage: {}%", slippage);
        if !snipe_by_tag.is_empty() {
            println!("Tag Filter: {}", snipe_by_tag);
        }

        // Show duration info
        if duration > 0 {
            println!("Monitor Duration: {} seconds", duration);
        } else {
            println!("Monitor Duration: INDEFINITE (press Ctrl+C to stop)");
        }

        println!("Auto-reconnect: ENABLED (will automatically reconnect if WebSocket disconnects)");
        println!("Press Ctrl+C at any time to stop monitoring");
        println!("{:-^100}", "");
    }

    // This will collect all tokens found during the monitoring session
    let mut token_data_list = Vec::new();

    // Reconnection settings
    let initial_backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);
    let mut current_backoff = initial_backoff;
    let backoff_factor = 2.0;
    let mut consecutive_failures = 0;

    // Master loop that handles reconnections and monitoring sessions
    loop {
        // Initialize the WebSocket - use the authenticated WebSocket URL from chainstack_simple
        let wss_endpoint = std::env::var("WSS_ENDPOINT")
            .unwrap_or_else(|_| chainstack_simple::get_authenticated_wss_url());

        if !is_quiet_mode {
            info!("Using WebSocket endpoint: {}", wss_endpoint);
        }

        // Run the WebSocket test and handle errors
        match websocket_test::run_websocket_test(&wss_endpoint).await {
            Ok(partial_list) => {
                // Success! Reset the backoff since we had a successful connection
                current_backoff = initial_backoff;
                consecutive_failures = 0;

                // Process each token found
                if !partial_list.is_empty() {
                    for token in &partial_list {
                        // Add to our master list
                        token_data_list.push(token.clone());

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
                            slippage,
                        )
                        .await
                        {
                            warn!("Error processing token {}: {}", token.mint, e);
                        }
                    }
                }

                info!(
                    "WebSocket monitoring cycle completed. Total tokens found so far: {}",
                    token_data_list.len()
                );

                // If we have a non-zero duration, exit after one iteration
                if duration > 0 {
                    break;
                }

                // Small delay between cycles to prevent overloading the server
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => {
                // WebSocket connection failed
                consecutive_failures += 1;

                // Log the error
                error!(
                    "WebSocket connection error: {}. Reconnection attempt #{}",
                    e, consecutive_failures
                );

                // Calculate the exponential backoff delay
                if consecutive_failures > 1 {
                    let backoff_secs = current_backoff.as_secs() * backoff_factor as u64;
                    current_backoff = std::cmp::min(Duration::from_secs(backoff_secs), max_backoff);
                }

                // Log the reconnection attempt
                info!(
                    "Reconnecting to WebSocket in {} seconds...",
                    current_backoff.as_secs()
                );

                // Wait before reconnecting with exponential backoff
                tokio::time::sleep(current_backoff).await;

                // If this is a time-limited session and we've exceeded the duration, exit
                if duration > 0 {
                    warn!("Monitoring session terminated due to connection issues");
                    break;
                }

                // Otherwise continue the reconnection loop
                continue;
            }
        }
    }

    // Display summary after monitoring completes (only reached if duration > 0 or on program exit)
    println!("\n{:-^100}", " MONITORING SUMMARY ");
    println!("Total tokens detected: {}", token_data_list.len());

    if !token_data_list.is_empty() {
        println!("\nDetected tokens:");

        for (i, token) in token_data_list.iter().enumerate() {
            println!(
                "{}. {} ({}) - Mint: {}",
                i + 1,
                token.name,
                token.symbol,
                token.mint
            );
        }
    } else {
        println!("No tokens were detected during the monitoring period.");
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Read environment variables
    dotenv().ok();

    // Load WebSocket settings
    let use_websocket = std::env::var("USE_WEBSOCKET")
        .unwrap_or_else(|_| "true".to_string())
        .parse::<bool>()
        .unwrap_or(true);

    let websocket_debug = std::env::var("WEBSOCKET_DEBUG")
        .unwrap_or_else(|_| "false".to_string())
        .parse::<bool>()
        .unwrap_or(false);

    info!("WebSocket enabled: {}", use_websocket);

    // Initialize logging with appropriate level
    let is_quiet_mode = std::env::args().any(|arg| arg == "--quiet" || arg == "-q");
    let log_level = if websocket_debug {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };

    // Initialize the logger with custom formatting
    let mut builder = env_logger::Builder::new();
    
    // In quiet mode, only show token creation logs
    if is_quiet_mode {
        // Only allow specific log messages in quiet mode
        builder.format(|buf, record| {
            // Only show token creation logs
            let msg = format!("{}", record.args());
            if msg.contains("NEW TOKEN CREATED") {
                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                writeln!(buf, "{} [{}] {}", timestamp, record.level(), record.args())
            } else {
                Ok(()) // Don't print other messages
            }
        });
    } else {
        // Standard formatting for normal mode
        builder.format(|buf, record| {
            let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            writeln!(buf, "{} [{}] {}", timestamp, record.level(), record.args())
        });
    }
    
    // Set the log level and initialize
    builder.filter_level(log_level);
    builder.init();

    // Print the banner only when not in quiet mode
    if !is_quiet_mode {
        println!("\n{:-^100}", " PUMP.FUN TOKEN SNIPER ");
        println!("Version: 1.0.0");
        println!("Author: @solanadev");
        println!("Website: https://pump.fun");
        println!("{:-^100}", "");
    }

    // Create an optimized HTTP client
    let client = Arc::new(create_optimized_client()?);

    // Check command line arguments
    let args: Vec<String> = std::env::args().collect();

    // Process command line arguments
    if args.len() > 1 {
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
                println!(
                    "\n{:<5} {:<20} {:<10} {:<44} {:<44} {:<44}",
                    "#", "NAME", "SYMBOL", "MINT", "BONDING CURVE", "CREATOR"
                );
                println!("{:-<170}", "");

                for (i, token) in token_data_list.iter().enumerate() {
                    println!(
                        "{:<5} {:<20} {:<10} {:<44} {:<44} {:<44}",
                        i + 1,
                        token.name,
                        token.symbol,
                        token.mint,
                        token.bonding_curve,
                        token.user
                    );
                }
            }

            return Ok(());
        }

        // Check if we should monitor for new tokens via WebSocket
        if std::env::args().any(|arg| arg == "--monitor-websocket" || arg == "-m") {
            return monitor_websocket().await;
        }

        // Default behavior - display help (this will only be reached if --help or -h is provided)
        println!("Pump.fun Sniper Bot - Available Commands:");
        println!("  --extract-tokens, -e : Extract token data from WebSocket messages");
        println!("  --monitor-websocket, -m : Monitor for new tokens via WebSocket with automatic buying (default mode)");
        println!("  --quiet, -q : Only show token creation messages and suppress other logs");

        return Ok(());
    }

    Ok(())
}
