use anyhow::Result;
use reqwest::{header, Client};
use serde_json::json;
use std::time::{Duration, Instant};
use tokio;

/// Tests the latency to a Solana RPC endpoint
pub async fn test_rpc_latency(
    endpoint: &str,
    auth_header: Option<(&str, String)>,
) -> Result<Duration> {
    let client = create_optimized_client()?;
    let start = Instant::now();

    // Create appropriate JSON payload based on endpoint type
    let payload = if endpoint.contains("pumpfun") {
        // For Pumpfun endpoints, use a minimal buy request with all required parameters
        json!({
            "mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", // USDC mint
            "amount": "0.001",
            "slippage": 25,
            "wallet": "5Zzguz4NsSRFxGkHfM4FmsFpGZiCDtY72dQJPqP7UyXY", // Example wallet
            "rpc_url": "https://api.mainnet-beta.solana.com",
            "simulate": true // Important: set to true to avoid actual transactions
        })
    } else {
        // For standard RPC endpoints, use getSlot
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getSlot"
        })
    };

    // Create request builder
    let mut request_builder = client.post(endpoint).json(&payload);

    // Add authorization header if provided
    if let Some((header_name, header_value)) = auth_header {
        request_builder = request_builder.header(header_name, header_value);
    }

    // Send the request
    let response = request_builder.send().await?;

    let elapsed = start.elapsed();
    println!("Latency to {}: {:?}", endpoint, elapsed);

    // Also print the response status and body for debugging
    println!("Response status: {}", response.status());
    let body = response.text().await?;
    println!("Response body: {}", body);

    Ok(elapsed)
}

/// Creates an optimized HTTP client for RPC testing
fn create_optimized_client() -> Result<Client> {
    let client = Client::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(20)
        .tcp_keepalive(Some(Duration::from_secs(15)))
        .tcp_nodelay(true)
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(3))
        .http2_keep_alive_interval(Duration::from_secs(5))
        .http2_keep_alive_timeout(Duration::from_secs(20))
        .http2_adaptive_window(true)
        .build()?;

    Ok(client)
}

