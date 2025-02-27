use anyhow::Result;
use log::{info, warn, error};
use reqwest::Client;
use serde_json::Value;
use std::fs;
use std::path::Path;
use url::Url;
use crate::api;

/// Check if token metadata contains required URLs
#[inline]
pub async fn check_urls(client: &Client, metadata_url: &str) -> Result<bool> {
    // Create user agent and headers to prevent 403 responses
    let response = client.get(metadata_url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
        .header("Accept", "application/json, text/plain, */*")
        .send()
        .await?;
    
    let data: Value = response.json().await?;
    
    let is_valid_url = |url_str: &str| -> bool {
        Url::parse(url_str).is_ok()
    };
    
    // Check for Twitter URL
    let has_twitter = match data.get("twitter") {
        Some(twitter) => {
            match twitter.as_str() {
                Some(twitter_str) if !twitter_str.trim().is_empty() => {
                    is_valid_url(twitter_str)
                },
                _ => false,
            }
        },
        None => false,
    };
    
    // Check for Telegram URL
    let has_telegram = match data.get("telegram") {
        Some(telegram) => {
            match telegram.as_str() {
                Some(telegram_str) if !telegram_str.trim().is_empty() => {
                    is_valid_url(telegram_str)
                },
                _ => false,
            }
        },
        None => false,
    };
    
    // Check for Website URL
    let has_website = match data.get("website") {
        Some(website) => {
            match website.as_str() {
                Some(website_str) if !website_str.trim().is_empty() => {
                    is_valid_url(website_str)
                },
                _ => false,
            }
        },
        None => false,
    };
    
    if !has_twitter {
        warn!("Twitter URL missing or invalid in metadata");
    }
    
    if !has_telegram {
        warn!("Telegram URL missing or invalid in metadata");
    }
    
    if !has_website {
        warn!("Website URL missing or invalid in metadata");
    }
    
    Ok(has_twitter && has_telegram && has_website)
}

/// Check the minimum liquidity for a token
#[inline]
pub async fn check_minimum_liquidity(client: &Client, dev_wallet: &str, mint: &str) -> Result<bool> {
    // Get the minimum liquidity threshold from environment
    let min_liquidity_str = std::env::var("MIN_LIQUIDITY").unwrap_or_else(|_| "0".to_string());
    let min_liquidity = min_liquidity_str.parse::<f64>().unwrap_or(0.0);
    
    if min_liquidity <= 0.0 {
        return Ok(true); // Skip check if not properly configured
    }
    
    // Get the developer's token balance - with retry mechanism
    let balance = api::retry_async(
        || async {
            api::get_balance(client, dev_wallet, mint).await
        },
        Some(2), // max retries
        Some(1000) // delay in ms
    ).await?;
    
    // Parse the balance - handle potential formatting issues
    let dev_token_balance = if balance.contains('.') {
        // If there's a decimal, use the whole number part
        balance.split('.').next()
            .unwrap_or("0")
            .parse::<u64>()
            .unwrap_or(0)
    } else {
        balance.parse::<u64>().unwrap_or(0)
    };
    
    // Use the EXACT bonding curve formula for pump.fun:
    // SOL = (32,190,005,730 / (1,073,000,191 - X)) - 30
    // Where X is the developer's token balance
    
    let initial_virtual_token_reserve: u64 = 1_073_000_191;
    let constant: f64 = 32_190_005_730.0;
    let offset: f64 = 30.0;
    
    // Check to prevent division by zero or negative values
    if dev_token_balance >= initial_virtual_token_reserve {
        warn!("Token {} has unusual token balance, skipping buy", mint);
        return Ok(false);
    }
    
    // Calculate SOL deposited using the exact formula
    let sol_deposited = (constant / (initial_virtual_token_reserve as f64 - dev_token_balance as f64)) - offset;
    
    // Check against the minimum liquidity threshold
    if sol_deposited >= min_liquidity {
        info!("Token {} meets minimum liquidity requirements ({:.2} SOL >= {} SOL)", 
             mint, sol_deposited, min_liquidity);
        Ok(true)
    } else {
        warn!("Token {} does not meet minimum liquidity requirements ({:.2} SOL < {} SOL), skipping buy", 
             mint, sol_deposited, min_liquidity);
        Ok(false)
    }
}

/// Check if the token developer is in the approved list
#[inline]
pub async fn check_approved_devs(dev_wallet: &str) -> Result<bool> {
    // Check if APPROVED_DEVS_ONLY is set to true
    let approved_devs_only = std::env::var("APPROVED_DEVS_ONLY")
        .unwrap_or_else(|_| "false".to_string()) == "true";
    
    if !approved_devs_only {
        // If the feature is not enabled, pass the check
        return Ok(true);
    }
    
    // Load the approved developers list
    let approved_devs_path = Path::new("src/approved-devs.json");
    
    if !approved_devs_path.exists() {
        error!("Approved developers list not found");
        return Ok(false);
    }
    
    let approved_devs_json = fs::read_to_string(approved_devs_path)?;
    let approved_devs: Vec<Value> = serde_json::from_str(&approved_devs_json)?;
    
    // Check if the developer wallet is in the approved list
    let is_approved = approved_devs.iter().any(|dev| {
        if let Some(address) = dev.get("address").and_then(|a| a.as_str()) {
            address == dev_wallet
        } else {
            false
        }
    });
    
    if is_approved {
        info!("Developer {} is in the approved list", dev_wallet);
        Ok(true)
    } else {
        warn!("Developer {} is not in the approved list", dev_wallet);
        Ok(false)
    }
} 