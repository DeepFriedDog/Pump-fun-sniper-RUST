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
    // Clone URL early for potential reconnection attempts later
    let url_clone = url.clone();
    let connect_timeout = Duration::from_secs(10); // Increased from 5 to 10 seconds
    
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
    let send_timeout = Duration::from_secs(10); // Increased from 5 to 10 seconds
    
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
    
    // Wait for response with better error handling
    status.store(TX_STATUS_WAITING, Ordering::Relaxed);
    // Increase the response timeout for better reliability
    let response_timeout = Duration::from_secs(90); // Extended from 60s to 90s for more reliability
    let start = Instant::now();
    
    info!("⏳ Waiting for Warp transaction response (timeout: {}s)...", response_timeout.as_secs());
    
    // Keep the WebSocket alive with periodic pings
    let ping_interval = Duration::from_secs(5);
    let mut last_ping = Instant::now();
    
    while start.elapsed() < response_timeout {
        // Send periodic pings to keep the connection alive
        if last_ping.elapsed() >= ping_interval {
            debug!("Sending ping to keep WebSocket connection alive");
            if let Err(e) = write.send(Message::Ping(vec![1, 2, 3])).await {
                warn!("Failed to send ping: {}", e);
            }
            last_ping = Instant::now();
        }
        
        match timeout(Duration::from_secs(1), read.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                debug!("📥 Received WebSocket message: {}", text);
                
                if let Ok(json) = serde_json::from_str::<Value>(&text) {
                    // First check if this is a response to our specific request
                    if json.get("id").and_then(|id| id.as_str()) == Some(&request_id) {
                        // Check for error
                        if let Some(error) = json.get("error") {
                            let error_msg = error.to_string();
                            error!("🚫 Warp transaction error: {}", error_msg);
                            status.store(TX_STATUS_FAILED, Ordering::Relaxed);
                            return Err(anyhow!("Warp transaction error: {}", error_msg));
                        }
                        
                        // Extract signature from the result
                        if let Some(result) = json.get("result") {
                            if let Some(signature) = result.as_str() {
                                match Signature::from_str(signature) {
                                    Ok(sig) => {
                                        // Log the successful transaction
                                        info!("🚀 Warp transaction SUCCESSFULLY SENT: {}", signature);
                                        
                                        // Store the signature in environment for tracking
                                        std::env::set_var("LAST_BUY_SIGNATURE", signature);
                                        
                                        // Also set a timestamp for this transaction
                                        let timestamp = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_secs();
                                        std::env::set_var("LAST_BUY_TIMESTAMP", timestamp.to_string());
                                        
                                        // Get mint signature from environment for performance tracking
                                        if let Ok(mint_sig) = std::env::var("_DETECTION_SIGNATURE") {
                                            info!("Found mint signature for performance tracking: {}", mint_sig);
                                            std::env::set_var("LAST_MINT_SIGNATURE", mint_sig.clone());
                                            
                                            // Spawn a task to calculate performance metrics
                                            let buy_sig = signature.to_string();
                                            let mint_sig_clone = mint_sig;
                                            tokio::spawn(async move {
                                                match crate::trading::performance::compute_performance_metrics(&mint_sig_clone, &buy_sig).await {
                                                    Ok((block_diff, time_diff_ms)) => {
                                                        info!("🏁 PERFORMANCE: {}ms ({} blocks) from mint to buy confirmation", 
                                                              time_diff_ms, block_diff);
                                                        
                                                        // Store the metrics for future reference
                                                        std::env::set_var("_BLOCK_TO_BLOCK_MS", time_diff_ms.to_string());
                                                        std::env::set_var("_BLOCKS_DIFFERENCE", block_diff.to_string());
                                                    },
                                                    Err(e) => {
                                                        warn!("Could not calculate performance metrics: {}", e);
                                                    }
                                                }
                                            });
                                        } else {
                                            warn!("No mint signature found for performance tracking");
                                        }
                                        
                                        // Mark as completed
                                        status.store(TX_STATUS_COMPLETED, Ordering::Relaxed);
                                        
                                        // This is critical - return the signature to complete the transaction flow
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
                        
                        // If we got here, the response format was invalid
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
                last_ping = Instant::now(); // Reset ping timer when we get a pong
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
                // Try to reconnect on error
                warn!("Attempting to reconnect WebSocket...");
                match timeout(connect_timeout, connect_async(url_clone.clone())).await {
                    Ok(Ok((stream, _))) => {
                        info!("✅ WebSocket reconnected");
                        let (w, r) = stream.split();
                        write = w;
                        read = r;
                        // Resend the transaction
                        if let Err(e) = write.send(Message::Text(request.to_string())).await {
                            error!("Failed to resend transaction after reconnect: {}", e);
                            break;
                        }
                    },
                    _ => {
                        error!("Failed to reconnect WebSocket");
                        break;
                    }
                }
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
    
    // If we reach this point, we didn't get a successful response in time
    error!("❌ No response received for Warp transaction within {} seconds", response_timeout.as_secs());
    info!("Network conditions may be congested or the transaction may still be in flight");
    info!("You can check if the transaction was eventually confirmed using a block explorer");
    
    status.store(TX_STATUS_FAILED, Ordering::Relaxed);
    Err(anyhow!("No response received for Warp transaction within {} seconds", response_timeout.as_secs()))
} 