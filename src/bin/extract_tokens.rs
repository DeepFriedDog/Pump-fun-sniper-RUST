use anyhow::Result;
use dotenv::dotenv;
use log::{error, info};
use std::env;
use std::time::Duration;

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
    
    // Set max idle time for connection health monitoring (in seconds)    
    let max_idle_seconds = env::var("MAX_IDLE_TIME")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60); // Default to 60 seconds
        
    let max_idle_time = Duration::from_secs(max_idle_seconds);
    
    // Get quiet mode setting
    let quiet_mode = !debug_websocket;
        
    info!("Using Chainstack WebSocket endpoint: {}", wss_endpoint);
    info!("Max idle time before reconnection: {} seconds", max_idle_seconds);
    
    // Run the WebSocket with indefinite reconnection logic
    match websocket_reconnect::run_websocket_with_reconnect(
        &wss_endpoint, 
        quiet_mode,
        max_idle_time
    ).await {
        Ok(_) => {
            // This should only happen if the function returns normally (unlikely as it's a loop)
            info!("WebSocket monitoring completed normally.");
        },
        Err(e) => {
            error!("Error with WebSocket connection: {}", e);
            
            // Suggest specific troubleshooting options for Chainstack
            println!("\nThe Chainstack WebSocket endpoint might be unavailable. Try these options:");
            println!("1. Check your Chainstack API key in .env file");
            println!("2. Verify your Chainstack subscription is active");
            println!("3. Try increasing MAX_IDLE_TIME (current: {} seconds)", max_idle_seconds);
            println!("4. Set CHAINSTACK_WSS_ENDPOINT to a different endpoint in your .env file");
        }
    }
    
    Ok(())
} 