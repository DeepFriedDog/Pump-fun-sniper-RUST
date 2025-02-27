use solana_program::pubkey::Pubkey;
use std::str::FromStr;
use dotenv::dotenv;
use std::env;
use lazy_static::lazy_static;

// WebSocket endpoint for the token monitor
pub fn get_wss_endpoint() -> String {
    dotenv().ok(); // Load .env file if exists
    env::var("CHAINSTACK_WSS_ENDPOINT").unwrap_or_else(|_| {
        // Default to Chainstack WebSocket endpoint as fallback
        "wss://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab".to_string()
    })
}

// Default WebSocket endpoint for the token monitor
pub const WSS_ENDPOINT: &str = "wss://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab";

lazy_static! {
    // Pump.fun program ID
    pub static ref PUMP_PROGRAM_ID: Pubkey = Pubkey::from_str(
        &env::var("PUMP_PROGRAM_ID")
        .unwrap_or_else(|_| "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P".to_string())
    ).unwrap_or_else(|_| panic!("Invalid PUMP_PROGRAM_ID"));

    // Solana Token Program ID (standard constant)
    pub static ref TOKEN_PROGRAM_ID: Pubkey = Pubkey::from_str(
        "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
    ).unwrap();

    // Solana Associated Token Account Program ID (standard constant)
    pub static ref ATA_PROGRAM_ID: Pubkey = Pubkey::from_str(
        "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
    ).unwrap();
}

// Structure to hold the configuration
pub struct Config {
    pub wss_endpoint: String,
    pub pump_program_id: String,
    pub token_program_id: String,
    pub ata_program_id: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            wss_endpoint: get_wss_endpoint(),
            pump_program_id: PUMP_PROGRAM_ID.to_string(),
            token_program_id: TOKEN_PROGRAM_ID.to_string(),
            ata_program_id: ATA_PROGRAM_ID.to_string(),
        }
    }
}

// Function to load configuration from environment variables
pub fn load_config() -> Config {
    Config::default()
} 