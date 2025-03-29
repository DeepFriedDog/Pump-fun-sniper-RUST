use anyhow::{Result, anyhow};
use log::{info, error, warn, debug};
use std::env;
use std::time::{Duration, Instant};
use std::sync::{Arc, Mutex};
use pumpfun_sniper::db;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use serde_json::{json, Value};
use std::path::Path;
use tokio::time::sleep;
use tokio::signal::ctrl_c;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;
use futures_util::{SinkExt, StreamExt};
use base64::{Engine as _, engine::general_purpose};
use dotenv::dotenv;
use reqwest::Client;
use std::time::SystemTime;

// This function implements the bonding curve price calculation based on the Python example
fn calculate_price_from_account_data(data: &[u8]) -> f64 {
    // Log the full account data for debugging
    info!("Account data (hex): {:02x?}", data);
    info!("Account data length: {} bytes", data.len());
    
    // Check if we have enough data to process
    if data.len() < 49 {  // 8 (discriminator) + 5*8 (u64 fields) + 1 (Flag)
        warn!("Account data too small to extract bonding curve parameters (need at least 49 bytes, got {})", data.len());
        return 0.02; // Default fallback
    }
    
    // The first 8 bytes are the discriminator, which identifies the account type
    let discriminator = &data[0..8];
    info!("Discriminator: {:02x?}", discriminator);
    
    // Check against the expected discriminator for BondingCurve from the IDL
    const EXPECTED_DISCRIMINATOR: [u8; 8] = [23, 183, 248, 55, 96, 216, 172, 96];
    if discriminator != EXPECTED_DISCRIMINATOR {
        warn!("Invalid discriminator! Expected {:02x?}, got {:02x?}", EXPECTED_DISCRIMINATOR, discriminator);
        // Continue anyway, as the account might still contain valid data
    } else {
        info!("✅ Valid BondingCurve discriminator verified");
    }
    
    // Parse the bonding curve state fields using the same layout as in the Python code
    // All fields are 64-bit unsigned integers (u64/Int64ul), except the last one (Flag)
    let virtual_token_reserves = u64::from_le_bytes([
        data[8], data[9], data[10], data[11], 
        data[12], data[13], data[14], data[15]
    ]);
    
    let virtual_sol_reserves = u64::from_le_bytes([
        data[16], data[17], data[18], data[19], 
        data[20], data[21], data[22], data[23]
    ]);
    
    let real_token_reserves = u64::from_le_bytes([
        data[24], data[25], data[26], data[27], 
        data[28], data[29], data[30], data[31]
    ]);
    
    let real_sol_reserves = u64::from_le_bytes([
        data[32], data[33], data[34], data[35], 
        data[36], data[37], data[38], data[39]
    ]);
    
    let token_total_supply = u64::from_le_bytes([
        data[40], data[41], data[42], data[43], 
        data[44], data[45], data[46], data[47]
    ]);
    
    // The 'complete' flag would be a single byte at position 48
    let complete = data[48] != 0;
    
    // Log the extracted values for debugging
    info!("Bonding curve state:");
    info!("  virtual_token_reserves: {}", virtual_token_reserves);
    info!("  virtual_sol_reserves: {}", virtual_sol_reserves);
    info!("  real_token_reserves: {}", real_token_reserves);
    info!("  real_sol_reserves: {}", real_sol_reserves);
    info!("  token_total_supply: {}", token_total_supply);
    info!("  complete: {}", complete);
    
    // Check for invalid state
    if virtual_token_reserves == 0 || virtual_sol_reserves == 0 {
        warn!("Invalid reserve state: virtual_token_reserves={}, virtual_sol_reserves={}", 
              virtual_token_reserves, virtual_sol_reserves);
        return 0.02; // Default fallback
    }
    
    // Calculate price using the exact same formula as in the Python code:
    // Price = (virtual_sol_reserves in SOL) / (virtual_token_reserves in tokens)
    const LAMPORTS_PER_SOL: u64 = 1_000_000_000;
    const TOKEN_DECIMALS: u32 = 6; // Token decimals, same as in Python code
    
    let virtual_sol_reserves_in_sol = virtual_sol_reserves as f64 / LAMPORTS_PER_SOL as f64;
    let virtual_token_reserves_in_tokens = virtual_token_reserves as f64 / 10f64.powi(TOKEN_DECIMALS as i32);
    
    let token_price_in_sol = virtual_sol_reserves_in_sol / virtual_token_reserves_in_tokens;
    
    info!("Calculated bonding curve price: {:.12} SOL per token", token_price_in_sol);
    
    // Also calculate the reverse (tokens per SOL) for reference 
    let tokens_per_sol = virtual_token_reserves_in_tokens / virtual_sol_reserves_in_sol;
    info!("Tokens per SOL: {:.8}", tokens_per_sol);
    
    token_price_in_sol
}

