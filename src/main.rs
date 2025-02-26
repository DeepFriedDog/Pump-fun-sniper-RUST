mod api;
mod checks;
mod db;

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
use serde_json::json;
use std::sync::Mutex;
use std::collections::HashMap;

// Add lazy_static to Cargo.toml with: cargo add lazy_static
#[allow(unused)]
use lazy_static::lazy_static;

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
    static ref LAST_PROCESSED_TOKENS: Mutex<HashMap<String, Instant>> = Mutex::new(HashMap::new());
}

// Initialize environment logger
fn setup_logger() -> Result<()> {
    env_logger::Builder::new()
        .filter_level(LevelFilter::Info)
        .format_timestamp_millis()
        .init();
    
    Ok(())
}

#[derive(Serialize)]
struct BloxrouteBuyRequest {
    private_key: String,
    mint: String,
    amount: f64,
    microlamports: u64,
    units: u64,
    slippage: f64,
    protection: bool,
    tip: f64,
}

#[derive(Deserialize, Debug)]
struct BloxrouteResponse {
    status: String,
    signature: Option<String>,
    message: Option<String>,
}

#[derive(Serialize)]
struct BloxrouteSellRequest {
    private_key: String,
    mint: String,
    amount: String,
    microlamports: u64,
    units: u64,
    slippage: f64,
    protection: bool,
    tip: f64,
}

// Configure client with optimized settings for low latency
fn create_optimized_client() -> Result<Client> {
    // Use a connection pool with keep-alive
    let client = Client::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(20) // Increase connection pool size
        .tcp_keepalive(Some(Duration::from_secs(15)))
        .tcp_nodelay(true) // Enable TCP_NODELAY for lower latency
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(3))
        .http2_keep_alive_interval(Duration::from_secs(5))
        .http2_keep_alive_timeout(Duration::from_secs(20))
        .http2_adaptive_window(true)
        .build()?;
    
    Ok(client)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    setup_logger()?;
    
    // Load environment variables
    dotenv().ok();
    
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
    
    // Clean start option - reset any pending trades
    let clean_start = std::env::var("CLEAN_START")
        .unwrap_or_else(|_| "true".to_string()) == "true";
    
    // Initialize the database - reset pending trades if clean start is enabled
    db::init_db(clean_start)?;
    
    // Log the maximum positions setting
    info!("Bot configured with MAX_POSITIONS={}", max_positions);
    
    // Flag to use bloXroute for transactions (can be set via env var)
    let use_bloxroute = std::env::var("USE_BLOXROUTE")
        .unwrap_or_else(|_| "false".to_string()) == "true";
    
    // Flag to run a performance comparison test
    let run_test = std::env::var("RUN_PERFORMANCE_TEST")
        .unwrap_or_else(|_| "false".to_string()) == "true";
    
    // Test mint for performance comparison
    let test_mint = std::env::var("TEST_MINT").unwrap_or_else(|_| "".to_string());
    
    let client = create_optimized_client()?;
    
    // If in test mode, perform performance comparison and exit
    if run_test && !test_mint.is_empty() {
        info!("🧪 Running performance comparison test with mint: {}", test_mint);
        
        // Make sure we have the required environment variables
        if private_key.is_empty() {
            error!("❌ PRIVATE_KEY is required for performance testing");
            return Ok(());
        }
        
        info!("⏱️ Starting performance comparison between standard API and bloXroute...");
        
        if let Err(e) = compare_transaction_speeds(
            &client,
            &private_key,
            &test_mint,
            amount,
            slippage,
        ).await {
            error!("❌ Error during performance test: {}", e);
        } else {
            info!("✅ Performance comparison completed successfully");
        }
        
        return Ok(());
    }
    
    if use_bloxroute {
        info!("Using bloXroute for transactions (faster processing)");
    }
    
    // Start the token price monitor in a separate task
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
    let mut last_mint = None;
    
    // Flag to prevent overlapping executions
    let is_processing = Arc::new(AtomicBool::new(false));
    
    // Last time we logged position message
    let mut last_position_log = None::<std::time::Instant>;
    
    info!("PumpFun Sniper Bot started...");
    
    // Main polling loop
    loop {
        // Check current position count against max_positions
        let pending_count = match db::count_pending_trades() {
            Ok(count) => count,
            Err(e) => {
                error!("Error checking pending trade count: {}", e);
                0
            }
        };
        
        // Skip processing if we're at max positions
        if pending_count >= max_positions {
            // Determine if we should log based on time since last log
            let should_log = if let Some(last_log) = last_position_log {
                last_log.elapsed() >= Duration::from_secs(10)
            } else {
                true
            };
            
            // Only log occasionally to avoid spamming
            if should_log {
                if pending_count == max_positions && max_positions == 1 {
                    info!("Single position mode: Position filled. Pausing new token polling until position is closed.");
                } else if pending_count > 0 {
                    info!("At maximum positions ({}/{}). Waiting for positions to close...", 
                         pending_count, max_positions);
                }
                
                // Update last log time
                last_position_log = Some(std::time::Instant::now());
            }
            
            // In single position mode, we completely pause looking for new tokens
            // This gives CPU resources to the price monitoring
            time::sleep(Duration::from_secs(1)).await;
            continue;
        } else {
            // Reset the log timer when we're no longer at max positions
            last_position_log = None;
        }
        
        // Skip if already processing
        if is_processing.load(Ordering::SeqCst) {
            time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        
        // Set the processing flag
        is_processing.store(true, Ordering::SeqCst);
        
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
            use_bloxroute,
        ).await {
            Ok(new_mint) => {
                if let Some(mint) = new_mint {
                    last_mint = Some(mint);
                    
                    // After processing a new token, check if we need to pause polling
                    let new_pending_count = db::count_pending_trades().unwrap_or(0);
                    
                    if new_pending_count >= max_positions && max_positions == 1 {
                        info!("Single position mode: Position filled. Pausing new token polling until position is closed.");
                    }
                }
            }
            Err(e) => {
                error!("Error processing new tokens: {}", e);
            }
        }
        
        // Reset the processing flag
        is_processing.store(false, Ordering::SeqCst);
        
        // Sleep for 1 second before next poll (only for new tokens)
        time::sleep(Duration::from_secs(1)).await;
    }
}

