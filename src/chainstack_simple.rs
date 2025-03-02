use anyhow::{anyhow, Result};
use log::{info, warn};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::time::Duration;
use std::env;

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
    // Try to get the endpoint from environment variables first
    env::var("CHAINSTACK_RPC_ENDPOINT")
        .unwrap_or_else(|_| {
            // Use the default endpoint from config as fallback
            "https://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string()
        })
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
    env::var("CHAINSTACK_WSS_ENDPOINT")
        .unwrap_or_else(|_| {
            // Use the default endpoint from config as fallback
            "wss://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string()
        })
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
        let env_url = std::env::var("CHAINSTACK_ENDPOINT")
            .unwrap_or_else(|_| "https://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string());
            
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
    // Simulate a successful buy for testing
    let response = ApiResponse {
        status: "success".to_string(),
        data: json!({
            "transaction": "simulated_tx_hash",
            "message": "Simulated buy successful"
        }),
        mint: mint.to_string(),
    };
    
    Ok(response)
}

/// Get token balance
pub async fn get_token_balance(
    client: &Client,
    wallet: &str,
    mint: &str,
) -> Result<f64> {
    // Simulate a balance for testing
    Ok(100.0)
}

/// Get token price
pub async fn get_token_price(
    client: &Client,
    mint: &str,
) -> Result<f64> {
    // Simulate a price for testing
    Ok(0.01)
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
                return Some(NewToken {
                    token_name: name.to_string(),
                    token_symbol: symbol.to_string(),
                    mint_address: mint.to_string(),
                    creator_address: creator.to_string(),
                    transaction_signature: "".to_string(),
                    timestamp: chrono::Utc::now().timestamp(),
                });
            }
        }
    }
    
    None
} 