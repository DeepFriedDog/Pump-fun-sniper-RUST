use tokio::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{SinkExt, StreamExt};
use url::Url;
use serde_json::json;
use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get the WebSocket URL from environment or use default
    let wss_url = env::var("CHAINSTACK_WSS_ENDPOINT").unwrap_or_else(|_| {
        "wss://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string()
    });
    
    println!("Connecting to: {}", wss_url);
    
    let url = Url::parse(&wss_url)?;
    
    // Connect to the WebSocket server
    let (mut ws_stream, _) = connect_async(url).await?;
    println!("Connected successfully!");
    
    // Subscribe to logs for the pump program
    let pump_program_id = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
    
    let subscription = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "logsSubscribe",
        "params": [
            {"mentions": [pump_program_id]},
            {"commitment": "processed"}
        ]
    });
    
    // Send subscription request
    ws_stream.send(Message::Text(subscription.to_string())).await?;
    println!("Subscription request sent for program: {}", pump_program_id);
    
    // Wait for the subscription confirmation
    if let Some(msg) = ws_stream.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                println!("Received subscription response: {}", text);
            },
            Ok(_) => println!("Received non-text message"),
            Err(e) => println!("Error receiving message: {}", e),
        }
    } else {
        println!("No response received for subscription request!");
    }
    
    // Send a ping and wait for response
    println!("Sending ping...");
    ws_stream.send(Message::Ping(vec![1, 2, 3])).await?;
    
    // Wait for some messages with a timeout
    println!("Waiting for messages (will timeout after 10 seconds)...");
    let mut timeout = tokio::time::interval(Duration::from_secs(10));
    
    loop {
        tokio::select! {
            Some(msg) = ws_stream.next() => {
                match msg {
                    Ok(Message::Text(text)) => {
                        println!("Received message: {}", text);
                    },
                    Ok(Message::Ping(_)) => {
                        println!("Received ping from server");
                        ws_stream.send(Message::Pong(vec![1, 2, 3])).await?;
                    },
                    Ok(Message::Pong(_)) => {
                        println!("Received pong from server");
                    },
                    Ok(Message::Close(frame)) => {
                        println!("Connection closed by server: {:?}", frame);
                        break;
                    },
                    Ok(_) => println!("Received other message type"),
                    Err(e) => {
                        println!("Error: {}", e);
                        break;
                    }
                }
            },
            _ = timeout.tick() => {
                println!("Timeout reached, no messages received");
                println!("Sending close frame and exiting...");
                let _ = ws_stream.close(None).await;
                break;
            }
        }
    }
    
    println!("Test completed");
    Ok(())
} 