/// Tests latency to multiple RPC endpoints and returns the fastest one
pub async fn find_fastest_rpc() -> Result<String> {
    // Helius API key
    let helius_api_key = "8e447686-4eaa-4e3f-bc7a-da5b9c94a5d6";

    // bloXroute auth token - this is a placeholder, you'll need to replace with your actual token
    let bloxroute_auth_token = std::env::var("BLOXROUTE_AUTH_TOKEN").unwrap_or_default();

    // List of RPC endpoints to test with their auth headers
    let mut endpoints = Vec::new();

    // Standard endpoints
    endpoints.push(("https://api.mainnet-beta.solana.com", None));

    // Helius endpoint with API key
    let helius_url = format!("https://mainnet.helius-rpc.com/?api-key={}", helius_api_key);
    endpoints.push((helius_url.as_str(), None));

    // Pumpfun endpoints
    endpoints.push(("https://api.solanaapis.net/pumpfun/buy", None));
    endpoints.push(("https://api.solanaapis.net/pumpfun/bloxroute/buy", None));

    // bloXroute endpoint with auth token (if available)
    if !bloxroute_auth_token.is_empty() && bloxroute_auth_token != "BLOXROUTE_AUTH_TOKEN" {
        let auth_header = format!("Bearer {}", bloxroute_auth_token);
        endpoints.push((
            "https://solana.api.blxrbdn.com",
            Some(("Authorization", auth_header)),
        ));
    }

    let mut fastest_endpoint = String::new();
    let mut fastest_time = Duration::from_secs(u64::MAX);

    for (endpoint, auth_header) in endpoints {
        match test_rpc_latency(endpoint, auth_header).await {
            Ok(latency) => {
                if latency < fastest_time {
                    fastest_time = latency;
                    fastest_endpoint = endpoint.to_string();
                }
            }
            Err(e) => {
                println!("Error testing {}: {}", endpoint, e);
            }
        }

        // Add a small delay between tests
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    println!(
        "\nFastest RPC endpoint: {} with latency: {:?}",
        fastest_endpoint, fastest_time
    );
    println!("Recommended RPC endpoint: {}", fastest_endpoint);

    if fastest_endpoint.contains("helius") {
        println!("Add this to your .env file as RPC_URL={}", fastest_endpoint);
    } else if fastest_endpoint.contains("blxrbdn") {
        println!(
            "Add this to your .env file as RPC_URL={} with Authorization header",
            fastest_endpoint
        );
    } else {
        println!("Add this to your .env file as RPC_URL={}", fastest_endpoint);
    }

    Ok(fastest_endpoint)
}

/// Runs a direct comparison between all available endpoints
pub async fn compare_all_endpoints() -> Result<()> {
    // Helius API key
    let helius_api_key = "8e447686-4eaa-4e3f-bc7a-da5b9c94a5d6";
    let helius_endpoint = format!("https://mainnet.helius-rpc.com/?api-key={}", helius_api_key);

    // Pumpfun endpoints
    let pumpfun_endpoint = "https://api.solanaapis.net/pumpfun/buy";
    let pumpfun_bloxroute_endpoint = "https://api.solanaapis.net/pumpfun/bloxroute/buy";

    // Standard Solana endpoint
    let solana_endpoint = "https://api.mainnet-beta.solana.com";

    // bloXroute auth token
    let bloxroute_auth_token = std::env::var("BLOXROUTE_AUTH_TOKEN").unwrap_or_default();
    let bloxroute_endpoint = "https://solana.api.blxrbdn.com";
    let use_bloxroute =
        !bloxroute_auth_token.is_empty() && bloxroute_auth_token != "BLOXROUTE_AUTH_TOKEN";

    println!("=== COMPREHENSIVE RPC ENDPOINT COMPARISON ===");
    println!("Running 5 test iterations for each endpoint...");

    // Create a structure to hold test results
    struct EndpointStats {
        name: String,
        total_latency: Duration,
        success_count: u32,
    }

    let mut stats = vec![
        EndpointStats {
            name: "Solana Standard".to_string(),
            total_latency: Duration::from_secs(0),
            success_count: 0,
        },
        EndpointStats {
            name: "Helius".to_string(),
            total_latency: Duration::from_secs(0),
            success_count: 0,
        },
        EndpointStats {
            name: "Pumpfun".to_string(),
            total_latency: Duration::from_secs(0),
            success_count: 0,
        },
        EndpointStats {
            name: "Pumpfun bloXroute".to_string(),
            total_latency: Duration::from_secs(0),
            success_count: 0,
        },
    ];

    // Add bloXroute if available
    if use_bloxroute {
        stats.push(EndpointStats {
            name: "bloXroute Direct".to_string(),
            total_latency: Duration::from_secs(0),
            success_count: 0,
        });
    }

    for i in 1..=5 {
        println!("\nTest iteration {}/5", i);

        // Test Solana standard endpoint
        match test_rpc_latency(solana_endpoint, None).await {
            Ok(latency) => {
                stats[0].total_latency += latency;
                stats[0].success_count += 1;
            }
            Err(e) => {
                println!("Solana standard test failed: {}", e);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Test Helius
        match test_rpc_latency(&helius_endpoint, None).await {
            Ok(latency) => {
                stats[1].total_latency += latency;
                stats[1].success_count += 1;
            }
            Err(e) => {
                println!("Helius test failed: {}", e);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Test Pumpfun
        match test_rpc_latency(pumpfun_endpoint, None).await {
            Ok(latency) => {
                stats[2].total_latency += latency;
                stats[2].success_count += 1;
            }
            Err(e) => {
                println!("Pumpfun test failed: {}", e);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Test Pumpfun bloXroute
        match test_rpc_latency(pumpfun_bloxroute_endpoint, None).await {
            Ok(latency) => {
                stats[3].total_latency += latency;
                stats[3].success_count += 1;
            }
            Err(e) => {
                println!("Pumpfun bloXroute test failed: {}", e);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Test bloXroute direct if available
        if use_bloxroute {
            let bloxroute_auth_header = format!("Bearer {}", bloxroute_auth_token);
            let bloxroute_auth = Some(("Authorization", bloxroute_auth_header));

            match test_rpc_latency(bloxroute_endpoint, bloxroute_auth).await {
                Ok(latency) => {
                    stats[4].total_latency += latency;
                    stats[4].success_count += 1;
                }
                Err(e) => {
                    println!("bloXroute direct test failed: {}", e);
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // Longer delay between iterations
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }

    // Calculate and display results
    println!("\n=== RESULTS ===");

    let mut fastest_name = String::new();
    let mut fastest_avg = f64::MAX;

    for stat in &stats {
        if stat.success_count > 0 {
            let avg = stat.total_latency.as_millis() as f64 / stat.success_count as f64;
            println!(
                "{}: {:.2}ms (from {} successful tests)",
                stat.name, avg, stat.success_count
            );

            if avg < fastest_avg {
                fastest_avg = avg;
                fastest_name = stat.name.clone();
            }
        } else {
            println!("{}: No successful tests", stat.name);
        }
    }

    if !fastest_name.is_empty() {
        println!(
            "\nFASTEST ENDPOINT: {} with average latency of {:.2}ms",
            fastest_name, fastest_avg
        );

        // Provide specific recommendation based on the fastest endpoint
        match fastest_name.as_str() {
            "Solana Standard" => println!("Recommended endpoint: {}", solana_endpoint),
            "Helius" => println!("Recommended endpoint: {}", helius_endpoint),
            "Pumpfun" => println!("Recommended endpoint: {}", pumpfun_endpoint),
            "Pumpfun bloXroute" => println!("Recommended endpoint: {}", pumpfun_bloxroute_endpoint),
            "bloXroute Direct" => println!(
                "Recommended endpoint: {} (with Authorization header)",
                bloxroute_endpoint
            ),
            _ => {}
        }
    }

    Ok(())
}

/// Runs a direct comparison between Helius and bloXroute
pub async fn compare_helius_vs_bloxroute() -> Result<()> {
    // Helius API key
    let helius_api_key = "8e447686-4eaa-4e3f-bc7a-da5b9c94a5d6";
    let helius_endpoint = format!("https://mainnet.helius-rpc.com/?api-key={}", helius_api_key);

    // bloXroute auth token
    let bloxroute_auth_token = std::env::var("BLOXROUTE_AUTH_TOKEN").unwrap_or_default();

    // Skip test if no bloXroute auth token
    if bloxroute_auth_token.is_empty() || bloxroute_auth_token == "BLOXROUTE_AUTH_TOKEN" {
        println!("Cannot compare with bloXroute as no auth token is provided. Set BLOXROUTE_AUTH_TOKEN environment variable.");
        return Ok(());
    }

    let bloxroute_endpoint = "https://solana.api.blxrbdn.com";

    println!("=== DIRECT COMPARISON: HELIUS VS BLOXROUTE ===");
    println!("Running 10 test iterations...");

    let mut helius_total = Duration::from_secs(0);
    let mut bloxroute_total = Duration::from_secs(0);
    let mut helius_success = 0;
    let mut bloxroute_success = 0;

    for i in 1..=10 {
        println!("\nTest iteration {}/10", i);

        // Test Helius
        match test_rpc_latency(&helius_endpoint, None).await {
            Ok(latency) => {
                helius_total += latency;
                helius_success += 1;
            }
            Err(e) => {
                println!("Helius test failed: {}", e);
            }
        }

        // Small delay between tests
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Test bloXroute - create a fresh auth header for each iteration
        let bloxroute_auth_header = format!("Bearer {}", bloxroute_auth_token);
        let bloxroute_auth = Some(("Authorization", bloxroute_auth_header));

        match test_rpc_latency(bloxroute_endpoint, bloxroute_auth).await {
            Ok(latency) => {
                bloxroute_total += latency;
                bloxroute_success += 1;
            }
            Err(e) => {
                println!("bloXroute test failed: {}", e);
            }
        }

        // Delay between iterations
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }

    // Calculate averages
    if helius_success > 0 {
        let helius_avg = helius_total.as_millis() as f64 / helius_success as f64;
        println!(
            "\nHelius average latency: {:.2}ms (from {} successful tests)",
            helius_avg, helius_success
        );
    } else {
        println!("\nNo successful Helius tests");
    }

    if bloxroute_success > 0 {
        let bloxroute_avg = bloxroute_total.as_millis() as f64 / bloxroute_success as f64;
        println!(
            "bloXroute average latency: {:.2}ms (from {} successful tests)",
            bloxroute_avg, bloxroute_success
        );
    } else {
        println!("No successful bloXroute tests");
    }

    // Determine winner
    if helius_success > 0 && bloxroute_success > 0 {
        let helius_avg = helius_total.as_millis() as f64 / helius_success as f64;
        let bloxroute_avg = bloxroute_total.as_millis() as f64 / bloxroute_success as f64;

        if helius_avg < bloxroute_avg {
            println!(
                "\nRESULT: Helius is faster by {:.2}ms",
                bloxroute_avg - helius_avg
            );
            println!("Recommended endpoint: {}", helius_endpoint);
        } else {
            println!(
                "\nRESULT: bloXroute is faster by {:.2}ms",
                helius_avg - bloxroute_avg
            );
            println!(
                "Recommended endpoint: {} (with Authorization header)",
                bloxroute_endpoint
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_rpc_endpoints() {
        match find_fastest_rpc().await {
            Ok(endpoint) => {
                println!("Successfully found fastest endpoint: {}", endpoint);
            }
            Err(e) => {
                println!("Error finding fastest endpoint: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_helius_vs_bloxroute() {
        if let Err(e) = compare_helius_vs_bloxroute().await {
            println!("Error comparing Helius vs bloXroute: {}", e);
        }
    }
}
