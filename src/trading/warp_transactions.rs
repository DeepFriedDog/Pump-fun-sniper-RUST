//! WebSocket-based Warp Transactions implementation for Solana
use anyhow::{anyhow, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use serde_json::{json, Value};
use solana_sdk::{signature::Signature, transaction::VersionedTransaction};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream};
use tokio::net::TcpStream;
use url::Url;
use uuid::Uuid;

/// Transaction status: preparing transaction
pub const TX_STATUS_PREPARING: usize = 1;
/// Transaction status: connecting to WebSocket
pub const TX_STATUS_CONNECTING: usize = 2;
/// Transaction status: sending transaction
pub const TX_STATUS_SENDING: usize = 3;
/// Transaction status: waiting for confirmation
pub const TX_STATUS_WAITING: usize = 4;
/// Transaction status: transaction completed
pub const TX_STATUS_COMPLETED: usize = 5;
/// Transaction status: transaction failed
pub const TX_STATUS_FAILED: usize = 10;

/// Send a transaction via WebSocket using Chainstack's Warp Transactions feature
/// This uses a dedicated WebSocket connection to the TRADER_NODE_WSS_URL, separate from token detection
pub async fn send_transaction_via_websocket(transaction: &VersionedTransaction) -> Result<Signature> {
    // Create a status tracker with default preparing state
    let status = Arc::new(AtomicUsize::new(TX_STATUS_PREPARING));
    
    // Send the transaction with status tracking
    send_transaction_via_websocket_with_status(transaction, status).await
}

