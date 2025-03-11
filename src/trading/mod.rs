/// Trading module for token buying and selling
use log::{info, warn, error};
use anyhow::{Result, anyhow};

// Export submodules
pub mod performance;
pub mod utils;
pub mod warp_transactions;
pub mod priority;

/// Buys a token with the provided parameters
pub async fn buy_token(
    mint: &str,
    amount: f64,
    slippage: f64,
    use_wsol: bool,
) -> Result<String> {
    // From the .env file, we can see SLIPPAGE=30 (meaning 30%)
    // This is already a percentage value, not a decimal like 0.3
    // So we just display it directly without multiplying by 100
    info!("🔄 Buying {} SOL of token {} with slippage {}%", amount, mint, slippage);
    
    // Check if Warp Transactions are enabled
    if crate::config::should_use_warp_transactions() {
        info!("🚀 Using Chainstack Warp transactions for high-speed trading");
        
        // For now, we'll just return a placeholder
        // In a real implementation, this would call the warp_transactions module
        return Ok("Warp transaction placeholder".to_string());
    } else {
        info!("🔄 Using standard transaction processing (Warp transactions not enabled)");
    }
    
    // Get private key from environment
    let private_key = std::env::var("PRIVATE_KEY")
        .map_err(|_| anyhow!("PRIVATE_KEY environment variable not set"))?;
    
    // For now, we'll just return a placeholder
    // In a real implementation, this would call the API module
    Ok(format!("Transaction for {} SOL of {}", amount, mint))
}

/// Sells a token with the provided parameters
pub async fn sell_token(
    mint: &str,
    amount: f64,
    slippage: f64,
) -> Result<String> {
    info!("🔄 Selling {} of token {} with slippage {:.1}%", amount, mint, slippage * 100.0);
    
    // Get private key from environment
    let private_key = std::env::var("PRIVATE_KEY")
        .map_err(|_| anyhow!("PRIVATE_KEY environment variable not set"))?;
    
    // For now, we'll just return a placeholder
    // In a real implementation, this would call the API module
    Ok(format!("Sell transaction for {} of {}", amount, mint))
} 