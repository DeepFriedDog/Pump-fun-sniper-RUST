use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
    sysvar,
};
use solana_program::program_error::ProgramError;
use spl_associated_token_account::get_associated_token_address;
use spl_token;
use std::str::FromStr;
use log::{info, warn, error};
use anyhow::{Result, anyhow};

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

/// Create a buy instruction for a token
pub fn create_buy_instruction(
    user: &Pubkey,
    mint: &str,
    bonding_curve: &str,
    amount: f64,
    slippage: f64,
) -> anyhow::Result<Instruction> {
    // Update to the correct program ID (verified working on mainnet)
    let program_id = Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P")?;

    info!("🧮 Creating buy instructions for mint: {}", mint);

    // Convert string to pubkeys for mint and bonding curve
    let mint_pubkey = Pubkey::from_str(mint)?;
    let bonding_curve_pubkey = Pubkey::from_str(bonding_curve)?;
    
    // Calculate token amount based on SOL amount
    let token_amount = (amount * 1_000_000.0) as u64; // 10^6 conversion
    info!("🔄 Buy instruction data: token_amount={}, max_sol_cost={}", token_amount, (token_amount as f64 * slippage) as u64);

    // Create the discriminator
    let discriminator: u64 = 16927863322537952870;
    info!("🔍 Instruction discriminator: {}", discriminator);

    // Create instruction data
    let mut data = Vec::new();
    data.extend_from_slice(&discriminator.to_le_bytes());
    data.extend_from_slice(&token_amount.to_le_bytes());
    let slippage_amount = (token_amount as f64 * slippage) as u64;
    data.extend_from_slice(&slippage_amount.to_le_bytes());

    // Get associated token account address for the user and mint
    let associated_token_account = get_associated_token_address(user, &mint_pubkey);
    
    // Get the associated bonding curve PDA
    let associated_bonding_curve = get_associated_bonding_curve(&mint_pubkey, &bonding_curve_pubkey, &program_id)?;

    // Create accounts vector for the instruction
    let accounts = vec![
        AccountMeta::new_readonly(get_global_state_account(bonding_curve)?, false),    // Global state account
        AccountMeta::new(get_fee_recipient()?, false),                    // Fee recipient (write)
        AccountMeta::new_readonly(mint_pubkey, false),                    // Mint (read)
        AccountMeta::new(bonding_curve_pubkey, false),                    // Bonding curve (write)
        AccountMeta::new(associated_bonding_curve, false),                // Associated bonding curve (write)
        AccountMeta::new(associated_token_account, false),                // User's ATA (write)
        AccountMeta::new(*user, true),                                    // User (write, signer)
        AccountMeta::new_readonly(system_program::id(), false),           // System program
        AccountMeta::new_readonly(spl_token::id(), false),                // Token program
        AccountMeta::new_readonly(sysvar::rent::id(), false),             // Rent
        AccountMeta::new_readonly(get_authority_account(bonding_curve)?, false), // Authority
        AccountMeta::new_readonly(program_id, false),                     // Program ID itself (THIS IS THE MISSING ACCOUNT)
    ];

    // Return the instruction
    Ok(Instruction {
        program_id,
        accounts,
        data,
    })
} 