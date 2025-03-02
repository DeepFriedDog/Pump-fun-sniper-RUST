use anyhow::Result;
use dotenv::dotenv;
use log::{error, info};
use std::env;

// Import the websocket modules
use pumpfun_sniper::websocket_reconnect;
use pumpfun_sniper::chainstack_simple;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize environment variables from .env file
    dotenv().ok();
    
    // Check if DEBUG_WEBSOCKET_MESSAGES is set to true
    let debug_websocket = env::var("DEBUG_WEBSOCKET_MESSAGES")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase() == "true";
    
    // Set appropriate log level based on DEBUG_WEBSOCKET_MESSAGES
    let log_level = if debug_websocket {
        "debug" // Use debug level if DEBUG_WEBSOCKET_MESSAGES=true
    } else {
        "info"  // Otherwise use info level to hide raw messages
    };
    
    // Initialize logger with the appropriate level
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_level))
        .format_timestamp(Some(env_logger::TimestampPrecision::Millis))
        .format_module_path(false)
        .init();
    
    info!("Starting token extraction...");
    
    if debug_websocket {
        info!("Debug mode enabled: Will show raw WebSocket messages");
    }
    
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