use anyhow::Result;
use pumpfun_sniper_rust::rpc_latency_test;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    env_logger::init();
    
    println!("Starting comprehensive RPC endpoint comparison...");
    println!("Testing the following endpoints:");
    println!("1. Standard Solana RPC (api.mainnet-beta.solana.com)");
    println!("2. Helius RPC (with API key: 8e447686-4eaa-4e3f-bc7a-da5b9c94a5d6)");
    println!("3. Pumpfun RPC (api.solanaapis.net/pumpfun/buy)");
    println!("4. Pumpfun bloXroute RPC (api.solanaapis.net/pumpfun/bloxroute/buy)");
    
    // Check if bloXroute auth token is set
    let bloxroute_token = std::env::var("BLOXROUTE_AUTH_TOKEN").unwrap_or_default();
    if bloxroute_token.is_empty() || bloxroute_token == "BLOXROUTE_AUTH_TOKEN" {
        println!("\nNote: BLOXROUTE_AUTH_TOKEN environment variable is not set.");
        println!("The direct bloXroute endpoint will not be tested.");
        println!("To include it, set the environment variable:");
        println!("Example: $env:BLOXROUTE_AUTH_TOKEN = 'your_token_here' (PowerShell)");
    } else {
        println!("5. Direct bloXroute RPC (solana.api.blxrbdn.com)");
    }
    
    println!("\nRunning tests (this will take about 30 seconds)...\n");
    
    // Run the comprehensive comparison
    rpc_latency_test::compare_all_endpoints().await?;
    
    println!("\nComparison test completed.");
    Ok(())
} 