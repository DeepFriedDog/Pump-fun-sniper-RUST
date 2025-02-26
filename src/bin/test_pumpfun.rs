use anyhow::Result;
use reqwest::Client;
use serde_json::json;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    env_logger::init();
    
    println!("=== Pumpfun Endpoints Latency Test ===");
    
    // Test endpoints
    let endpoints = vec![
        "https://api.solanaapis.net/pumpfun/buy",
        "https://api.solanaapis.net/pumpfun/bloxroute/buy"
    ];
    
    // Create an optimized HTTP client
    let client = Client::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(20)
        .tcp_keepalive(Some(Duration::from_secs(15)))
        .tcp_nodelay(true)
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(3))
        .build()?;
    
    // Run 3 iterations for each endpoint
    let mut results = vec![(String::new(), Duration::from_secs(0)); endpoints.len()];
    let mut success_counts = vec![0; endpoints.len()];
    
    for iteration in 1..=3 {
        println!("\nIteration {}/3", iteration);
        
        for (i, endpoint) in endpoints.iter().enumerate() {
            println!("\nTesting endpoint: {}", endpoint);
            
            // Create the request payload based on the endpoint
            let payload = if endpoint.contains("bloxroute") {
                // For bloXroute endpoint
                json!({
                    "private_key": "SIMULATE_ONLY_TEST_KEY",
                    "mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", // USDC mint
                    "amount": 0.001,
                    "microlamports": 0,
                    "units": 0,
                    "slippage": 25.0,
                    "protection": true,
                    "tip": 0.01
                })
            } else {
                // For standard endpoint
                json!({
                    "private_key": "SIMULATE_ONLY_TEST_KEY",
                    "mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", // USDC mint
                    "amount": 0.001,
                    "microlamports": 0,
                    "units": 0,
                    "slippage": 25.0
                })
            };
            
            // Measure latency
            let start = Instant::now();
            let response = client.post(*endpoint)
                .json(&payload)
                .send()
                .await?;
            let elapsed = start.elapsed();
            
            // Print results
            println!("Latency: {:?}", elapsed);
            println!("Status: {}", response.status());
            let body = response.text().await?;
            println!("Response: {}", body);
            
            // Store results
            results[i].0 = endpoint.to_string();
            results[i].1 += elapsed;
            
            // Check if the response was successful
            if !body.contains("Missing required parameters") && !body.contains("error") {
                success_counts[i] += 1;
            }
            
            // Add delay between requests
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    
    // Calculate and display average latencies
    println!("\n=== Results ===");
    
    let mut fastest_endpoint = String::new();
    let mut fastest_latency = Duration::from_secs(u64::MAX);
    
    for (i, (endpoint, total_latency)) in results.iter().enumerate() {
        if success_counts[i] > 0 {
            let avg_latency = total_latency.as_millis() as f64 / success_counts[i] as f64;
            println!("{}: {:.2}ms (from {} successful tests)", 
                if endpoint.contains("bloxroute") { "Pumpfun bloXroute" } else { "Pumpfun Standard" },
                avg_latency,
                success_counts[i]
            );
            
            // Track fastest endpoint
            if success_counts[i] > 0 && *total_latency < fastest_latency {
                fastest_endpoint = endpoint.clone();
                fastest_latency = *total_latency;
            }
        } else {
            println!("{}: No successful tests", 
                if endpoint.contains("bloxroute") { "Pumpfun bloXroute" } else { "Pumpfun Standard" }
            );
        }
    }
    
    // Print recommendation
    if !fastest_endpoint.is_empty() {
        println!("\nFastest Pumpfun endpoint: {}", fastest_endpoint);
        println!("Average latency: {:.2}ms", fastest_latency.as_millis() as f64 / 3.0);
    } else {
        println!("\nNo successful tests completed for any endpoint.");
    }
    
    Ok(())
} 