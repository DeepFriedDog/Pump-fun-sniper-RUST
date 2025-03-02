use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use std::time::{Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;

/// Handles WebSocket connection with automatic reconnection
pub async fn connect_with_retry(url_str: &str, quiet_mode: bool) -> Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>> {
    // Parse the WebSocket URL
    let url = Url::parse(url_str)?;
    
    // Retry settings
    let mut retry_attempts = 0;
    let max_retries = 5;
    let initial_backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);
    let mut current_backoff = initial_backoff;
    let backoff_factor = 2.0;
    
    // Try to establish the WebSocket connection with retries
    loop {
        match connect_async(&url).await {
            Ok((ws_stream, _)) => {
                if !quiet_mode {
                    info!("WebSocket connection established to: {}", url);
                }
                return Ok(ws_stream);
            },
            Err(e) => {
                retry_attempts += 1;
                if retry_attempts >= max_retries {
                    return Err(anyhow!("Failed to connect to WebSocket after {} attempts: {}", max_retries, e));
                }
                
                warn!("WebSocket connection attempt {} failed: {}. Retrying in {} seconds...", 
                      retry_attempts, e, current_backoff.as_secs());
                
                // Exponential backoff
                tokio::time::sleep(current_backoff).await;
                
                // Update backoff for next attempt
                let backoff_secs = (current_backoff.as_secs() as f64 * backoff_factor) as u64;
                current_backoff = std::cmp::min(Duration::from_secs(backoff_secs), max_backoff);
            }
        }
    }
}

/// Send WebSocket subscription for the Pump.fun program
pub async fn subscribe_to_pump_program(ws_stream: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>) -> Result<()> {
    // Pump.fun program ID for token creation
    let pump_program_id = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
    
    // Set to "processed" commitment for faster detection
    let subscription_request = serde_json::json!({
        "id": 1,
        "jsonrpc": "2.0",
        "method": "logsSubscribe",
        "params": [
            {"mentions": [pump_program_id]},
            {"commitment": "processed"}
        ]
    }).to_string();
    
    // Send the subscription
    ws_stream.send(Message::Text(subscription_request)).await?;
    info!("Sent subscription request for Pump.fun program");
    
    Ok(())
}

/// Check if WebSocket connection is alive and send heartbeat
pub async fn send_heartbeat(ws_stream: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, quiet_mode: bool) -> Result<()> {
    // Send a ping to keep the connection alive
    ws_stream.send(Message::Ping(vec![1, 2, 3])).await?;
    
    if !quiet_mode {
        debug!("Sent heartbeat ping to keep WebSocket connection alive");
    }
    
    Ok(())
}