/// WebSocket Warp Transaction handler - processes a transaction with status tracking
/// Uses a dedicated WebSocket connection to the trader node, completely separate from token detection
pub async fn send_transaction_via_websocket_with_status(
    transaction: &VersionedTransaction, 
    status: Arc<AtomicUsize>
) -> Result<Signature> {
    // Update status to preparing
    status.store(TX_STATUS_PREPARING, Ordering::Relaxed);
    
    // Prepare the transaction data
    let serialized_tx = bincode::serialize(transaction)?;
    let encoded_tx = base64::engine::general_purpose::STANDARD.encode(serialized_tx);
    
    // Get the WebSocket URL specifically for Warp transactions (trader node)
    // This is completely separate from the token detection WebSocket
    let ws_url = crate::config::get_trader_node_ws_url();
    info!("🚀 Using dedicated Warp Transaction WebSocket ({})", ws_url);
    debug!("Note: This is a completely separate connection from the token detection WebSocket");
    
    // Create a unique request ID
    let request_id = Uuid::new_v4().to_string();
    debug!("📝 Creating WebSocket request with ID: {}", request_id);
    
    // Prepare the WebSocket request
    let skip_preflight = std::env::var("SKIP_PREFLIGHT")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(true);

    let max_retries = std::env::var("MAX_RETRIES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(3);

    // Create request with processed commitment for maximum speed
    let request = json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "sendTransaction",
        "params": [
            encoded_tx,
            {
                "skipPreflight": skip_preflight,
                "preflightCommitment": "processed", // Using processed for fastest speed
                "encoding": "base64",
                "maxRetries": max_retries,
            }
        ]
    });
    
    status.store(TX_STATUS_CONNECTING, Ordering::Relaxed);
    
    // Parse URL and connect with timeout
    let url = Url::parse(&ws_url)?;
    let connect_timeout = Duration::from_secs(5);
    
    // Important: This WebSocket connection is DEDICATED to the Warp transaction
    // and is completely separate from the token detection WebSocket.
    // It should remain open until the transaction is complete, regardless
    // of the token detection WebSocket's status.
    let ws_stream = match timeout(connect_timeout, connect_async(url)).await {
        Ok(Ok((stream, _))) => {
            info!("✅ WebSocket connection established for Warp transaction");
            stream
        },
        Ok(Err(e)) => {
            error!("❌ WebSocket connection failed: {}", e);
            status.store(TX_STATUS_FAILED, Ordering::Relaxed);
            return Err(anyhow!("WebSocket connection failed: {}", e));
        },
        Err(_) => {
            error!("❌ WebSocket connection timed out after {}s", connect_timeout.as_secs());
            status.store(TX_STATUS_FAILED, Ordering::Relaxed);
            return Err(anyhow!("WebSocket connection timed out"));
        }
    };
    
    let (mut write, mut read) = ws_stream.split();
    
    // Send the transaction
    status.store(TX_STATUS_SENDING, Ordering::Relaxed);
    let send_timeout = Duration::from_secs(5);
    
    match timeout(send_timeout, write.send(Message::Text(request.to_string()))).await {
        Ok(Ok(_)) => {
            info!("✅ Warp transaction request sent successfully");
        },
        Ok(Err(e)) => {
            error!("❌ Failed to send transaction request: {}", e);
            status.store(TX_STATUS_FAILED, Ordering::Relaxed);
            return Err(anyhow!("Failed to send transaction request: {}", e));
        },
        Err(_) => {
            error!("❌ Transaction request send timed out");
            status.store(TX_STATUS_FAILED, Ordering::Relaxed);
            return Err(anyhow!("Transaction request send timed out"));
        }
    }
    
    // Wait for response
    status.store(TX_STATUS_WAITING, Ordering::Relaxed);
    // Increase the response timeout for better reliability
    let response_timeout = Duration::from_secs(30); // Extended from 10s to 30s
    let start = Instant::now();
    
    info!("⏳ Waiting for Warp transaction response (timeout: {}s)...", response_timeout.as_secs());
    
    while start.elapsed() < response_timeout {
        match timeout(Duration::from_secs(1), read.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                debug!("📥 Received WebSocket message: {}", text);
                
                if let Ok(json) = serde_json::from_str::<Value>(&text) {
                    if json.get("id").and_then(|id| id.as_str()) == Some(&request_id) {
                        // Check for error
                        if let Some(error) = json.get("error") {
                            let error_msg = error.to_string();
                            error!("🚫 Warp transaction error: {}", error_msg);
                            status.store(TX_STATUS_FAILED, Ordering::Relaxed);
                            return Err(anyhow!("Warp transaction error: {}", error_msg));
                        }
                        
                        // Extract signature
                        if let Some(result) = json.get("result") {
                            if let Some(signature) = result.as_str() {
                                match Signature::from_str(signature) {
                                    Ok(sig) => {
                                        info!("🚀 Warp transaction sent successfully: {}", signature);
                                        status.store(TX_STATUS_COMPLETED, Ordering::Relaxed);
                                        return Ok(sig);
                                    },
                                    Err(e) => {
                                        error!("❌ Invalid signature: {}", e);
                                        status.store(TX_STATUS_FAILED, Ordering::Relaxed);
                                        return Err(anyhow!("Invalid signature: {}", e));
                                    }
                                }
                            }
                        }
                        
                        error!("❌ Invalid response format");
                        status.store(TX_STATUS_FAILED, Ordering::Relaxed);
                        return Err(anyhow!("Invalid response format"));
                    }
                }
            },
            Ok(Some(Ok(Message::Binary(_)))) => {
                debug!("📦 Received binary message (ignoring)");
            },
            Ok(Some(Ok(Message::Ping(data)))) => {
                debug!("🏓 Received ping, responding with pong");
                if let Err(e) = write.send(Message::Pong(data)).await {
                    warn!("⚠️ Failed to send pong: {}", e);
                }
            },
            Ok(Some(Ok(Message::Pong(_)))) => {
                debug!("🏓 Received pong");
            },
            Ok(Some(Ok(Message::Close(_)))) => {
                warn!("🔌 WebSocket connection closed by server");
                break;
            },
            Ok(Some(Ok(Message::Frame(_)))) => {
                debug!("🖼️ Received frame message (ignoring)");
            },
            Ok(Some(Err(e))) => {
                error!("❌ WebSocket error: {}", e);
                break;
            },
            Ok(None) => {
                warn!("🔌 WebSocket stream ended");
                break;
            },
            Err(_) => {
                // Timeout on read, continue the loop
            }
        }
    }
    
    error!("❌ No response received for Warp transaction");
    status.store(TX_STATUS_FAILED, Ordering::Relaxed);
    Err(anyhow!("No response received for Warp transaction"))
} 