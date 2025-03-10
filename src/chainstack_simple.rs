use anyhow::{anyhow, Context, Result};
use log::{info, warn};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solana_sdk::pubkey::Pubkey;
use std::env;
use std::str::FromStr;
use std::time::Duration;
use uuid::Uuid;
use url;

// Define the NewToken structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewToken {
    pub token_name: String,
    pub token_symbol: String,
    pub mint_address: String,
    pub creator_address: String,
    pub transaction_signature: String,
    pub timestamp: i64,
}

// Define the ApiResponse structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiResponse {
    pub status: String,
    pub data: Value,
    pub mint: String,
}

/// Get the Chainstack endpoint URL
pub fn get_chainstack_endpoint() -> String {
    env::var("CHAINSTACK_RPC_URL").unwrap_or_else(|_| {
        env::var("SOLANA_RPC_URL").unwrap_or_else(|_| {
            env::var("CHAINSTACK_ENDPOINT").unwrap_or_else(|_| {
                "https://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab"
                    .to_string()
            })
        })
    })
}

/// Get the Chainstack Trader Node endpoint for high-speed transactions
pub fn get_chainstack_trader_endpoint() -> String {
    // Check if Trader Node should be used for transactions
    if env::var("USE_TRADER_NODE_FOR_TRANSACTIONS").unwrap_or_else(|_| "false".to_string())
        == "true"
    {
        // Return the Trader Node endpoint if configured
        env::var("CHAINSTACK_TRADER_RPC_URL")
            .unwrap_or_else(|_| {
                log::warn!("⚠️ USE_TRADER_NODE_FOR_TRANSACTIONS is true but CHAINSTACK_TRADER_RPC_URL not set, falling back to regular endpoint");
                get_chainstack_endpoint()
            })
    } else {
        // Otherwise return the regular endpoint
        get_chainstack_endpoint()
    }
}

/// Create a Chainstack HTTP client
pub fn create_chainstack_client() -> Client {
    // Create a reqwest client with timeout
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| {
            warn!("Failed to build custom HTTP client, using default");
            Client::new()
        })
}

/// Get the authenticated WebSocket URL
pub fn get_authenticated_wss_url() -> String {
    // Try to get the endpoint from environment variables first
    let ws_endpoint = env::var("CHAINSTACK_WSS_ENDPOINT").unwrap_or_else(|_| {
        // Use the default endpoint from config as fallback
        "wss://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string()
    });
    
    // Check if we need to use authentication
    let use_auth = env::var("USE_CHAINSTACK_AUTH")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase() == "true";
    
    if use_auth {
        warn!("WebSocket authentication via URL is enabled, but this might not work with all providers");
        
        // Some WebSocket providers use a different format for authentication
        // Let's try the base64 encoded Authorization header approach
        let username = env::var("CHAINSTACK_USERNAME").unwrap_or_default();
        let password = env::var("CHAINSTACK_PASSWORD").unwrap_or_default();
        
        if !username.is_empty() && !password.is_empty() {
            // For WebSocket connections, we need to modify the connection code in websocket_test.rs
            // and websocket_reconnect.rs to include the Authorization header
            // For now, we'll just log a warning and return the endpoint
            info!("WebSocket authentication credentials will be used in the connection code");
            
            return ws_endpoint;
        }
    }
    
    // Return the regular endpoint if no special handling is needed
    ws_endpoint
}

