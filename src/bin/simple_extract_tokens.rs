use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use dotenv::dotenv;
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use serde_json::{json, Value};
use std::env;
use std::time::Duration;
use tokio::time;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;

// Constants
const PUMP_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
const DEFAULT_WS_ENDPOINT: &str = "ws://127.0.0.1:8765";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    env_logger::init();
    dotenv().ok();

    // Get WebSocket endpoint from environment or use default
    let ws_endpoint =
        env::var("CHAINSTACK_WSS_ENDPOINT").unwrap_or_else(|_| DEFAULT_WS_ENDPOINT.to_string());
    info!("Using WebSocket endpoint: {}", ws_endpoint);

    // Connect to WebSocket
    let url = Url::parse(&ws_endpoint)?;
    let (ws_stream, _) = connect_async(url).await?;
    info!("Connected to WebSocket server");

    let (mut write, mut read) = ws_stream.split();

    // Subscribe to logs for the Pump.fun program
    let subscription_request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "logsSubscribe",
        "params": [
            {"mentions": [PUMP_PROGRAM]},
            {"commitment": "processed"}
        ]
    });

    write
        .send(Message::Text(subscription_request.to_string()))
        .await?;
    info!("Sent subscription request for program: {}", PUMP_PROGRAM);

    // Process messages
    let mut ping_interval = time::interval(Duration::from_secs(10));

    loop {
        tokio::select! {
            Some(msg) = read.next() => {
                match msg {
                    Ok(msg) => {
                        if msg.is_text() {
                            let text = msg.to_text()?;
                            debug!("Received message: {}", text);

                            if let Ok(json_msg) = serde_json::from_str::<Value>(text) {
                                // Handle subscription confirmation
                                if json_msg["id"] == 1 && json_msg.get("result").is_some() {
                                    info!("Subscription confirmed with ID: {:?}", json_msg["result"].as_u64());
                                }

                                // Handle log notifications
                                if json_msg["method"] == "logsNotification" {
                                    process_logs_notification(&json_msg);
                                }
                            }
                        } else if msg.is_ping() {
                            debug!("Received ping, responding with pong");
                            write.send(Message::Pong(vec![])).await?;
                        } else if msg.is_pong() {
                            debug!("Received pong");
                        } else if msg.is_binary() {
                            debug!("Received binary message of size: {}", msg.into_data().len());
                        } else if msg.is_close() {
                            warn!("WebSocket connection closed by server");
                            break;
                        }
                    },
                    Err(e) => {
                        error!("Error receiving message: {}", e);
                        break;
                    }
                }
            },
            _ = ping_interval.tick() => {
                debug!("Sending ping to keep connection alive");
                write.send(Message::Ping(vec![1, 2, 3])).await?;
            }
        }
    }

    Ok(())
}

fn process_logs_notification(notification: &Value) {
    if let Some(logs) = notification["params"]["result"]["value"]["logs"].as_array() {
        let signature = notification["params"]["result"]["value"]["signature"]
            .as_str()
            .unwrap_or("unknown");
        debug!("Processing logs for transaction: {}", signature);

        // Check for Create instruction in Pump.fun program
        let mut found_create = false;
        let mut program_data: Option<String> = None;

        for log in logs {
            let log_str = log.as_str().unwrap_or("");

            // Look for the Create instruction in the Pump.fun program
            if log_str.contains(&format!("Program {} invoke", PUMP_PROGRAM)) {
                debug!("Found Pump.fun program invocation");
            } else if log_str.contains("Program log: Instruction: Create")
                || (log_str.contains("Program log: ") && log_str.contains("Create"))
            {
                debug!("Found Create instruction");
                found_create = true;
            } else if found_create && log_str.starts_with("Program data: ") {
                // Extract program data
                program_data = Some(log_str.replace("Program data: ", ""));
                debug!("Found program data: {}", program_data.as_ref().unwrap());
                break;
            }
        }

        // Parse token data if program data is found
        if let Some(data) = program_data {
            match parse_token_data(&data) {
                Ok(token) => {
                    info!("Detected new token: {} ({})", token.name, token.symbol);
                    info!("Mint address: {}", token.mint);
                    if let Some(uri) = token.uri {
                        info!("URI: {}", uri);
                    }
                }
                Err(e) => {
                    error!("Failed to parse token data: {}", e);
                }
            }
        }
    }
}

struct TokenData {
    name: String,
    symbol: String,
    mint: String,
    uri: Option<String>,
}

fn parse_token_data(program_data: &str) -> Result<TokenData, Box<dyn std::error::Error>> {
    // Decode base64 data
    let data = BASE64.decode(program_data)?;

    // The format is specific to Pump.fun token creation data
    // This is a simplified parser - actual parsing might be more complex
    if data.len() < 20 {
        return Err("Data too short".into());
    }

    // Skip first byte (instruction discriminator)
    let mut offset = 1;

    // Name length (u32 little endian)
    let name_len = u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]) as usize;
    offset += 4;

    // Name
    let name_end = offset + name_len;
    if name_end > data.len() {
        return Err("Invalid name length".into());
    }
    let name = String::from_utf8_lossy(&data[offset..name_end]).to_string();
    offset = name_end;

    // Symbol length (u32 little endian)
    let symbol_len = u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]) as usize;
    offset += 4;

    // Symbol
    let symbol_end = offset + symbol_len;
    if symbol_end > data.len() {
        return Err("Invalid symbol length".into());
    }
    let symbol = String::from_utf8_lossy(&data[offset..symbol_end]).to_string();
    offset = symbol_end;

    // URI length (u32 little endian)
    let uri_len = u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]) as usize;
    offset += 4;

    // URI (optional)
    let uri = if uri_len > 0 {
        let uri_end = offset + uri_len;
        if uri_end > data.len() {
            return Err("Invalid URI length".into());
        }
        Some(String::from_utf8_lossy(&data[offset..uri_end]).to_string())
    } else {
        None
    };

    // Mint address is the last 32 bytes (assuming it's at the end of the data)
    let mint_offset = data.len() - 32;
    if mint_offset >= data.len() {
        return Err("Data too short for mint address".into());
    }
    let mint = bs58::encode(&data[mint_offset..]).into_string();

    Ok(TokenData {
        name,
        symbol,
        mint,
        uri,
    })
}
