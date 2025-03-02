use anyhow::Result;
use dotenv::dotenv;
use log::{error, info, LevelFilter};
use std::env;

// Import the websocket modules
use pumpfun_sniper::websocket_reconnect;
use pumpfun_sniper::chainstack_simple;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize environment variables from .env file
    dotenv().ok();
    
    // Initialize logging
    env_logger::Builder::new()
        .filter_level(LevelFilter::Info)
        .format_timestamp_millis()
        .init();
    
    info!("Starting token extraction...");
    
    // Get WebSocket endpoint from environment or use default from chainstack
    let wss_endpoint = env::var("CHAINSTACK_WSS_ENDPOINT")
        .unwrap_or_else(|_| chainstack_simple::get_authenticated_wss_url());
        
    // Set max reconnection attempts
    let max_attempts = env::var("MAX_RECONNECTION_ATTEMPTS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(5);
        
    // Set test duration
    let duration = env::var("MONITOR_DURATION")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(120);
        
    info!("Using Chainstack WebSocket endpoint: {}", wss_endpoint);
    info!("Max reconnection attempts: {}", max_attempts);
    info!("Test duration: {} seconds", duration);
    
    // Run the WebSocket test with reconnection logic
    match websocket_reconnect::run_websocket_with_reconnect(
        &wss_endpoint, 
        Some(max_attempts),
        Some(duration)
    ).await {
        Ok(token_data_list) => {
            // Display the results
            if token_data_list.is_empty() {
                info!("Token extraction complete. No token creation events detected during the test period.");
            } else {
                info!("Token extraction complete. Found {} tokens during the session.", token_data_list.len());
            }
            
            // Print a simple completion message
            println!("Token extraction completed.");
        },
        Err(e) => {
            error!("Error running WebSocket test: {}", e);
            
            // Suggest specific troubleshooting options for Chainstack
            println!("\nThe Chainstack WebSocket endpoint might be unavailable. Try these options:");
            println!("1. Check your Chainstack API key in .env file");
            println!("2. Verify your Chainstack subscription is active");
            println!("3. Try increasing MAX_RECONNECTION_ATTEMPTS (current: {})", max_attempts);
            println!("4. Set CHAINSTACK_WSS_ENDPOINT to a different endpoint in your .env file");
        }
    }
    
    Ok(())
} 