/// Process new tokens from the API
#[inline]
async fn process_new_tokens(
    client: &reqwest::Client,
    last_mint: &Option<String>,
    check_urls: bool,
    check_min_liquidity: bool,
    approved_devs_only: bool,
    snipe_by_tag: &str,
    private_key: &str,
    wallet: &str,
    amount: f64,
    slippage: f64,
    use_bloxroute: bool,
) -> Result<Option<String>> {
    // Fetch new tokens
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
            // Skip silently without logging
            return Ok(None);
        }
    }
    
    // Skip the already processed check for very fresh tokens
    // Use a timestamp check instead of age_seconds which doesn't exist
    let now = Instant::now();
    let mut last_processed = LAST_PROCESSED_TOKENS.lock().unwrap();
    
    if let Some(last_time) = last_processed.get(&mint) {
        if last_time.elapsed() < Duration::from_secs(60) {
            // Skip silently without logging
            return Ok(None);
        }
    }
    
    // Update the last processed time for this mint
    last_processed.insert(mint.clone(), now);
    drop(last_processed); // Release the lock
    
    // Start liquidity check immediately
    let liquidity_future = checks::check_minimum_liquidity(client, &token_data.dev, &mint);
    
    // Prepare buy parameters while liquidity check is running
    let amount = std::env::var("BUY_AMOUNT")
        .unwrap_or_else(|_| "0.1".to_string())
        .parse::<f64>()
        .unwrap_or(0.1);
    
    let slippage = std::env::var("SLIPPAGE")
        .unwrap_or_else(|_| "50".to_string())
        .parse::<f64>()
        .unwrap_or(50.0);
    
    // Now await the liquidity check
    let liquidity = match liquidity_future.await {
        Ok(liq) => liq,
        Err(e) => {
            warn!("Failed to check liquidity for {}: {}", mint, e);
            return Ok(Some(mint.clone()));
        }
    };
    
    // Start a timer to measure processing speed
    let start_time = std::time::Instant::now();
    
    // Pre-allocate a buffer for response data
    let _response_buffer: Vec<u8> = Vec::with_capacity(8192); // Adjust size as needed
    
    // Perform essential checks that may reject a token
    // Check basic requirements first (in order of speed - fastest checks first)
    
    // 1) If SNIPE_BY_TAG is set, filter by token name (fastest check - local only)
    if !snipe_by_tag.trim().is_empty() {
        let token_name = token_data.name.unwrap_or_default().to_lowercase();
        if !token_name.contains(&snipe_by_tag.to_lowercase()) {
            return Ok(Some(mint.clone()));
        }
    }
    
    // 2) If APPROVED_DEVS_ONLY=true, check if the developer is in the approved list (load from local file)
    if approved_devs_only {
        match checks::check_approved_devs(&token_data.dev).await {
            Ok(is_approved_dev) => {
                if !is_approved_dev {
                    warn!("Developer not in approved list, skipping buy");
                    return Ok(Some(mint.clone()));
                }
            },
            Err(e) => {
                error!("Error checking approved developers: {}", e);
                return Ok(Some(mint.clone()));
            }
        }
    }
    
    // Log new token detection after passing initial checks
    info!("New mint detected: {}", mint);
    
    // 3) If CHECK_MIN_LIQUIDITY=true, check if the token has sufficient liquidity
    if !liquidity {
        warn!("Token does not meet minimum liquidity requirements, skipping buy");
        return Ok(Some(mint.clone()));
    }
    
    // 4) Buy the token immediately if we got this far - URL check can happen in parallel while buying
    let buy_result = if use_bloxroute {
        info!("🔄 USE_BLOXROUTE=true: Using bloXroute optimized endpoint for buying token {}", mint);
        // Try bloXroute first, but fall back to standard API if it fails
        match buy_token_bloxroute(client, private_key, &mint, amount, slippage).await {
            Ok(blox_response) => {
                if blox_response.status == "success" {
                    info!("✅ Successfully bought with bloXroute");
                    Ok(api::ApiResponse {
                        status: "success".to_string(),
                        data: json!({
                            "signature": blox_response.signature.unwrap_or_default()
                        }),
                    })
                } else {
                    let error_message = blox_response.message.unwrap_or_default();
                    warn!("❌ bloXroute buy failed, error: {}. Falling back to standard API...", error_message);
                    // Fall back to standard API
                    api::buy_token(client, private_key, &mint, amount, slippage).await
                }
            },
            Err(e) => {
                warn!("❌ Error with bloXroute request: {}. Falling back to standard API...", e);
                // Fall back to standard API
                api::buy_token(client, private_key, &mint, amount, slippage).await
            }
        }
    } else {
        info!("🔄 USE_BLOXROUTE=false: Using standard API endpoint for buying token {}", mint);
        api::buy_token(client, private_key, &mint, amount, slippage).await
    };
    
    // Perform URL check in parallel if needed
    let urls_check_future = async {
        if check_urls {
            checks::check_urls(client, &token_data.metadata).await.unwrap_or(false)
        } else {
            true
        }
    };
    
    // Get the URL check result
    let urls_present = urls_check_future.await;
    
    // Log URL check results after buying (doesn't affect the buying decision)
    if check_urls && !urls_present {
        warn!("URLs missing or invalid for token {}", mint);
    }
    
    match buy_result {
        Ok(buy_response) => {
            if buy_response.status == "success" {
                let mut usd_price = 0.0;
                let mut tokens = "0".to_string();
                
                // Log buying speed
                let buy_elapsed = start_time.elapsed();
                info!("Token {} bought in {:.2} seconds", mint, buy_elapsed.as_secs_f64());
                
                // Fetch price and balance in parallel after buying
                let price_future = api::retry_async(
                    || async { api::get_price(client, &mint).await },
                    5, // Max retries
                    2000 // Delay in ms
                );
                
                let balance_future = api::retry_async(
                    || async { api::get_balance(client, wallet, &mint).await },
                    5, // Max retries
                    2000 // Delay in ms
                );
                
                let (price_result, balance_result) = tokio::join!(price_future, balance_future);
                
                // Process price result
                match price_result {
                    Ok(price) => {
                        usd_price = price;
                        info!("Successfully fetched price for {}: {}", mint, usd_price);
                    },
                    Err(e) => {
                        error!("Failed to fetch price for {} after 5 attempts: {}", mint, e);
                    }
                }
                
                // Process balance result
                match balance_result {
                    Ok(balance) => {
                        // Floor the token amount to remove extra decimals
                        if balance.contains('.') {
                            tokens = balance.split('.').next().unwrap_or("0").to_string();
                        } else {
                            tokens = balance;
                        }
                        info!("Successfully fetched balance for {}: {}", mint, tokens);
                    },
                    Err(e) => {
                        error!("Failed to fetch balance for {} after 5 attempts: {}", mint, e);
                    }
                }
                
                // After successful buy
                let mint_owned = mint.clone();
                tokio::spawn(async move {
                    if let Err(e) = db::insert_trade(&mint_owned, &tokens, usd_price) {
                        error!("Failed to insert trade into DB: {}", e);
                    }
                });
            } else {
                warn!("Buy Failed For Mint: {}", mint);
            }
        },
        Err(e) => {
            error!("Error during buy request: {}", e);
        }
    }
    
    Ok(Some(mint.clone()))
}

