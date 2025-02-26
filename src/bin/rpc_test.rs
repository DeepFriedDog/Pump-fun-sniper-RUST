use anyhow::Result;
use std::env;

// Import the rpc_latency_test module from the main crate
use pumpfun_sniper_rust::rpc_latency_test;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    env_logger::init();
    
    println!("=== Solana RPC Latency Test ===");
    
    // Check if an endpoint is provided as an argument
    let args: Vec<String> = env::args().collect();
    
    if args.len() > 1 {
        // Test a specific endpoint
        let endpoint = &args[1];
        println!("Testing RPC latency to: {}", endpoint);
        
        match rpc_latency_test::test_rpc_latency(endpoint, None).await {
            Ok(latency) => {
                println!("Latency: {:?}", latency);
            },
            Err(e) => {
                println!("Error: {}", e);
            }
        }
    } else {
        // Find the fastest RPC endpoint
        println!("Testing multiple RPC endpoints to find the fastest one...");
        
        match rpc_latency_test::find_fastest_rpc().await {
            Ok(endpoint) => {
                println!("Fastest endpoint: {}", endpoint);
            },
            Err(e) => {
                println!("Error: {}", e);
            }
        }
    }
    
    Ok(())
} 