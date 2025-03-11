// src/api/utils.rs
// Utility functions for the API module

use anyhow::{anyhow, Result};
use log::{debug, error, info, warn};
use reqwest::{Client, ClientBuilder};
use std::time::Duration;

/// Create a standard HTTP client
pub fn create_client() -> Client {
    ClientBuilder::new()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| Client::new())
}

/// Create a speed-optimized HTTP client for high-performance operations
pub fn create_speed_optimized_client() -> Client {
    ClientBuilder::new()
        .timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(10)
        .pool_idle_timeout(Duration::from_secs(30))
        .tcp_nodelay(true)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .http2_keep_alive_interval(Duration::from_secs(10))
        .http2_keep_alive_timeout(Duration::from_secs(20))
        .http2_keep_alive_while_idle(true)
        .build()
        .unwrap_or_else(|_| {
            warn!("Failed to create optimized client, falling back to default");
            Client::new()
        })
}

// Re-export the get_transaction_block_info function from the trading module
pub use crate::trading::performance::get_transaction_block_info;

/// Fetch new tokens from the API
pub async fn fetch_new_tokens(client: &reqwest::Client) -> Result<crate::api::models::TokenData> {
    // Implementation would go here
    // (Placeholder)
    Err(anyhow!("Not implemented"))
}

/// Fetch tokens from the API with optional filtering
pub async fn fetch_tokens_from_api(client: &reqwest::Client) -> Result<Option<crate::api::models::TokenData>> {
    // Implementation would go here
    // (Placeholder)
    Ok(None)
}