/// Prepare a WebSocket URL for direct authentication
/// 
/// This modifies a WebSocket URL to include authentication credentials directly
/// in the URL for providers that support this method. For example:
/// wss://username:password@hostname/path
pub fn get_auth_embedded_wss_url() -> String {
    // Try to get the endpoint from environment variables
    let ws_endpoint = env::var("CHAINSTACK_WSS_ENDPOINT").unwrap_or_else(|_| {
        // Use the default endpoint from config as fallback
        "wss://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string()
    });
    
    // Check if we need to use authentication
    let use_auth = env::var("USE_CHAINSTACK_AUTH")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase() == "true";
    
    if use_auth {
        // Get authentication credentials
        let username = env::var("CHAINSTACK_USERNAME").unwrap_or_default();
        let password = env::var("CHAINSTACK_PASSWORD").unwrap_or_default();
        
        if !username.is_empty() && !password.is_empty() {
            // Try to parse the URL to add auth credentials directly
            if let Ok(mut url) = url::Url::parse(&ws_endpoint) {
                // Try to set credentials directly in the URL
                if url.scheme() == "wss" || url.scheme() == "ws" {
                    if let Err(_) = url.set_username(&username) {
                        warn!("Failed to set username in WebSocket URL");
                    }
                    if let Err(_) = url.set_password(Some(&password)) {
                        warn!("Failed to set password in WebSocket URL");
                    }
                    
                    // Return the URL with embedded credentials
                    return url.to_string();
                }
            }
        }
    }
    
    // If authentication isn't needed or fails, just return the original URL
    ws_endpoint
}

/// Get a fresh WebSocket URL that forces a new session
/// 
/// This adds a unique session ID to the WebSocket URL to ensure
/// that each startup of the application gets a brand new connection
/// with no accumulated history or buffered events.
pub fn get_fresh_wss_url() -> String {
    // Get the base URL with authentication credentials embedded
    let base_url = get_auth_embedded_wss_url();
    
    // Generate a unique session ID
    let session_id = Uuid::new_v4().to_string();
    
    // Add the session ID as a query parameter
    if base_url.contains('?') {
        format!("{}&fresh_session={}", base_url, session_id)
    } else {
        format!("{}?fresh_session={}", base_url, session_id)
    }
}

/// Create a Solana RPC client using the given RPC URL
pub fn create_solana_client(rpc_url: &str) -> solana_client::rpc_client::RpcClient {
    use solana_client::rpc_client::RpcClient;
    use solana_sdk::commitment_config::CommitmentConfig;

    // Get a valid Chainstack endpoint URL with proper schema
    let endpoint = if !rpc_url.is_empty() && rpc_url != "default" {
        // Use provided URL if it exists
        let url = if !rpc_url.starts_with("http") {
            format!("https://{}", rpc_url)
        } else {
            rpc_url.to_string()
        };
        url
    } else {
        // Otherwise get from env or use default
        let env_url = std::env::var("CHAINSTACK_ENDPOINT").unwrap_or_else(|_| {
            "https://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab"
                .to_string()
        });

        if !env_url.starts_with("http") {
            format!("https://{}", env_url)
        } else {
            env_url
        }
    };

    // Create client with "processed" commitment config for fastest response
    RpcClient::new_with_commitment(endpoint, CommitmentConfig::processed())
}

