#![allow(unused)]

pub mod api;
pub mod chainstack_simple;
pub mod checks;
pub mod config;
pub mod db;
pub mod error;
pub mod token_detector;
pub mod trading;

// Create a unified websocket module that uses token_detector for token detection
// and api/websocket for transaction handling
pub mod websocket {
    pub use crate::api::websocket::*;
    
    // Export important functionality from token_detector 
    pub use crate::token_detector::{
        DetectorTokenData,
        NewToken,
        listen_for_new_tokens,
        check_token_liquidity,
        check_token_primary_liquidity,
        get_bonding_curve_address,
        find_associated_bonding_curve,
        connect_websocket_simple,
    };
    
    // Add constants for WebSocket types
    pub const WS_TYPE_TOKEN_DETECTION: &str = "token_detection";
    pub const WS_TYPE_WARP_TRANSACTION: &str = "warp_transaction";
    
    // Re-implement connect_with_retry functionality for backwards compatibility
    pub async fn connect_with_retry(
        url: &str, 
        quiet_mode: bool,
    ) -> anyhow::Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>> {
        // Connect to websocket but return a descriptive error 
        // since this function is meant to be deprecated
        let _ = connect_websocket_simple(url).await;
        
        // Return a clear error that this function is deprecated and should be replaced
        Err(anyhow::anyhow!("This function is deprecated. Use token_detector::listen_for_new_tokens instead for token detection, or api::websocket functions for transactions."))
    }
}
