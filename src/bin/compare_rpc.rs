use anyhow::Result;
use pumpfun_sniper_rust::rpc_latency_test;

#[tokio::main]
async fn main() -> Result<()> {
    println!("Starting RPC comparison test between Helius and bloXroute...");
    println!("Using Helius API key: 8e447686-4eaa-4e3f-bc7a-da5b9c94a5d6");
    
    // Check if bloXroute auth token is set
    let bloxroute_token = std::env::var("BLOXROUTE_AUTH_TOKEN").unwrap_or_default();
    if bloxroute_token.is_empty() || bloxroute_token == "BLOXROUTE_AUTH_TOKEN" {
        println!("\nWARNING: BLOXROUTE_AUTH_TOKEN environment variable is not set.");
        println!("The test will only run with Helius. To compare with bloXroute, set the environment variable.");
        println!("Example: $env:BLOXROUTE_AUTH_TOKEN = 'your_token_here' (PowerShell)");
        println!("Example: export BLOXROUTE_AUTH_TOKEN='your_token_here' (Bash)\n");
    } else {
        println!("bloXroute auth token is set. Will compare both endpoints.");
    }
    
    // Run the comparison
    rpc_latency_test::compare_helius_vs_bloxroute().await?;
    
    println!("\nComparison test completed.");
    Ok(())
} 