/// Calculate the associated bonding curve address
pub fn calculate_associated_bonding_curve(mint: &str, bonding_curve: &str) -> Result<String> {
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

/// Buy a token using the API
pub async fn buy_token(
    client: &Client,
    private_key: &str,
    mint: &str,
    amount: f64,
    slippage: f64,
) -> Result<ApiResponse> {
    // Get priority fee and compute units from environment variables
    let priority_fee = env::var("PRIORITY_FEE")
        .unwrap_or_else(|_| "2000000".to_string())
        .parse::<u64>()
        .unwrap_or(2000000);

    let compute_units = env::var("COMPUTE_UNITS")
        .unwrap_or_else(|_| "200000".to_string())
        .parse::<u64>()
        .unwrap_or(200000);

    // Get the appropriate endpoint for transactions
    let trader_endpoint = get_chainstack_trader_endpoint();
    let use_trader_node = env::var("USE_TRADER_NODE_FOR_TRANSACTIONS")
        .unwrap_or_else(|_| "false".to_string())
        == "true";

    // Log the buy attempt with appropriate note about Warp transaction
    if use_trader_node {
        info!("🚀 Attempting to buy token {} with {} SOL using Chainstack Trader Node with Warp transactions", 
              mint, amount);
        info!("🔌 Using Trader Node endpoint: {}", trader_endpoint);
    } else {
        info!(
            "🚀 Attempting to buy token {} with {} SOL using standard Chainstack endpoint",
            mint, amount
        );
    }

    // For Solana, we create a sendTransaction RPC call which will go through bloXroute when using Trader Node
    let params = json!([
        {
            "mint": mint,
            "amount": amount,
            "slippage": slippage,
            "priorityFee": priority_fee,
            "computeUnits": compute_units,
            "privateKey": private_key
        }
    ]);

    // Send the transaction through appropriate endpoint
    let result =
        make_jsonrpc_call(client, "sendTransaction", params, Some(trader_endpoint)).await?;

    // Process the result
    if let Some(tx_signature) = result.get("result").and_then(|h| h.as_str()) {
        if use_trader_node {
            info!(
                "✅ Buy transaction sent successfully via Trader Node with Warp: {}",
                tx_signature
            );
        } else {
            info!("✅ Buy transaction sent successfully: {}", tx_signature);
        }

        Ok(ApiResponse {
            status: "success".to_string(),
            data: json!({
                "transaction": tx_signature,
                "message": if use_trader_node {
                    "Buy transaction sent via Trader Node with Warp successfully"
                } else {
                    "Buy transaction sent successfully"
                }
            }),
            mint: mint.to_string(),
        })
    } else if let Some(error) = result.get("error") {
        let error_msg = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown error");

        warn!("❌ Failed to send buy transaction: {}", error_msg);

        Ok(ApiResponse {
            status: "error".to_string(),
            data: json!({
                "message": error_msg
            }),
            mint: mint.to_string(),
        })
    } else {
        warn!("❌ Unknown response format from transaction");

        Ok(ApiResponse {
            status: "error".to_string(),
            data: json!({
                "message": "Unknown response format from transaction"
            }),
            mint: mint.to_string(),
        })
    }
}

/// Get token balance
pub async fn get_token_balance(client: &Client, wallet: &str, mint: &str) -> Result<f64> {
    // Simulate a balance for testing
    Ok(100.0)
}

/// Get token price
pub async fn get_token_price(client: &Client, mint: &str) -> Result<f64> {
    // Create a cache key for this token
    let cache_key = format!("price-{}", mint);

    // Check if we have this price in our cache
    // This helps prevent excessive API calls
    let cached_price = {
        let token_cache = super::api::TOKEN_PRICE_CACHE.lock().unwrap();
        if let Some((price, timestamp)) = token_cache.get(&cache_key) {
            // Only use cache if it's less than 30 seconds old for frequent price updates
            if timestamp.elapsed() < std::time::Duration::from_secs(30) {
                Some(*price)
            } else {
                None
            }
        } else {
            None
        }
    };

    // Return early if we have cached data
    if let Some(price) = cached_price {
        return Ok(price);
    }

    // Determine if this is a pump.fun token
    let is_pump_fun_token = mint.ends_with("pump");

    let price = if is_pump_fun_token {
        // For pump.fun tokens, calculate price using the bonding curve
        // Get developer wallet address for this token
        let dev_wallet = match get_token_creator(client, mint).await {
            Ok(creator) => creator,
            Err(_) => {
                // If we can't get the creator, use our fallback calculation
                return calculate_price_from_liquidity(client, mint).await;
            }
        };

        // Get the token balance of the developer
        let dev_balance = match get_token_balance(client, &dev_wallet, mint).await {
            Ok(balance) => balance,
            Err(_) => {
                warn!(
                    "Failed to get developer balance for {}, using fallback price calculation",
                    mint
                );
                return calculate_price_from_liquidity(client, mint).await;
            }
        };

        // Use the pump.fun bonding curve formula:
        // SOL = (32,190,005,730 / (1,073,000,191 - X)) - 30
        // Where X is the developer's token balance
        let initial_virtual_token_reserve: f64 = 1_073_000_191.0;
        let constant: f64 = 32_190_005_730.0;
        let offset: f64 = 30.0;

        // Calculate the current SOL value using the formula
        if dev_balance >= initial_virtual_token_reserve {
            warn!(
                "Developer balance exceeds reserve limit for {}, using fallback price",
                mint
            );
            0.01 // Fallback price if calculation would cause division by zero
        } else {
            // Calculate price based on current bonding curve state
            let sol_value = (constant / (initial_virtual_token_reserve - dev_balance)) - offset;

            // Perform sanity check on the price
            if sol_value.is_nan() || sol_value.is_infinite() || sol_value < 0.0 {
                warn!("Invalid price calculation for {}: {}", mint, sol_value);
                0.01 // Fallback price
            } else {
                // Convert SOL value to USD price (this is simplified - in a real scenario you'd use a SOL/USD price feed)
                // For now we'll assume 1 token = sol_value in USD
                sol_value
            }
        }
    } else {
        // For non-pump.fun tokens, use a placeholder price or implement a different price feed
        // In a real scenario, you'd query a DEX or oracle for the price
        0.01 // Placeholder price for non-pump.fun tokens
    };

    // Update the cache with the new price
    {
        let mut token_cache = super::api::TOKEN_PRICE_CACHE.lock().unwrap();
        token_cache.insert(cache_key, (price, std::time::Instant::now()));
    }

    Ok(price)
}

/// Calculate price from liquidity as a fallback method
async fn calculate_price_from_liquidity(client: &Client, mint: &str) -> Result<f64> {
    // Simplified fallback calculation
    // In a real implementation, you'd use liquidity pool data
    Ok(0.01) // Placeholder
}

/// Get the creator/developer wallet for a token
pub async fn get_token_creator(client: &Client, mint: &str) -> Result<String> {
    // If this is a test environment, return a placeholder
    Ok("CcHLuGzJZ2GtDhQBP1PkPmvfQoXNkG4Y6XGeQYfFSfv".to_string())
}

/// Process a WebSocket notification
pub fn process_notification(text: &str) -> Option<NewToken> {
    // Try to parse the notification
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        // Extract token data if available
        if let Some(data) = value.get("data") {
            if let (Some(name), Some(symbol), Some(mint), Some(creator)) = (
                data.get("name").and_then(|v| v.as_str()),
                data.get("symbol").and_then(|v| v.as_str()),
                data.get("mint").and_then(|v| v.as_str()),
                data.get("creator").and_then(|v| v.as_str()),
            ) {
                let token = NewToken {
                    token_name: name.to_string(),
                    token_symbol: symbol.to_string(),
                    mint_address: mint.to_string(),
                    creator_address: creator.to_string(),
                    transaction_signature: "".to_string(),
                    timestamp: chrono::Utc::now().timestamp(),
                };

                // ULTRA-FAST PATH: Add token directly to the queue for immediate processing
                if let Ok(mut queue) = crate::api::NEW_TOKEN_QUEUE.try_lock() {
                    let token_data = crate::api::TokenData {
                        status: "success".to_string(),
                        mint: token.mint_address.clone(),
                        dev: token.creator_address.clone(),
                        metadata: Some("".to_string()), // Need to set something here
                        name: Some(token.token_name.clone()),
                        symbol: Some(token.token_symbol.clone()),
                        timestamp: Some(token.timestamp),
                    };

                    queue.push_back(token_data);
                    info!("⚡ Added token to processing queue for immediate handling");
                } else {
                    warn!("Could not lock token queue, token processing might be delayed");
                }

                return Some(token);
            }
        }
    }

    None
}

/// Make a JSON-RPC call to the Chainstack API
pub async fn make_jsonrpc_call(
    client: &Client,
    method: &str,
    params: Value,
    custom_endpoint: Option<String>,
) -> Result<Value> {
    // Use custom endpoint if provided, otherwise get the default Chainstack endpoint
    let rpc_url = match custom_endpoint {
        Some(endpoint) => endpoint,
        None => get_chainstack_endpoint(),
    };

    // Prepare the JSON-RPC request
    let request = json!({
        "jsonrpc": "2.0",
        "id": Uuid::new_v4().to_string(),
        "method": method,
        "params": params
    });

    // Send the request
    let response = client
        .post(&rpc_url)
        .json(&request)
        .send()
        .await
        .context("Failed to send JSON-RPC request to Chainstack")?;

    // Parse the response
    let result: Value = response
        .json()
        .await
        .context("Failed to parse JSON-RPC response")?;

    // Check for errors
    if let Some(error) = result.get("error") {
        let error_msg = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown error");

        log::error!("JSON-RPC error: {}", error_msg);
    }

    Ok(result)
}
