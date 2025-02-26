mod api;
mod checks;
mod db;

use anyhow::{Context, Result};
use chrono::{NaiveDateTime, TimeZone, Utc};
use dotenv::dotenv;
use log::{info, error, warn, LevelFilter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time;

// Initialize environment logger
fn setup_logger() -> Result<()> {
    env_logger::Builder::new()
        .filter_level(LevelFilter::Info)
        .format_timestamp_millis()
        .init();
    
    Ok(())
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
    
    // Create an HTTP client
    let client = api::create_client();
    
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
        ).await {
            Ok(new_mint) => {
                if let Some(mint) = new_mint {
                    last_mint = Some(mint);
                    
                    // After processing a new token, check if we need to pause polling
                    let new_pending_count = match db::count_pending_trades() {
                        Ok(count) => count,
                        Err(_) => 0
                    };
                    
                    if new_pending_count >= max_positions && max_positions == 1 {
                        info!("Single position mode: Position filled. Pausing new token polling until position is closed.");
                    }
                }
            },
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
    
    let mint = &token_data.mint;
    
    // Check if the mint is the same as the last processed mint
    if let Some(last) = last_mint {
        if last == mint {
            // Mint is the same, skip processing
            return Ok(None);
        }
    }
    
    // Start a timer to measure processing speed
    let start_time = std::time::Instant::now();
    
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
    if check_min_liquidity {
        match checks::check_minimum_liquidity(client, &token_data.dev, mint).await {
            Ok(has_min_liquidity) => {
                if !has_min_liquidity {
                    warn!("Token does not meet minimum liquidity requirements, skipping buy");
                    return Ok(Some(mint.clone()));
                }
            },
            Err(e) => {
                error!("Error checking minimum liquidity: {}", e);
                return Ok(Some(mint.clone()));
            }
        }
    }
    
    // 4) Buy the token immediately if we got this far - URL check can happen in parallel while buying
    let buy_future = api::buy_token(client, private_key, mint, amount, slippage);
    
    // Perform URL check in parallel if needed
    let urls_check_future = async {
        if check_urls {
            match checks::check_urls(client, &token_data.metadata).await {
                Ok(urls_present) => urls_present,
                Err(_) => false
            }
        } else {
            true
        }
    };
    
    // Wait for both operations
    let (buy_result, urls_present) = tokio::join!(buy_future, urls_check_future);
    
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
                    || async { api::get_price(client, mint).await },
                    5, // Max retries
                    2000 // Delay in ms
                );
                
                let balance_future = api::retry_async(
                    || async { api::get_balance(client, wallet, mint).await },
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
                
                // Insert new row into the "trades" table
                if let Err(e) = db::insert_trade(mint, &tokens, usd_price) {
                    error!("Failed to insert trade into DB: {}", e);
                }
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
                            if should_log && (elapsed_minutes > 30.0 || elapsed_minutes < 0.0) {
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
    
    match api::sell_token(client, private_key, &trade.mint, &trade.tokens, slippage).await {
        Ok(sell_response) => {
            if sell_response.status == "success" {
                // Get current (sell) price
                match api::get_price(client, &trade.mint).await {
                    Ok(usd_price) => {
                        // Update the trade in DB
                        if let Err(e) = db::update_trade_sold(id, usd_price) {
                            error!("Failed to update trade as sold in DB: {}", e);
                            return Err(e.into());
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