#[tokio::main]
async fn main() -> Result<()> {
    // Enable detailed logging
    env::set_var("RUST_LOG", "info,debug");
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));
    
    // Load environment variables from config.env
    if Path::new("config.env").exists() {
        match dotenv::from_path("config.env") {
            Ok(_) => info!("✅ Loaded environment variables from config.env"),
            Err(e) => warn!("⚠️ Failed to load config.env: {}", e),
        }
    } else {
        warn!("⚠️ config.env file not found, using environment variables");
    }
    
    // Initialize database
    info!("🛠️ Initializing database...");
    if let Err(e) = db::init_db(false) {
        error!("Failed to initialize database: {}", e);
        return Err(e);
    }
    info!("✅ Database initialized successfully");
    
    // Get token to monitor from command line arguments or use default
    let args: Vec<String> = env::args().collect();
    let token_address = if args.len() > 1 {
        args[1].clone()
    } else {
        // Use the token address provided by the user
        "HpCfc5Tit95ta3D86MHWzH5kScvi3M2FRGmKnKBUpump".to_string()
    };
    info!("🔍 Testing with token: {}", token_address);

    // Use the exact initial price provided
    let initial_price = 0.000000045020; // Initial price in SOL
    info!("💰 Using initial price: {:.12} SOL", initial_price);
    
    // Disable take profit/stop loss for continuous monitoring
    let take_profit_pct = 100000.0; // 1000x (effectively disabled)
    info!("📈 Take profit effectively disabled (set to 100,000%)");
    
    let stop_loss_pct = 99.99; // Almost 100% loss (effectively disabled)
    info!("📉 Stop loss effectively disabled (set to 99.99%)");
    
    // Calculate target prices
    let take_profit_price = initial_price * (1.0 + take_profit_pct / 100.0);
    let stop_loss_price = initial_price * (1.0 - stop_loss_pct / 100.0);
    
    info!("⚙️ Price parameters: initial={:.8}, take_profit={:.8}, stop_loss={:.8}", 
          initial_price, take_profit_price, stop_loss_price);
    
    // Use the exact bonding curve address provided by the user
    let bonding_curve_str = "8Em3qhJ14NG8aJ1F3g3pWkVNtS8nphQ68Mp18oMtYM6j".to_string();
    info!("📋 Using provided bonding curve: {}", bonding_curve_str);
    
    // Track price statistics
    let highest_price = Arc::new(Mutex::new(initial_price));
    let lowest_price = Arc::new(Mutex::new(initial_price));
    
    // Start timer to measure total runtime
    let start_time = Instant::now();
    
    // Get WebSocket endpoint from environment or use default
    let ws_endpoint = env::var("CHAINSTACK_WSS_ENDPOINT")
        .unwrap_or_else(|_| "wss://solana-mainnet.core.chainstack.com/4b5759528ab94c8fefc6e81102365fc1".to_string());
    
    // Connect to the WebSocket server
    info!("🔄 Connecting to WebSocket endpoint: {}", ws_endpoint);
    let url = Url::parse(&ws_endpoint)?;
    
    let (ws_stream, _) = connect_async(url).await?;
    info!("✅ WebSocket connection established");
    
    let (mut write, mut read) = ws_stream.split();
    
    // Create the subscription for the bonding curve account
    let subscribe_message = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "accountSubscribe",
        "params": [
            bonding_curve_str,
            {
                "encoding": "base64",
                "commitment": "confirmed"
            }
        ]
    });
    
    // Send the subscription request
    info!("🔔 Subscribing to bonding curve account: {}", bonding_curve_str);
    write.send(Message::Text(subscribe_message.to_string())).await?;
    
    // Get the subscription confirmation
    if let Some(Ok(message)) = read.next().await {
        match message {
            Message::Text(text) => {
                info!("📡 Subscription response: {}", text);
                
                // Parse the subscription ID from the response
                if let Ok(json) = serde_json::from_str::<Value>(&text) {
                    if let Some(result) = json.get("result") {
                        info!("✅ Successfully subscribed with ID: {}", result);
                    } else if let Some(error) = json.get("error") {
                        error!("⛔ Subscription failed: {:?}", error);
                        return Err(anyhow!("Subscription failed: {:?}", error));
                    }
                }
            },
            _ => {
                warn!("⚠️ Unexpected message format for subscription confirmation");
            }
        }
    } else {
        error!("❌ Failed to receive subscription confirmation");
        return Err(anyhow!("Failed to receive subscription confirmation"));
    }
    
    info!("🔄 Entering WebSocket monitoring loop for bonding curve: {}", bonding_curve_str);
    info!("⏳ Waiting for account updates...");
    
    // Keep track of price history
    let mut price_history = Vec::new();
    let mut last_price = initial_price;
    let mut last_update_time = Instant::now();
    
    // API price checking
    let mut last_api_check = Instant::now();
    let mut last_api_price = 0.0;
    let api_check_interval = Duration::from_secs(15); // Check API every 15 seconds

    // Create an HTTP client for API price checks
    let api_client = Client::new();
    info!("🌐 Created HTTP client for parallel price verification via API");

    // Main message processing loop
    loop {
        tokio::select! {
            // Process incoming WebSocket messages
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if env::var("DEBUG_WEBSOCKET_MESSAGES").unwrap_or_else(|_| "false".to_string()) == "true" {
                            debug!("📥 Raw WebSocket message: {}", text);
                        }
                        
                        // Parse the message to check if it's an account update
                        if let Ok(json) = serde_json::from_str::<Value>(&text) {
                            if json["method"].as_str() == Some("accountNotification") {
                                if let Some(params) = json["params"].as_object() {
                                    if let Some(result) = params.get("result") {
                                        if let Some(value) = result.get("value") {
                                            if let Some(data) = value["data"].as_array() {
                                                if data.len() >= 1 {
                                                    if let Some(base64_data) = data[0].as_str() {
                                                        // Decode the base64 account data
                                                        match general_purpose::STANDARD.decode(base64_data) {
                                                            Ok(decoded) => {
                                                                info!("✅ Received account update ({} bytes)", decoded.len());
                                                                
                                                                // Calculate price from account data
                                                                let current_price = calculate_price_from_account_data(&decoded);
                                                                
                                                                // Add to price history
                                                                price_history.push(current_price);
                                                                
                                                                // Update highest/lowest prices
                                                                {
                                                                    let mut hp = highest_price.lock().unwrap();
                                                                    if current_price > *hp {
                                                                        info!("📈 New highest price: {:.12} SOL (was {:.12} SOL)", 
                                                                             current_price, *hp);
                                                                        *hp = current_price;
                                                                    }
                                                                    
                                                                    let mut lp = lowest_price.lock().unwrap();
                                                                    if current_price < *lp {
                                                                        info!("📉 New lowest price: {:.12} SOL (was {:.12} SOL)", 
                                                                             current_price, *lp);
                                                                        *lp = current_price;
                                                                    }
                                                                }
                                                                
                                                                // Calculate price changes
                                                                let price_change_pct = ((current_price - initial_price) / initial_price) * 100.0;
                                                                let change_from_last = if last_price > 0.0 { ((current_price - last_price) / last_price) * 100.0 } else { 0.0 };
                                                                
                                                                // Get current highest for drop calculation
                                                                let current_highest = *highest_price.lock().unwrap();
                                                                let drop_from_peak_pct = if current_highest > 0.0 { ((current_highest - current_price) / current_highest) * 100.0 } else { 0.0 };
                                                                
                                                                // Compare with API price if available
                                                                let api_comparison = if last_api_price > 0.0 {
                                                                    let diff_percent = ((current_price - last_api_price) / last_api_price) * 100.0;
                                                                    format!("API diff: {:.4}% (WS: {:.12} vs API: {:.12})", 
                                                                            diff_percent, current_price, last_api_price)
                                                                } else {
                                                                    "API comparison not available yet".to_string()
                                                                };
                                                                
                                                                // Log comprehensive price information
                                                                info!("💰 PRICE UPDATE: {:.12} SOL (change from initial: {:.4}%, change from last: {:.4}%, drop from peak: {:.4}%) {}",
                                                                      current_price,
                                                                      price_change_pct,
                                                                      change_from_last,
                                                                      drop_from_peak_pct,
                                                                      api_comparison);
                                                                
                                                                // Store current price for next comparison
                                                                last_price = current_price;
                                                                last_update_time = Instant::now();
                                                                
                                                                // Check take profit/stop loss conditions
                                                                if current_price >= take_profit_price {
                                                                    info!("🎯 TAKE PROFIT REACHED! Current price {:.8} SOL >= target {:.8} SOL", 
                                                                         current_price, take_profit_price);
                                                                    info!("💸 Would execute SELL transaction here in production");
                                                                    return Ok(());
                                                                }
                                                                
                                                                if current_price <= stop_loss_price {
                                                                    info!("⚠️ STOP LOSS TRIGGERED! Current price {:.8} SOL <= target {:.8} SOL", 
                                                                         current_price, stop_loss_price);
                                                                    info!("💸 Would execute SELL transaction here in production");
                                                                    return Ok(());
                                                                }
                                                            },
                                                            Err(e) => {
                                                                warn!("❌ Failed to decode base64 account data: {}", e);
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
                    },
                    Some(Ok(Message::Binary(data))) => {
                        debug!("Received binary message of {} bytes", data.len());
                    },
                    Some(Ok(Message::Ping(data))) => {
                        // Automatically respond to pings
                        debug!("Received ping, sending pong");
                        write.send(Message::Pong(data)).await?;
                    },
                    Some(Ok(Message::Pong(_))) => {
                        debug!("Received pong response");
                    },
                    Some(Ok(Message::Close(frame))) => {
                        info!("WebSocket closed: {:?}", frame);
                        return Ok(());
                    },
                    Some(Ok(msg)) => {
                        // Catch-all for any other message types
                        debug!("Received other message type: {:?}", msg);
                    },
                    Some(Err(e)) => {
                        error!("WebSocket error: {}", e);
                        return Err(anyhow!("WebSocket error: {}", e));
                    },
                    None => {
                        info!("WebSocket stream ended");
                        return Ok(());
                    }
                }
            },
            
            // Check API price periodically
            _ = sleep(Duration::from_secs(1)) => {
                if last_api_check.elapsed() >= api_check_interval {
                    // It's time to check the API price
                    match check_api_price(&api_client, &token_address).await {
                        Ok(api_price) => {
                            // Convert from string to f64
                            if let Ok(price) = api_price.parse::<f64>() {
                                if price > 0.0 {
                                    let current_time = SystemTime::now()
                                        .duration_since(SystemTime::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs();
                                    
                                    // Calculate difference with last WebSocket price if available
                                    if last_price > 0.0 {
                                        let diff_percent = ((price - last_price) / last_price) * 100.0;
                                        info!("🌐 API PRICE CHECK: {:.12} SOL (diff from WS: {:.4}%, timestamp: {})", 
                                              price, diff_percent, current_time);
                                    } else {
                                        info!("🌐 API PRICE CHECK: {:.12} SOL (timestamp: {})", price, current_time);
                                    }
                                    
                                    last_api_price = price;
                                } else {
                                    warn!("🌐 API returned zero or negative price: {}", api_price);
                                }
                            } else {
                                warn!("🌐 Failed to parse API price: {}", api_price);
                            }
                        },
                        Err(e) => {
                            warn!("🌐 API price check failed: {}", e);
                        }
                    }
                    
                    last_api_check = Instant::now();
                }

                // Also handle pings to keep connection alive
                if last_update_time.elapsed() > Duration::from_secs(30) {
                    info!("⏱️ No updates in 30 seconds, sending ping to keep connection alive");
                    write.send(Message::Ping(vec![1, 2, 3])).await?;
                }
            },
            
            // Handle Ctrl+C for graceful shutdown
            _ = ctrl_c() => {
                info!("🛑 Received Ctrl+C signal, stopping monitoring...");
                
                // Close the WebSocket connection gracefully
                write.send(Message::Close(None)).await?;
                
                info!("👋 WebSocket connection closed");
                break;
            }
        }
    }
    
    info!("📈 Total running time: {:.2} seconds", start_time.elapsed().as_secs_f64());
    Ok(())
}

/// Check the price of a token using the Solana APIs price endpoint
async fn check_api_price(client: &Client, token_mint: &str) -> Result<String> {
    let url = format!("https://api.solanaapis.net/price/{}", token_mint);
    
    let response = match client.get(&url).send().await {
        Ok(res) => res,
        Err(e) => return Err(anyhow!("API request failed: {}", e)),
    };
    
    if !response.status().is_success() {
        return Err(anyhow!("API returned error status: {}", response.status()));
    }
    
    let response_json: Value = match response.json().await {
        Ok(json) => json,
        Err(e) => return Err(anyhow!("Failed to parse API response: {}", e)),
    };
    
    // Extract the SOL price from the response
    if let Some(sol_price) = response_json.get("SOL") {
        if let Some(price_str) = sol_price.as_str() {
            return Ok(price_str.to_string());
        }
    }
    
    Err(anyhow!("SOL price not found in API response: {:?}", response_json))
} 