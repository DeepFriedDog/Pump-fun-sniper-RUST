use dotenv::dotenv;
use futures_util::{SinkExt, StreamExt};
use log::{error, info};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time;
use tokio_tungstenite::accept_async;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup logging
    env_logger::init();
    dotenv().ok();

    info!("Starting test token logs WebSocket server on localhost:8765");

    // Bind to the local address
    let addr = "127.0.0.1:8765";
    let listener = TcpListener::bind(addr).await?;
    info!("Listening on: {}", addr);

    // Wait for a single connection
    let (socket, _) = listener.accept().await?;
    info!("Client connected");

    // Upgrade the TCP stream to a WebSocket connection
    let ws_stream = accept_async(socket).await?;
    let (mut write, mut read) = ws_stream.split();

    // Handle initial subscription message
    if let Some(Ok(msg)) = read.next().await {
        if let Ok(text) = msg.to_text() {
            info!("Received subscription: {}", text);

            // Parse the subscription
            if let Ok(json_msg) = serde_json::from_str::<Value>(text) {
                // Send subscription confirmation
                if json_msg["method"] == "logsSubscribe" {
                    let id = json_msg["id"].as_u64().unwrap_or(1);
                    let subscription_id = 12345;

                    let confirmation = json!({
                        "jsonrpc": "2.0",
                        "result": subscription_id,
                        "id": id
                    });

                    write
                        .send(tokio_tungstenite::tungstenite::Message::Text(
                            confirmation.to_string(),
                        ))
                        .await?;
                    info!("Sent subscription confirmation");

                    // Wait 2 seconds
                    time::sleep(Duration::from_secs(2)).await;

                    // Send a token creation event
                    let create_event = create_token_notification(subscription_id as u64);
                    write
                        .send(tokio_tungstenite::tungstenite::Message::Text(
                            create_event.to_string(),
                        ))
                        .await?;
                    info!("Sent token creation event");

                    // Keep the connection open and handle ping/pong
                    loop {
                        tokio::select! {
                            msg = read.next() => {
                                match msg {
                                    Some(Ok(m)) => {
                                        if m.is_ping() {
                                            write.send(tokio_tungstenite::tungstenite::Message::Pong(vec![])).await?;
                                        } else if m.is_close() {
                                            break;
                                        }
                                    },
                                    Some(Err(e)) => {
                                        error!("Error: {}", e);
                                        break;
                                    },
                                    None => break,
                                }
                            },
                            _ = time::sleep(Duration::from_secs(5)) => {
                                // Every 5 seconds, send a new token creation event
                                let create_event = create_token_notification(subscription_id as u64);
                                write.send(tokio_tungstenite::tungstenite::Message::Text(create_event.to_string())).await?;
                                info!("Sent token creation event");
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn create_token_notification(subscription_id: u64) -> Value {
    // Sample log data with a token creation event
    let logs = vec![
        "Program ComputeBudget111111111111111111111111111111 invoke [1]",
        "Program ComputeBudget111111111111111111111111111111 success",
        "Program ComputeBudget111111111111111111111111111111 invoke [1]",
        "Program ComputeBudget111111111111111111111111111111 success",
        "Program ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL invoke [1]",
        "Program log: Create",
        "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA invoke [2]",
        "Program log: Instruction: GetAccountDataSize",
        "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA consumed 1569 of 92833 compute units",
        "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA success",
        "Program 11111111111111111111111111111111 invoke [2]",
        "Program 11111111111111111111111111111111 success",
        "Program log: Initialize the associated token account",
        "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA invoke [2]",
        "Program log: Instruction: InitializeImmutableOwner",
        "Program log: Please upgrade to SPL Token 2022 for immutable owner support",
        "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA consumed 1405 of 86246 compute units",
        "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA success",
        "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA invoke [2]",
        "Program log: Instruction: InitializeAccount3",
        "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA consumed 4188 of 82364 compute units",
        "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA success",
        "Program ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL consumed 21807 of 99700 compute units",
        "Program ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL success",
        "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [2]",
        "Program log: Instruction: Create",
        // This is a base64 encoded program data that represents a token creation with name "TestToken" and symbol "TEST"
        "Program data: AQAAAAAAAAATZXQUVG9rZW4AAAAAAAAM3RFU1QAAAAAAAAAAAAAAAAAAAAAAAAAA9ZRzHGkIxBHG3nDcaGCJiOKVUwqxnmS/6+kYhZrGAE+I84MCZPX2W/AYHHcWlF/QK2gMh4cZy8D5LH2Ztdwmwpr+7hb5tYNygO1+jRdynkPLJnhgCHg9m",
        "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P consumed 38427 of 74176 compute units",
        "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success",
    ];

    json!({
        "jsonrpc": "2.0",
        "method": "logsNotification",
        "params": {
            "result": {
                "context": {
                    "slot": 324778193
                },
                "value": {
                    "signature": "TestTokenCreationSignature12345",
                    "err": null,
                    "logs": logs
                }
            },
            "subscription": subscription_id
        }
    })
}