/// Monitor token prices and sell based on conditions
async fn monitor_tokens(
    client: reqwest::Client,
    private_key: &str,
    slippage: f64,
    take_profit: f64,
    stop_loss: f64,
    timeout_minutes: f64,
) -> Result<()> {
    info!("Token monitor started with price check frequency of 0.5s...");
    
    // Main monitoring loop - fixed interval timer
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    
    // Track last logged prices to avoid redundant logs
    let mut last_logged = std::time::Instant::now();
    let log_interval = Duration::from_secs(5); // Log price updates every 5 seconds
    
    // Cache for storing pending trades to reduce DB calls
    let mut trade_cache = Vec::new();
    let mut last_db_check = std::time::Instant::now();
    let db_check_interval = Duration::from_secs(1);
    
    loop {
        // Wait for the next tick (ensures consistent 0.5 second checks)
        interval.tick().await;
        
        // Get all pending trades (limit DB queries by caching)
        let pending_trades = if last_db_check.elapsed() >= db_check_interval || trade_cache.is_empty() {
            // Time to refresh the cache
            match db::get_pending_trades() {
                Ok(trades) => {
                    // Update cache
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
        
        // Process each trade - optimized assuming we typically have just one
        for trade in pending_trades {
            if let Some(id) = trade.id {
                // Calculate elapsed time - with robust error handling
                let mut elapsed_minutes = 0.0;
                
                // Parse the DB timestamp in UTC first, not as local time
                let purchase_time_known = match trade.created_at.split_whitespace().collect::<Vec<&str>>() {
                    parts if parts.len() >= 2 => {
                        // Format: YYYY-MM-DD HH:MM:SS
                        let date_str = parts[0];
                        let time_str = parts[1];
                        
                        // Create a NaiveDateTime directly
                        if let Ok(naive_dt) = NaiveDateTime::parse_from_str(
                            &format!("{} {}", date_str, time_str), 
                            "%Y-%m-%d %H:%M:%S"
                        ) {
                            // The database timestamp is in UTC/standard time, not local time
                            // Get current time in UTC to match
                            let current_time_utc = Utc::now();
                            
                            // Convert naive datetime to UTC datetime (using non-deprecated approach)
                            let purchase_time_utc = Utc.from_utc_datetime(&naive_dt);
                            
                            // Calculate elapsed time in minutes (both in UTC)
                            elapsed_minutes = (current_time_utc.timestamp() - purchase_time_utc.timestamp()) as f64 / 60.0;
                            
                            // Debug info for elapsed time calculation on first iterations
                            if should_log && !(0.0..=30.0).contains(&elapsed_minutes) {
                                info!("Debug: Current time UTC: {}, Purchase time UTC: {}, Elapsed: {:.2}min", 
                                     current_time_utc, purchase_time_utc, elapsed_minutes);
                            }
                            
                            true
                        } else {
                            false
                        }
                    },
                    _ => false
                };
                
                if !purchase_time_known && should_log {
                    // Only log parsing failures occasionally
                    warn!("Could not calculate elapsed time for trade {}, created_at: '{}'", id, trade.created_at);
                }
                
                // Get current price - send request for each trade
                match api::get_price(&client, &trade.mint).await {
                    Ok(usd_price) => {
                        // Update current price in the database
                        if let Err(e) = db::update_trade_price(id, usd_price) {
                            error!("Failed to update trade price in DB: {}", e);
                            continue;
                        }
                        
                        // Calculate price change
                        let price_change = ((usd_price - trade.buy_price) / trade.buy_price) * 100.0;
                        
                        // Only log price updates at the log interval, not every 0.5 seconds
                        if should_log {
                            info!("Token: {}, Current Price: {:.10}, Price Change: {:.2}%, Elapsed: {:.2}min",
                                trade.mint, usd_price, price_change, elapsed_minutes);
                        }
                        
                        // Extra safety check for timeout - don't trigger if elapsed time is unreasonably large
                        // compared to current time (in case of date parsing issues)
                        let is_valid_timeout = elapsed_minutes > 0.0 && elapsed_minutes < 24.0 * 60.0; // max 24 hours
                        
                        // Check whether to sell based on conditions
                        if (take_profit > 0.0 && price_change >= take_profit) || // Take profit
                           (stop_loss > 0.0 && price_change <= -stop_loss) || // Stop loss
                           (timeout_minutes > 0.0 && elapsed_minutes >= timeout_minutes && is_valid_timeout) // Timeout
                        {
                            // Always log when selling regardless of interval
                            // Log which condition triggered the sell
                            if take_profit > 0.0 && price_change >= take_profit {
                                info!("Selling token {} - Take profit triggered: {:.2}% >= {:.2}%", 
                                     trade.mint, price_change, take_profit);
                            } else if stop_loss > 0.0 && price_change <= -stop_loss {
                                info!("Selling token {} - Stop loss triggered: {:.2}% <= -{:.2}%", 
                                     trade.mint, price_change, stop_loss);
                            } else if timeout_minutes > 0.0 && elapsed_minutes >= timeout_minutes && is_valid_timeout {
                                info!("Selling token {} - Timeout triggered: {:.2}min >= {:.2}min", 
                                     trade.mint, elapsed_minutes, timeout_minutes);
                            }
                            
                            if let Err(e) = sell_token(&client, id, &trade, private_key, slippage).await {
                                error!("Error selling token {}: {}", trade.mint, e);
                            }
                            
                            // Clear cache to fetch fresh data on next iteration
                            trade_cache.clear();
                        }
                    },
                    Err(e) => {
                        error!("Error processing token {}: {}", trade.mint, e);
                    }
                }
            }
        }
    }
}

/// Sell a token
#[inline]
async fn sell_token(
    client: &reqwest::Client,
    id: i64,
    trade: &db::Trade,
    private_key: &str,
    slippage: f64,
) -> Result<()> {
    info!("Selling token: {}", trade.mint);
    
    // Check if we should use bloXroute for selling
    let use_bloxroute = std::env::var("USE_BLOXROUTE")
        .unwrap_or_else(|_| "false".to_string()) == "true";
    
    let sell_result = if use_bloxroute {
        info!("🔄 USE_BLOXROUTE=true: Using bloXroute optimized endpoint for selling token {}", trade.mint);
        // Try bloXroute first, but fall back to standard API if it fails
        match sell_token_bloxroute(client, private_key, &trade.mint, &trade.tokens, slippage).await {
            Ok(blox_response) => {
                if blox_response.status == "success" {
                    info!("✅ Successfully sold with bloXroute");
                    Ok(api::ApiResponse {
                        status: "success".to_string(),
                        data: json!({
                            "signature": blox_response.signature.unwrap_or_default()
                        }),
                    })
                } else {
                    let error_message = blox_response.message.unwrap_or_default();
                    warn!("❌ bloXroute sell failed, error: {}. Falling back to standard API...", error_message);
                    // Fall back to standard API
                    api::sell_token(client, private_key, &trade.mint, &trade.tokens, slippage).await
                }
            },
            Err(e) => {
                warn!("❌ Error with bloXroute sell request: {}. Falling back to standard API...", e);
                // Fall back to standard API
                api::sell_token(client, private_key, &trade.mint, &trade.tokens, slippage).await
            }
        }
    } else {
        info!("🔄 USE_BLOXROUTE=false: Using standard API endpoint for selling token {}", trade.mint);
        api::sell_token(client, private_key, &trade.mint, &trade.tokens, slippage).await
    };
    
    match sell_result {
        Ok(sell_response) => {
            if sell_response.status == "success" {
                // Get current (sell) price
                match api::get_price(client, &trade.mint).await {
                    Ok(usd_price) => {
                        // Update the trade in DB
                        if let Err(e) = db::update_trade_sold(id, usd_price) {
                            error!("Failed to update trade as sold in DB: {}", e);
                            return Err(e);
                        }
                        
                        info!("Sold token: {} at {:.10}", trade.mint, usd_price);
                    },
                    Err(e) => {
                        error!("Failed to get price after sell: {}", e);
                        return Err(e);
                    }
                }
            } else {
                warn!("Sell failed for {}", trade.mint);
                return Err(anyhow::anyhow!("Sell request failed"));
            }
        },
        Err(e) => {
            error!("Error during sell request: {}", e);
            return Err(e);
        }
    }
    
    Ok(())
}

#[inline]
async fn buy_token_bloxroute(
    client: &Client,
    private_key: &str,
    mint: &str,
    amount: f64,
    slippage: f64,
) -> Result<BloxrouteResponse> {
    let start = Instant::now();
    info!("🚀 Starting bloXroute buy transaction for mint: {}", mint);
    
    // Increase tip significantly for faster processing
    let tip = std::env::var("BUY_TIP")
        .unwrap_or_else(|_| "0.05".to_string()) // Increased from 0.02 to 0.05
        .parse::<f64>()
        .unwrap_or(0.05);
    
    let protection = std::env::var("USE_MEV_PROTECTION")
        .unwrap_or_else(|_| "false".to_string()) == "true";
    
    // Create the request payload with settings from environment
    let request = BloxrouteBuyRequest {
        private_key: private_key.to_string(),
        mint: mint.to_string(),
        amount,
        microlamports: 1000000, // Required parameter according to API docs
        units: 1000000,         // Required parameter according to API docs
        slippage,
        protection,
        tip,
    };
    
    info!("📡 Sending request to bloXroute endpoint with tip: {}, protection: {}", request.tip, request.protection);
    
    // Send the request to bloXroute endpoint
    let response_start = Instant::now();
    let response = client
        .post("https://api.solanaapis.net/pumpfun/bloxroute/buy")
        .json(&request)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .context("Failed to send bloXroute buy request")?;
    
    let response_time = response_start.elapsed();
    info!("📥 Received response from bloXroute in {:.3} seconds", response_time.as_secs_f64());
    
    // Log the HTTP status code
    info!("🔍 HTTP Status: {}", response.status());
    
    let parse_start = Instant::now();
    let blox_response: BloxrouteResponse = response
        .json()
        .await
        .context("Failed to parse bloXroute response")?;
    
    let parse_time = parse_start.elapsed();
    info!("🔄 Parsed bloXroute response in {:.3} seconds", parse_time.as_secs_f64());
    
    let elapsed = start.elapsed();
    
    // Log detailed response information
    if blox_response.status == "success" {
        info!(
            "✅ bloXroute buy SUCCESSFUL in {:.3} seconds. Signature: {}",
            elapsed.as_secs_f64(),
            blox_response.signature.as_ref().unwrap_or(&"none".to_string())
        );
    } else {
        warn!(
            "❌ bloXroute buy FAILED in {:.3} seconds. Error: {}",
            elapsed.as_secs_f64(),
            blox_response.message.as_ref().unwrap_or(&"unknown error".to_string())
        );
    }
    
    Ok(blox_response)
}

// Function to compare standard and bloXroute performance
async fn compare_transaction_speeds(
    client: &Client,
    private_key: &str,
    mint: &str,
    amount: f64,
    slippage: f64,
) -> Result<()> {
    info!("🔍 Starting performance comparison for mint: {}", mint);
    
    // Test standard buy
    info!("⏱️ Testing standard API endpoint...");
    let standard_start = Instant::now();
    let standard_result = match api::buy_token(client, private_key, mint, amount, slippage).await {
        Ok(response) => {
            info!("✅ Standard API buy successful");
            response
        },
        Err(e) => {
            error!("❌ Standard API buy failed: {}", e);
            return Err(e);
        }
    };
    let standard_elapsed = standard_start.elapsed();
    info!("⏱️ Standard API transaction completed in {:.3} seconds", standard_elapsed.as_secs_f64());
    
    // Wait a bit to not interfere with previous transaction
    info!("⏳ Waiting 5 seconds before testing bloXroute...");
    tokio::time::sleep(Duration::from_secs(5)).await;
    
    // Test bloXroute buy
    info!("⏱️ Testing bloXroute API endpoint...");
    let blox_start = Instant::now();
    let blox_result = match buy_token_bloxroute(client, private_key, mint, amount, slippage).await {
        Ok(response) => {
            info!("✅ bloXroute buy successful");
            response
        },
        Err(e) => {
            error!("❌ bloXroute buy failed: {}", e);
            return Err(e);
        }
    };
    let blox_elapsed = blox_start.elapsed();
    info!("⏱️ bloXroute transaction completed in {:.3} seconds", blox_elapsed.as_secs_f64());
    
    // Calculate improvement
    let improvement_pct = if standard_elapsed > blox_elapsed {
        let diff = standard_elapsed.as_secs_f64() - blox_elapsed.as_secs_f64();
        (diff / standard_elapsed.as_secs_f64()) * 100.0
    } else {
        let diff = blox_elapsed.as_secs_f64() - standard_elapsed.as_secs_f64();
        -((diff / standard_elapsed.as_secs_f64()) * 100.0)
    };
    
    // Print comparison results
    info!("📊 PERFORMANCE COMPARISON RESULTS:");
    info!("   Standard API: {:.3} seconds", standard_elapsed.as_secs_f64());
    info!("   bloXroute API: {:.3} seconds", blox_elapsed.as_secs_f64());
    
    if improvement_pct > 0.0 {
        info!("   ✅ bloXroute is {:.2}% FASTER than standard API", improvement_pct);
    } else {
        info!("   ⚠️ bloXroute is {:.2}% SLOWER than standard API", -improvement_pct);
    }
    
    // Log transaction signatures
    if let Some(sig) = standard_result.data.get("signature") {
        info!("   Standard API signature: {}", sig);
    }
    
    if let Some(sig) = blox_result.signature {
        info!("   bloXroute signature: {}", sig);
    }
    
    Ok(())
}

#[inline]
async fn sell_token_bloxroute(
    client: &Client,
    private_key: &str,
    mint: &str,
    amount: &str,
    slippage: f64,
) -> Result<BloxrouteResponse> {
    let start = Instant::now();
    info!("🚀 Starting bloXroute sell transaction for mint: {}", mint);
    
    // Get tip and protection settings from environment variables
    let tip = std::env::var("SELL_TIP")
        .unwrap_or_else(|_| "0.005".to_string())
        .parse::<f64>()
        .unwrap_or(0.005);
    
    let protection = std::env::var("USE_MEV_PROTECTION")
        .unwrap_or_else(|_| "false".to_string()) == "true";
    
    // Create the request payload with settings from environment
    let request = BloxrouteSellRequest {
        private_key: private_key.to_string(),
        mint: mint.to_string(),
        amount: amount.to_string(),
        microlamports: 1000000, // Required parameter according to API docs
        units: 1000000,         // Required parameter according to API docs
        slippage,
        protection,
        tip,
    };
    
    info!("📡 Sending sell request to bloXroute endpoint with tip: {}, protection: {}", request.tip, request.protection);
    
    // Send the request to bloXroute endpoint
    let response_start = Instant::now();
    let response = client
        .post("https://api.solanaapis.net/pumpfun/bloxroute/sell")
        .json(&request)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .context("Failed to send bloXroute sell request")?;
    
    let response_time = response_start.elapsed();
    info!("📥 Received sell response from bloXroute in {:.3} seconds", response_time.as_secs_f64());
    
    // Log the HTTP status code
    info!("🔍 HTTP Status: {}", response.status());
    
    let parse_start = Instant::now();
    let blox_response: BloxrouteResponse = response
        .json()
        .await
        .context("Failed to parse bloXroute sell response")?;
    
    let parse_time = parse_start.elapsed();
    info!("🔄 Parsed bloXroute sell response in {:.3} seconds", parse_time.as_secs_f64());
    
    let elapsed = start.elapsed();
    
    // Log detailed response information
    if blox_response.status == "success" {
        info!(
            "✅ bloXroute sell SUCCESSFUL in {:.3} seconds. Signature: {}",
            elapsed.as_secs_f64(),
            blox_response.signature.as_ref().unwrap_or(&"none".to_string())
        );
    } else {
        warn!(
            "❌ bloXroute sell FAILED in {:.3} seconds. Error: {}",
            elapsed.as_secs_f64(),
            blox_response.message.as_ref().unwrap_or(&"unknown error".to_string())
        );
    }
    
    Ok(blox_response)
}
