// src/api/models.rs
// Model definitions for the API module

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

/// Represents a new token listed on pump.fun
#[derive(Debug, Deserialize)]
pub struct NewToken {
    pub status: String,
    pub mint: String,
    pub name: Option<String>,
    pub metadata: String,
    pub dev: String,
}

/// Request parameters for buying a token
#[derive(Debug, serde::Serialize)]
pub struct BuyRequest {
    pub private_key: String,
    pub mint: String,
    pub amount: f64,
    pub microlamports: u64,
    pub units: u64,
    pub slippage: f64,
    pub rpc_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority_fee: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compute_units: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
}

/// Request parameters for selling a token
#[derive(Debug, serde::Serialize)]
pub struct SellRequest {
    pub private_key: String,
    pub mint: String,
    pub amount: String,
    pub microlamports: u64,
    pub units: u64,
    pub slippage: f64,
    pub rpc_url: String,
}

/// General API response format
#[derive(Debug, Deserialize, Clone)]
pub struct ApiResponse {
    pub status: String,
    #[serde(flatten)]
    #[allow(dead_code)] // Data field is needed for deserialization but not directly accessed
    pub data: Value, // Used for deserialization, even if not directly accessed
    pub mint: String, // Adding mint field to match usage
}

/// Balance query response
#[derive(Debug, Deserialize)]
pub struct BalanceResponse {
    pub status: String,
    pub balance: String,
}

/// Price query response
#[derive(Debug, Deserialize)]
pub struct PriceResponse {
    #[serde(rename = "USD")]
    pub usd: String,
}

/// Token details response
#[derive(Debug, Deserialize)]
pub struct TokenData {
    pub status: String,
    pub mint: String,
    pub dev: String,
    pub metadata: Option<String>,
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub timestamp: Option<i64>,
    pub liquidity_status: Option<bool>,
    pub liquidity_amount: Option<f64>,
}
