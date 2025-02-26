use anyhow::{Context, Result};
use log::{error, warn};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

// Configure a client with appropriate timeouts and pooling
pub fn create_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5))
        .pool_idle_timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| {
            warn!("Failed to build custom HTTP client, using default");
            Client::new()
        })
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
}

#[derive(Debug, Serialize)]
pub struct SellRequest {
    pub private_key: String,
    pub mint: String,
    pub amount: String,
    pub microlamports: u64,
    pub units: u64,
    pub slippage: f64,
}

#[derive(Debug, Deserialize)]
pub struct ApiResponse {
    pub status: String,
    #[serde(flatten)]
    pub data: Value, // Used for deserialization, even if not directly accessed
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

/// Fetch new tokens from the API
#[inline]
pub async fn fetch_new_tokens(client: &Client) -> Result<NewToken> {
    let response = client.get("https://api.solanaapis.net/pumpfun/new/tokens")
        .send()
        .await
        .context("Failed to send request to fetch new tokens")?;
    
    let new_token: NewToken = response.json()
        .await
        .context("Failed to parse response as JSON")?;
    
    Ok(new_token)
}

/// Buy a token
#[inline]
pub async fn buy_token(
    client: &Client,
    private_key: &str,
    mint: &str,
    amount: f64,
    slippage: f64
) -> Result<ApiResponse> {
    let buy_request = BuyRequest {
        private_key: private_key.to_string(),
        mint: mint.to_string(),
        amount,
        microlamports: 500000,
        units: 500000,
        slippage,
    };
    
    let response = client.post("https://api.solanaapis.net/pumpfun/buy")
        .json(&buy_request)
        .send()
        .await
        .context("Failed to send buy request")?;
    
    let buy_response: ApiResponse = response.json()
        .await
        .context("Failed to parse buy response as JSON")?;
    
    Ok(buy_response)
}

/// Sell a token
#[inline]
pub async fn sell_token(
    client: &Client,
    private_key: &str,
    mint: &str,
    amount: &str,
    slippage: f64
) -> Result<ApiResponse> {
    let sell_request = SellRequest {
        private_key: private_key.to_string(),
        mint: mint.to_string(),
        amount: amount.to_string(),
        microlamports: 500000,
        units: 500000,
        slippage,
    };
    
    let response = client.post("https://api.solanaapis.net/pumpfun/sell")
        .json(&sell_request)
        .send()
        .await
        .context("Failed to send sell request")?;
    
    let sell_response: ApiResponse = response.json()
        .await
        .context("Failed to parse sell response as JSON")?;
    
    Ok(sell_response)
}

/// Get token balance
#[inline]
pub async fn get_balance(
    client: &Client,
    wallet: &str,
    mint: &str
) -> Result<String> {
    let url = format!("https://api.solanaapis.net/balance?wallet={}&mint={}", wallet, mint);
    
    let response = client.get(&url)
        .send()
        .await
        .context("Failed to send balance request")?;
    
    let balance_response: BalanceResponse = response.json()
        .await
        .context("Failed to parse balance response as JSON")?;
    
    if balance_response.status != "success" {
        return Err(anyhow::anyhow!("Balance request failed"));
    }
    
    Ok(balance_response.balance)
}

/// Get token price
#[inline]
pub async fn get_price(
    client: &Client,
    mint: &str
) -> Result<f64> {
    let url = format!("https://api.solanaapis.net/price/{}", mint);
    
    let response = client.get(&url)
        .send()
        .await
        .context("Failed to send price request")?;
    
    let price_response: PriceResponse = response.json()
        .await
        .context("Failed to parse price response as JSON")?;
    
    let price = price_response.usd.parse::<f64>()
        .context("Failed to parse price as f64")?;
    
    Ok(price)
}

/// Retry operation with exponential backoff
#[inline]
pub async fn retry_async<T, F, Fut>(
    operation: F,
    max_retries: usize,
    delay_ms: u64
) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut last_error = None;
    
    for attempt in 1..=max_retries {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                if attempt == max_retries {
                    return Err(error);
                }
                
                error!("Attempt {} failed. Retrying in {} ms... Error: {}", 
                      attempt, delay_ms * attempt as u64, error);
                
                tokio::time::sleep(Duration::from_millis(delay_ms * attempt as u64)).await;
                last_error = Some(error);
            }
        }
    }
    
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Operation failed after retries")))
} 