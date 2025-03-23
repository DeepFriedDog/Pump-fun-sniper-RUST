use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
    sysvar,
    sysvar::rent,
};
use solana_program::program_error::ProgramError;
use solana_program::native_token::LAMPORTS_PER_SOL;
use spl_associated_token_account::get_associated_token_address;
use spl_token;
use std::str::FromStr;
use log::{info, warn, error, debug};
use anyhow::{Result, anyhow};
use solana_client::rpc_client::RpcClient;
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

/// Add this constant at the top of the file, near other constants
pub const DEFAULT_DISCRIMINATOR: u64 = 16927863322537952870;

/// Add these constants at the top of the file
pub const PUMP_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
pub const PUMP_GLOBAL_ACCOUNT: &str = "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf";

/// Add these additional constants for the account addresses
pub const PUMP_GLOBAL: &str = "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf";
pub const PUMP_FEE: &str = "CebN5WGQ4jvEPvsVU4EoHEpgzq1VV7AbicfhtW4xC9iM";
pub const PUMP_EVENT_AUTHORITY: &str = "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1";
pub const SYSTEM_RENT: &str = "SysvarRent111111111111111111111111111111111";

/// Helper function to get the swap account for a bonding curve
fn get_swap_account(bonding_curve: &str) -> Result<Pubkey> {
    Pubkey::from_str("5e9vkyVZLSzB3TgXXfXcy4Bs1JxwJvLnpahKKmXGbPHQ")
        .map_err(|e| anyhow!("Failed to parse swap account: {}", e))
}

/// Helper function to get the fee account for a bonding curve
fn get_fee_account(bonding_curve: &str) -> Result<Pubkey> {
    Pubkey::from_str("9wdSZytQB3PWmBTVqPpreEff16xoec72TWVpMWc7Ly9y")
        .map_err(|e| anyhow!("Failed to parse fee account: {}", e))
}

/// Helper function to get the authority account for a bonding curve
fn get_authority_account(bonding_curve: &str) -> Result<Pubkey> {
    // Use the Event Authority constant
    Pubkey::from_str("Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1")
        .map_err(|e| anyhow!("Failed to parse authority account: {}", e))
}

/// Helper function to get the global state account for pump.fun
fn get_global_state_account(_bonding_curve: &str) -> Result<Pubkey> {
    // The global state account is a fixed value for the pump.fun program
    // Based on protocol design, not individual token
    let global_account = "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf";
    
    let global_pubkey = Pubkey::from_str(global_account)
        .map_err(|e| anyhow!("Failed to parse global account: {}", e))?;
    
    info!("Using pump.fun global account: {}", global_pubkey);
    Ok(global_pubkey)
}

/// Helper function to get the fee recipient account
fn get_fee_recipient() -> Result<Pubkey> {
    // Use the correct Fee account
    Pubkey::from_str("CebN5WGQ4jvEPvsVU4EoHEpgzq1VV7AbicfhtW4xC9iM")
        .map_err(|e| anyhow!("Failed to parse fee recipient account: {}", e))
}

/// Helper function to derive the associated bonding curve PDA
pub fn get_associated_bonding_curve(mint: &Pubkey, bonding_curve: &Pubkey, program_id: &Pubkey) -> Result<Pubkey> {
    // The associated bonding curve is an Associated Token Account (ATA)
    // Following the standard ATA derivation logic from the Python reference script
    let ata_program_id = Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL")?;
    let token_program_id = Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA")?;
    
    let seeds = &[
        bonding_curve.as_ref(),
        token_program_id.as_ref(),
        mint.as_ref(),
    ];
    
    let (pda, _) = Pubkey::find_program_address(seeds, &ata_program_id);
    
    info!("Derived associated bonding curve for mint {} and curve {}: {}", 
          mint, bonding_curve, pda);
    
    Ok(pda)
}