/// Run a WebSocket connection with automatic reconnection
pub async fn run_websocket_with_reconnect(
    url: &str, 
    quiet_mode: bool,
    max_idle_time: Duration
) -> Result<()> {
    // Connect with retry
    let mut ws_stream = connect_with_retry(url, quiet_mode).await?;
    
    // Subscribe to pump program
    subscribe_to_pump_program(&mut ws_stream).await?;
    
    // Setup heartbeat interval
    let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
    
    // Last received message time for connection health check
    let mut last_message_time = Instant::now();
    
    // Process messages in a loop
    loop {
        tokio::select! {
            // Heartbeat timer
            _ = ping_interval.tick() => {
                // Check if connection is stale (no messages for too long)
                if last_message_time.elapsed() > max_idle_time {
                    warn!("WebSocket connection appears to be stale - no messages for {} seconds", 
                          last_message_time.elapsed().as_secs());
                    
                    // Reconnect
                    match connect_with_retry(url, quiet_mode).await {
                        Ok(new_stream) => {
                            // Close old connection if possible
                            let _ = ws_stream.close(None).await;
                            
                            // Update the stream
                            ws_stream = new_stream;
                            
                            // Resubscribe
                            if let Err(e) = subscribe_to_pump_program(&mut ws_stream).await {
                                error!("Failed to resubscribe after reconnection: {}", e);
                            }
                            
                            // Reset timer
                            last_message_time = Instant::now();
                        },
                        Err(e) => {
                            error!("Failed to reconnect: {}", e);
                            // Keep trying in next heartbeat interval
                        }
                    }
                } else {
                    // Send heartbeat to keep connection alive
                    if let Err(e) = send_heartbeat(&mut ws_stream, quiet_mode).await {
                        warn!("Failed to send heartbeat: {}", e);
                        
                        // Reconnect
                        match connect_with_retry(url, quiet_mode).await {
                            Ok(new_stream) => {
                                // Update the stream
                                ws_stream = new_stream;
                                
                                // Resubscribe
                                if let Err(e) = subscribe_to_pump_program(&mut ws_stream).await {
                                    error!("Failed to resubscribe after reconnection: {}", e);
                                }
                                
                                // Reset timer
                                last_message_time = Instant::now();
                            },
                            Err(e) => {
                                error!("Failed to reconnect: {}", e);
                                // Keep trying in next heartbeat interval
                            }
                        }
                    }
                }
            },
            
            // Process incoming messages
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(message)) => {
                        // Update last message time
                        last_message_time = Instant::now();
                        
                        // Process different message types
                        match message {
                            Message::Text(text) => {
                                // Process text message
                                // Example: you would parse JSON and handle it here
                                if !quiet_mode {
                                    debug!("Received text message: {}", text);
                                }
                            },
                            Message::Binary(data) => {
                                if !quiet_mode {
                                    debug!("Received binary message: {} bytes", data.len());
                                }
                            },
                            Message::Ping(data) => {
                                // Automatically respond with pong
                                if let Err(e) = ws_stream.send(Message::Pong(data)).await {
                                    warn!("Failed to send pong: {}", e);
                                }
                            },
                            Message::Pong(_) => {
                                if !quiet_mode {
                                    debug!("Received pong response");
                                }
                            },
                            Message::Close(frame) => {
                                info!("WebSocket closed by server: {:?}", frame);
                                
                                // Reconnect
                                match connect_with_retry(url, quiet_mode).await {
                                    Ok(new_stream) => {
                                        // Update the stream
                                        ws_stream = new_stream;
                                        
                                        // Resubscribe
                                        if let Err(e) = subscribe_to_pump_program(&mut ws_stream).await {
                                            error!("Failed to resubscribe after reconnection: {}", e);
                                        }
                                        
                                        // Reset timer
                                        last_message_time = Instant::now();
                                    },
                                    Err(e) => {
                                        error!("Failed to reconnect after server closure: {}", e);
                                        // Try again in next iteration
                                    }
                                }
                            },
                            _ => {}
                        }
                    },
                    Some(Err(e)) => {
                        warn!("WebSocket error: {}", e);
                        
                        // Reconnect
                        match connect_with_retry(url, quiet_mode).await {
                            Ok(new_stream) => {
                                // Update the stream
                                ws_stream = new_stream;
                                
                                // Resubscribe
                                if let Err(e) = subscribe_to_pump_program(&mut ws_stream).await {
                                    error!("Failed to resubscribe after reconnection: {}", e);
                                }
                                
                                // Reset timer
                                last_message_time = Instant::now();
                            },
                            Err(e) => {
                                error!("Failed to reconnect: {}", e);
                                // Try again in next iteration
                            }
                        }
                    },
                    None => {
                        warn!("WebSocket connection closed unexpectedly");
                        
                        // Reconnect
                        match connect_with_retry(url, quiet_mode).await {
                            Ok(new_stream) => {
                                // Update the stream
                                ws_stream = new_stream;
                                
                                // Resubscribe
                                if let Err(e) = subscribe_to_pump_program(&mut ws_stream).await {
                                    error!("Failed to resubscribe after reconnection: {}", e);
                                }
                                
                                // Reset timer
                                last_message_time = Instant::now();
                            },
                            Err(e) => {
                                error!("Failed to reconnect: {}", e);
                                // Try again in next iteration
                            }
                        }
                    }
                }
            }
        }
    }
} 