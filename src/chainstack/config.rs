use lazy_static::lazy_static;
use std::env;
use log::info;

// Default Chainstack endpoints
const DEFAULT_CHAINSTACK_HTTPS_ENDPOINT: &str = "https://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab";
const DEFAULT_CHAINSTACK_WSS_ENDPOINT: &str = "wss://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab";

// Default auth credentials
const DEFAULT_CHAINSTACK_USERNAME: &str = "frosty-archimedes";
const DEFAULT_CHAINSTACK_PASSWORD: &str = "thank-angles-choice-unsaid-spooky-woven";

lazy_static! {
    // Chainstack endpoints from environment variables or defaults
    pub static ref CHAINSTACK_HTTPS_ENDPOINT: String = env::var("CHAINSTACK_HTTPS_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_CHAINSTACK_HTTPS_ENDPOINT.to_string());
    
    pub static ref CHAINSTACK_WSS_ENDPOINT: String = env::var("CHAINSTACK_WSS_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_CHAINSTACK_WSS_ENDPOINT.to_string());
    
    // Chainstack auth credentials from environment variables or defaults
    pub static ref CHAINSTACK_USERNAME: String = env::var("CHAINSTACK_USERNAME")
        .unwrap_or_else(|_| DEFAULT_CHAINSTACK_USERNAME.to_string());
    
    pub static ref CHAINSTACK_PASSWORD: String = env::var("CHAINSTACK_PASSWORD")
        .unwrap_or_else(|_| DEFAULT_CHAINSTACK_PASSWORD.to_string());
    
    // Whether to use authentication - default to true since Chainstack typically requires auth
    pub static ref USE_CHAINSTACK_AUTH: bool = env::var("USE_CHAINSTACK_AUTH")
        .map(|v| v.to_lowercase() != "false")
        .unwrap_or(true);
}

/// Get the appropriate Chainstack endpoint URL
pub fn get_chainstack_endpoint() -> String {
    let use_auth = std::env::var("USE_CHAINSTACK_AUTH")
        .unwrap_or_else(|_| "false".to_string()) == "true";
        
    // Check if we should use the trader node
    let use_trader = std::env::var("USE_TRADER_NODE")
        .unwrap_or_else(|_| "false".to_string()) == "true";
        
    // Get the configured endpoint from environment
    let endpoint = std::env::var("CHAINSTACK_ENDPOINT")
        .unwrap_or_else(|_| {
            if use_trader {
                "https://your-trader-node-url.solana.p2pify.com".to_string()
            } else {
                "https://your-chainstack-node-url.solana.p2pify.com".to_string()
            }
        });
        
    // Add authentication if needed
    if use_auth {
        let username = get_chainstack_username();
        let password = get_chainstack_password();
        
        // Extract protocol and rest of the URL
        if let Some(pos) = endpoint.find("://") {
            let (protocol, rest) = endpoint.split_at(pos + 3);
            return format!("{}{}:{}@{}", protocol, username, password, rest);
        }
        
        // Fallback in case URL doesn't have protocol
        format!("https://{}:{}@{}", username, password, endpoint)
    } else {
        endpoint
    }
}

// Get WebSocket endpoint based on HTTP endpoint
pub fn get_chainstack_wss_endpoint() -> String {
    let http_endpoint = get_chainstack_endpoint();
    
    // Convert HTTP to WSS
    if http_endpoint.starts_with("https://") {
        http_endpoint.replace("https://", "wss://")
    } else if http_endpoint.starts_with("http://") {
        http_endpoint.replace("http://", "ws://")
    } else {
        format!("wss://{}", http_endpoint)
    }
}

/// Gets the Chainstack username
pub fn get_chainstack_username() -> String {
    std::env::var("CHAINSTACK_USERNAME").unwrap_or_else(|_| DEFAULT_CHAINSTACK_USERNAME.to_string())
}

/// Gets the Chainstack password
pub fn get_chainstack_password() -> String {
    std::env::var("CHAINSTACK_PASSWORD").unwrap_or_else(|_| DEFAULT_CHAINSTACK_PASSWORD.to_string())
}

/// Checks if Chainstack authentication is enabled
pub fn use_chainstack_auth() -> bool {
    std::env::var("USE_CHAINSTACK_AUTH")
        .map(|v| v.to_lowercase() != "false")
        .unwrap_or(true) // Default to true since Chainstack typically requires auth
}

pub fn get_chainstack_wss_url() -> String {
    // Get the WebSocket URL from environment
    let wss_url = std::env::var("CHAINSTACK_WSS_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_CHAINSTACK_WSS_ENDPOINT.to_string());
    
    // Check if we should use authentication
    if use_chainstack_auth() {
        // Get credentials
        let username = get_chainstack_username();
        let password = get_chainstack_password();
        
        // Check if wss_url already contains the credentials to avoid duplication
        if wss_url.contains(&format!("{}:{}", username, password)) {
            wss_url
        } else if wss_url.starts_with("wss://") {
            // Clean URL to ensure no duplicate credentials
            let clean_url = if wss_url.contains('@') {
                // Strip existing credentials
                let parts = wss_url.split('@').collect::<Vec<&str>>();
                format!("wss://{}", parts[1])
            } else {
                wss_url
            };
            
            // Insert credentials after wss://
            let with_auth = clean_url.replace("wss://", &format!("wss://{}:{}@", username, password));
            info!("Using authenticated WebSocket URL: {}", with_auth);
            with_auth
        } else {
            // Fallback for non-standard URLs
            wss_url
        }
    } else {
        wss_url
    }
} 