/// Derive the bonding curve address for a mint
pub fn get_bonding_curve_address(mint: &Pubkey, program_id: &Pubkey) -> Result<Pubkey> {
    let seeds = &[
        b"bonding-curve",  // Note the hyphen instead of underscore
        mint.as_ref(),
    ];
    
    let (pda, _) = Pubkey::find_program_address(seeds, program_id);
    
    info!("Derived bonding curve address for mint {}: {}", mint, pda);
    Ok(pda)
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct BondingCurveState {
    pub virtual_token_reserves: u64,
    pub virtual_sol_reserves: u64,
    pub real_token_reserves: u64,
    pub real_sol_reserves: u64,
    pub token_total_supply: u64,
    pub complete: bool,
}

/// Get the bonding curve state from the RPC client
pub fn get_pump_curve_state(
    rpc_client: &RpcClient,
    bonding_curve_pubkey: &Pubkey,
) -> Result<BondingCurveState> {
    let account = rpc_client.get_account(bonding_curve_pubkey)
        .map_err(|e| anyhow!("Failed to get bonding curve account: {}", e))?;
    
    let data = account.data.as_slice();
    
    // Skip 8 bytes of the discriminator
    if data.len() < 8 {
        return Err(anyhow!("Account data is too short for bonding curve state"));
    }
    
    let state_data = &data[8..];
    let state = BorshDeserialize::deserialize(&mut &state_data[..])
        .map_err(|e| anyhow!("Failed to deserialize bonding curve state: {}", e))?;
    
    Ok(state)
}

/// Calculate token price from bonding curve state
pub fn calculate_pump_curve_price(state: &BondingCurveState) -> Result<f64> {
    if state.virtual_token_reserves == 0 || state.virtual_sol_reserves == 0 {
        return Err(anyhow!("Invalid bonding curve state with zero reserves"));
    }
    
    let price = (state.virtual_sol_reserves as f64 / LAMPORTS_PER_SOL as f64) / 
                (state.virtual_token_reserves as f64 / 1_000_000.0); // 6 decimals for token
    
    Ok(price)
}

/// Get expected token amount for a given SOL amount
pub fn calculate_token_amount_for_sol(
    state: &BondingCurveState, 
    sol_amount: f64
) -> Result<u64> {
    let price = calculate_pump_curve_price(state)?;
    
    // Calculate how many tokens to buy based on SOL amount and current price
    let token_amount = (sol_amount / price) * 1_000_000.0; // 6 decimals
    
    // Ensure the calculated amount doesn't exceed u64::MAX
    if token_amount >= u64::MAX as f64 {
        // Use a high but safe value
        return Ok(u64::MAX / 2);
    }
    
    Ok(token_amount as u64)
}

/// Add this helper function to derive associated pump curve
pub fn derive_associated_pump_curve(mint: &Pubkey, bonding_curve: &Pubkey) -> Pubkey {
    // PDA derivation for associated curve based on Python implementation
    // Using the bonding curve address, token program ID, and mint as seeds
    // And finding a PDA with the Associated Token Account program

    // Get the token program ID
    let token_program_id = spl_token::id();
    
    // Use the same seeds as the Python implementation:
    // 1. Bonding curve address
    // 2. Token program ID
    // 3. Mint address
    let seeds = &[
        bonding_curve.as_ref(),
        token_program_id.as_ref(),
        mint.as_ref(),
    ];
    
    // Use the ATA program to find the PDA
    let (address, _) = Pubkey::find_program_address(
        seeds, 
        &spl_associated_token_account::id() // ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL
    );
    
    address
}

/// Create a buy instruction for a token
pub fn create_buy_instruction(
    user: &Pubkey,
    mint: &str,
    bonding_curve: &str,
    amount: f64,
    slippage: f64,
) -> anyhow::Result<(Instruction, Vec<Instruction>)> {
    // Use conservative default values for token_amount and max_sol_cost since we can't
    // call the async get_real_time_prices function directly from a sync function
    
    // Calculate SOL amount in lamports
    let amount_in_lamports = (amount * LAMPORTS_PER_SOL as f64) as u64;
    
    // Conservative default values:
    // For token amount, assume 1 trillion tokens for 0.01 SOL
    let token_amount: u64 = 1_000_000_000_000;
    
    // For max_sol_cost, assume 3x the input amount with slippage
    let multiplier = 3.0;
    let max_sol_cost = (amount_in_lamports as f64 * multiplier) as u64;
    
    info!("Using conservative values for sync instruction building:");
    info!("🔍 Instruction discriminator: {}", DEFAULT_DISCRIMINATOR);
    info!("🔄 Buy instruction data: token_amount={}, max_sol_cost={} lamports", 
         token_amount, max_sol_cost);
    
    // Get program ID for pump.fun
    let program_id = solana_program::pubkey::Pubkey::from_str(PUMP_PROGRAM_ID)
        .map_err(|_| anyhow!("Invalid program ID"))?;
    
    // Parse the mint pubkey
    let mint_pubkey = solana_program::pubkey::Pubkey::from_str(mint)
        .map_err(|_| anyhow!("Invalid mint pubkey"))?;
        
    // Parse the bonding curve pubkey
    let bonding_curve_pubkey = solana_program::pubkey::Pubkey::from_str(bonding_curve)
        .map_err(|_| anyhow!("Invalid bonding curve pubkey"))?;
        
    // Derive the associated bonding curve address
    let associated_curve_pubkey = derive_associated_pump_curve(&mint_pubkey, &bonding_curve_pubkey);
    info!("Derived associated bonding curve for mint {} and curve {}: {}", 
          mint, bonding_curve, associated_curve_pubkey);
        
    // Get the pump.fun global account
    let global_account = solana_program::pubkey::Pubkey::from_str(PUMP_GLOBAL_ACCOUNT)
        .map_err(|_| anyhow!("Invalid global account pubkey"))?;
    info!("Using pump.fun global account: {}", global_account);
    
    // Setup instructions for creating necessary accounts
    let mut setup_instructions = Vec::new();
    
    // Create instruction to initialize the associated token account for the user
    let user_token_account = get_associated_token_address(user, &mint_pubkey);
    let create_ata_ix = spl_associated_token_account::instruction::create_associated_token_account(
        user,
        user,
        &mint_pubkey,
        &spl_token::id(),
    );
    setup_instructions.push(create_ata_ix);
    
    // Create accounts for the buy instruction
    let accounts = vec![
        AccountMeta::new_readonly(Pubkey::from_str(PUMP_GLOBAL).map_err(|_| anyhow!("Invalid global pubkey"))?, false),      // Global account
        AccountMeta::new(Pubkey::from_str(PUMP_FEE).map_err(|_| anyhow!("Invalid fee pubkey"))?, false),                  // Fee recipient
        AccountMeta::new_readonly(mint_pubkey, false),      // Mint
        AccountMeta::new(bonding_curve_pubkey, false),      // Bonding curve
        AccountMeta::new(associated_curve_pubkey, false),   // Associated bonding curve
        AccountMeta::new(user_token_account, false),        // User's ATA
        AccountMeta::new(*user, true),                      // User/signer
        AccountMeta::new_readonly(solana_program::system_program::id(), false), // System program
        AccountMeta::new_readonly(spl_token::id(), false),  // Token program
        AccountMeta::new_readonly(Pubkey::from_str(SYSTEM_RENT).map_err(|_| anyhow!("Invalid rent pubkey"))?, false),      // Rent account
        AccountMeta::new_readonly(Pubkey::from_str(PUMP_EVENT_AUTHORITY).map_err(|_| anyhow!("Invalid event authority pubkey"))?, false), // Event authority
        AccountMeta::new_readonly(Pubkey::from_str(PUMP_PROGRAM_ID).map_err(|_| anyhow!("Invalid program pubkey"))?, false),     // Program ID itself
    ];
    
    // Create the buy data with discriminator and parameters
    let discriminator = u64::to_le_bytes(DEFAULT_DISCRIMINATOR);
    
    let mut buy_data = vec![];
    buy_data.extend_from_slice(&discriminator);
    buy_data.extend_from_slice(&token_amount.to_le_bytes());
    buy_data.extend_from_slice(&max_sol_cost.to_le_bytes());
    
    // Create the instruction
    let instruction = Instruction {
        program_id,
        accounts,
        data: buy_data,
    };
    
    Ok((instruction, setup_instructions))
}

/// Get real-time prices and calculate token amounts for a buy
pub async fn get_real_time_prices(
    mint: &str, 
    bonding_curve: &str, 
    user_amount: f64, 
    slippage: f64
) -> Result<(u64, u64)> {
    // Always use a fixed amount of 0.01 SOL to simplify transactions
    let amount = std::env::var("AMOUNT")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.01);
    
    info!("🔒 Using amount of {} SOL for token purchase from environment variable", amount);
    
    // For pump.fun tokens, use a minimum of 100% slippage to handle very fast price movements
    // This effectively doubles the max_sol_cost to prevent slippage errors
    let effective_slippage = if slippage < 1.0 { 1.0 } else { slippage };
    info!("🔒 Using dynamic slippage of {}% (user setting: {}%) to handle fast price movements", 
           effective_slippage * 100.0, slippage * 100.0);
    
    // Try to get token price data from multiple RPC providers
    let rpc_urls = vec![
        std::env::var("TRADER_NODE_RPC_URL").unwrap_or_else(|_| "".to_string()),
        std::env::var("RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
        "https://api.mainnet-beta.solana.com".to_string(),
    ];
    
    // Try each RPC endpoint until we get a successful response
    let mut token_amount = 0u64;
    let mut max_sol_cost = 0u64;
    let mut used_fallback = false;
    
    for rpc_url in rpc_urls.iter() {
        if rpc_url.is_empty() {
            continue;
        }
        
        match calculate_amounts(rpc_url, mint, bonding_curve, amount).await {
            Ok((token_amt, sol_cost)) => {
                token_amount = token_amt;
                // Apply the effective slippage to max_sol_cost - multiply by 3x for very fast tokens
                let slippage_multiplier = 3.0; // More aggressive multiplier for better transaction success
                max_sol_cost = ((sol_cost as f64) * (1.0 + effective_slippage * slippage_multiplier)) as u64;
                
                info!("🔄 Calculated token amount: {} tokens for {} SOL, max_sol_cost: {} lamports (with {}% effective slippage)", 
                     token_amount, amount, max_sol_cost, effective_slippage * 100.0 * slippage_multiplier);
                
                // Break the loop as we have a successful calculation
                break;
            },
            Err(e) => {
                warn!("Failed to calculate real-time prices from {}: {}", rpc_url, e);
                used_fallback = true;
                continue;
            }
        }
    }
    
    // If we couldn't get data from any RPC, use conservative fallback values
    if token_amount == 0 || max_sol_cost == 0 {
        used_fallback = true;
        // Conservative fallback - assume we can get 1 trillion tokens for 0.01 SOL
        token_amount = 1_000_000_000_000;
        
        // Set max_sol_cost to 0.03 SOL (3x the input amount)
        max_sol_cost = (amount * LAMPORTS_PER_SOL as f64 * 3.0) as u64;
        
        info!("⚠️ Using conservative fallback values: token_amount={} (for {} SOL), max_sol_cost={}", 
             token_amount, amount, max_sol_cost);
    }
    
    // Extra safety - ensure max_sol_cost has a reasonable minimum value (0.025 SOL)
    let min_sol_cost = (0.025 * LAMPORTS_PER_SOL as f64) as u64;
    if max_sol_cost < min_sol_cost {
        warn!("⚠️ Increasing max_sol_cost from {} to {} lamports to ensure transaction success", 
             max_sol_cost, min_sol_cost);
        max_sol_cost = min_sol_cost;
    }
    
    // Cap the maximum SOL cost to prevent excessive slippage
    let max_allowed_sol_cost = (0.05 * LAMPORTS_PER_SOL as f64) as u64; // 0.05 SOL maximum
    if max_sol_cost > max_allowed_sol_cost {
        warn!("⚠️ Capping max_sol_cost from {} to {} lamports to prevent excessive SOL spending", 
             max_sol_cost, max_allowed_sol_cost);
        max_sol_cost = max_allowed_sol_cost;
    }
    
    // Log the final values for debugging
    info!("🔍 Instruction discriminator: {}", DEFAULT_DISCRIMINATOR);
    info!("🔄 Buy instruction data: token_amount={} (max purchase for {} SOL), max_sol_cost={} lamports ({}% slippage)", 
         token_amount, amount, max_sol_cost, slippage * 100.0);
    
    Ok((token_amount, max_sol_cost))
}

/// Helper function to calculate token amounts and costs from an RPC endpoint
async fn calculate_amounts(rpc_url: &str, mint: &str, bonding_curve: &str, amount: f64) -> Result<(u64, u64)> {
    // Calculate SOL amount in lamports
    let amount_in_lamports = (amount * LAMPORTS_PER_SOL as f64) as u64;
    
    // Properly parse the bonding curve pubkey
    let bonding_curve_pubkey = match Pubkey::from_str(bonding_curve) {
        Ok(pubkey) => pubkey,
        Err(e) => return Err(anyhow!("Failed to parse bonding curve pubkey: {}", e)),
    };
    
    // Create RPC client with short timeout for faster fallback
    let rpc_client = RpcClient::new_with_timeout(rpc_url.to_string(), std::time::Duration::from_millis(300));
    
    // Try to get the bonding curve state
    let state = match get_pump_curve_state(&rpc_client, &bonding_curve_pubkey) {
        Ok(state) => state,
        Err(e) => return Err(anyhow!("Failed to get pump curve state: {}", e)),
    };
    
    // Calculate the token price from the bonding curve formula
    if state.virtual_token_reserves == 0 || state.virtual_sol_reserves == 0 {
        return Err(anyhow!("Invalid bonding curve state: virtual reserves are zero"));
    }
    
    // Calculate price per token using the bonding curve formula
    let price_per_token = state.virtual_sol_reserves as f64 / state.virtual_token_reserves as f64;
    
    // Calculate the token amount for the given SOL amount
    let calculated_token_amount = (amount_in_lamports as f64 / price_per_token) as u64;
    
    // Calculate the estimated SOL cost
    let estimated_sol_cost = (calculated_token_amount as f64 * price_per_token) as u64;
    
    // Log the price information for debugging
    let token_price_sol = (state.virtual_sol_reserves as f64 / LAMPORTS_PER_SOL as f64) / 
                          (state.virtual_token_reserves as f64 / 1_000_000.0);
    
    debug!("📊 Token price from RPC {}: {} SOL per token", rpc_url, token_price_sol);
    debug!("📊 Calculated token amount for {} SOL: {}", amount, calculated_token_amount);
    
    Ok((calculated_token_amount, estimated_sol_cost))
} 