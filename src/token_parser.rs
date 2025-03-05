use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bs58;
use log::{debug, error, info};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use solana_program::pubkey::Pubkey;
use std::str::FromStr;

// Constants for program IDs
pub const PUMP_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
pub const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const ATA_PROGRAM_ID: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenData {
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub mint: String,
    pub bonding_curve: String,
    pub user: String,
    pub associated_curve: Option<String>,
}

/// Find the associated bonding curve for a given mint and bonding curve.
/// This uses the standard ATA derivation, similar to the Python implementation.
pub fn find_associated_bonding_curve(mint: &Pubkey, bonding_curve: &Pubkey) -> Result<Pubkey> {
    let token_program = Pubkey::from_str(TOKEN_PROGRAM_ID)?;
    let ata_program = Pubkey::from_str(ATA_PROGRAM_ID)?;

    let seeds = &[
        bonding_curve.as_ref(),
        token_program.as_ref(),
        mint.as_ref(),
    ];

    let (derived_address, _) = Pubkey::find_program_address(seeds, &ata_program);

    Ok(derived_address)
}

/// Parse a WebSocket message for token creation events
pub fn parse_websocket_message(message: &str) -> Result<Vec<TokenData>> {
    let mut tokens = Vec::new();

    // Parse JSON message
    let data: Value = serde_json::from_str(message)?;

    // Check if this is a logs notification
    if let Some(method) = data.get("method").and_then(|m| m.as_str()) {
        if method == "logsNotification" {
            // Extract logs from the notification
            if let Some(logs) = data
                .get("params")
                .and_then(|p| p.get("result"))
                .and_then(|r| r.get("value"))
                .and_then(|v| v.get("logs"))
                .and_then(|l| l.as_array())
            {
                // Check if this is a Create instruction
                let is_create = logs.iter().any(|log| {
                    log.as_str()
                        .map(|s| s.contains("Program log: Instruction: Create"))
                        .unwrap_or(false)
                });

                if is_create {
                    debug!("Found Create instruction");

                    // Extract signature for reference
                    let signature = data
                        .get("params")
                        .and_then(|p| p.get("result"))
                        .and_then(|r| r.get("value"))
                        .and_then(|v| v.get("signature"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown");

                    info!("Processing creation event with signature: {}", signature);

                    // Look for program data in logs
                    for log in logs {
                        if let Some(log_str) = log.as_str() {
                            if log_str.starts_with("Program data:") {
                                let parts: Vec<&str> = log_str.splitn(2, ": ").collect();
                                if parts.len() == 2 {
                                    if let Ok(token) = parse_program_data(parts[1], signature) {
                                        tokens.push(token);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(tokens)
}

/// Parse program data from a base64 encoded string
fn parse_program_data(encoded_data: &str, signature: &str) -> Result<TokenData> {
    // Try to decode base64 data
    let decoded_data = BASE64
        .decode(encoded_data)
        .or_else(|_| bs58::decode(encoded_data).into_vec())
        .map_err(|e| anyhow!("Failed to decode program data: {}", e))?;

    if decoded_data.len() < 8 {
        return Err(anyhow!("Program data too short"));
    }

    // Skip the 8-byte instruction discriminator
    let mut offset = 8;
    let mut parsed_data = TokenData {
        name: String::new(),
        symbol: String::new(),
        uri: String::new(),
        mint: String::new(),
        bonding_curve: String::new(),
        user: String::new(),
        associated_curve: None,
    };

    // Parse string fields (name, symbol, uri)
    for field in ["name", "symbol", "uri"].iter() {
        if offset + 4 > decoded_data.len() {
            return Err(anyhow!("Unexpected end of data parsing field {}", field));
        }

        // Read string length (little endian u32)
        let length = u32::from_le_bytes([
            decoded_data[offset],
            decoded_data[offset + 1],
            decoded_data[offset + 2],
            decoded_data[offset + 3],
        ]) as usize;
        offset += 4;

        if offset + length > decoded_data.len() {
            return Err(anyhow!("String data out of bounds for field {}", field));
        }

        // Read string data
        let str_value = String::from_utf8(decoded_data[offset..offset + length].to_vec())
            .map_err(|e| anyhow!("Invalid UTF-8 in {}: {}", field, e))?;
        offset += length;

        // Set field value
        match *field {
            "name" => parsed_data.name = str_value,
            "symbol" => parsed_data.symbol = str_value,
            "uri" => parsed_data.uri = str_value,
            _ => unreachable!(),
        }
    }

    // Parse pubkey fields (mint, bondingCurve, user)
    for field in ["mint", "bondingCurve", "user"].iter() {
        if offset + 32 > decoded_data.len() {
            return Err(anyhow!("Unexpected end of data parsing field {}", field));
        }

        // Extract 32-byte pubkey
        let pubkey_bytes = &decoded_data[offset..offset + 32];
        let pubkey_str = bs58::encode(pubkey_bytes).into_string();
        offset += 32;

        // Set field value
        match *field {
            "mint" => parsed_data.mint = pubkey_str,
            "bondingCurve" => parsed_data.bonding_curve = pubkey_str,
            "user" => parsed_data.user = pubkey_str,
            _ => unreachable!(),
        }
    }

    // Calculate associated bonding curve
    if !parsed_data.mint.is_empty() && !parsed_data.bonding_curve.is_empty() {
        let mint = Pubkey::from_str(&parsed_data.mint)?;
        let bonding_curve = Pubkey::from_str(&parsed_data.bonding_curve)?;

        if let Ok(associated_curve) = find_associated_bonding_curve(&mint, &bonding_curve) {
            parsed_data.associated_curve = Some(associated_curve.to_string());
            info!("Associated bonding curve: {}", associated_curve);
        }
    }

    // Log successful parsing
    info!(
        "Parsed token: {} ({})",
        parsed_data.name, parsed_data.symbol
    );
    info!("Mint: {}", parsed_data.mint);
    info!("User: {}", parsed_data.user);

    Ok(parsed_data)
}
