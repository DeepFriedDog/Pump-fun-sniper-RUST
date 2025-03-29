use anyhow::Result;
use log::{info, error};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use pumpfun_sniper::db::{self, Trade};

#[tokio::main]
async fn main() -> Result<()> {
    // Setup logging
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));
    
    info!("=== DATABASE FUNCTIONALITY TEST ===");

    // Step 1: Initialize database and reset any pending trades
    info!("Step 1: Initializing database and resetting pending trades");
    if let Err(e) = db::init_db(true).await {
        error!("Failed to initialize database: {}", e);
        return Err(e);
    }
    info!("✅ Database initialized successfully");
    
    // Step 2: Check that there are no pending trades initially
    info!("Step 2: Checking initial pending trades count");
    let count = db::count_pending_trades()?;
    info!("Initial pending trades count: {}", count);
    if count != 0 {
        error!("Expected 0 pending trades, found {}", count);
        return Err(anyhow::anyhow!("Database not in expected initial state"));
    }
    info!("✅ Initial pending trades count verified");
    
    // Step 3: Insert a test trade
    info!("Step 3: Inserting test trade");
    let test_mint = format!("TEST_MINT_{}", get_current_timestamp_ms());
    let test_tokens = "1000.0";
    let buy_price = 0.01;
    let buy_liquidity = 5.0;
    let detection_time = get_current_timestamp_ms() - 1000; // 1 second ago
    let buy_time = get_current_timestamp_ms();
    
    db::insert_trade(
        &test_mint,
        test_tokens,
        buy_price,
        buy_liquidity,
        detection_time,
        buy_time
    )?;
    info!("✅ Test trade inserted with mint: {}", test_mint);
    
    // Step 4: Verify the trade was inserted correctly
    info!("Step 4: Verifying trade was inserted correctly");
    let pending_trades = db::get_pending_trades()?;
    if pending_trades.is_empty() {
        error!("No pending trades found after insertion");
        return Err(anyhow::anyhow!("Trade insertion failed"));
    }
    
    let trade = pending_trades.iter()
        .find(|t| t.mint == test_mint)
        .ok_or_else(|| anyhow::anyhow!("Inserted trade not found"))?;
    
    info!("Found trade: id={:?}, mint={}, buy_price={}, status={}", 
        trade.id, trade.mint, trade.buy_price, trade.status);
    
    if trade.buy_price != buy_price || trade.buy_liquidity != buy_liquidity {
        error!("Trade data doesn't match what was inserted");
        return Err(anyhow::anyhow!("Trade data mismatch"));
    }
    info!("✅ Trade data verified");
    
    // Step 5: Update the trade price
    info!("Step 5: Updating trade price");
    let new_price = 0.02; // Doubled in price
    let trade_id = trade.id.unwrap();
    db::update_trade_price(trade_id, new_price)?;
    info!("✅ Trade price updated");
    
    // Step 6: Verify the price was updated
    info!("Step 6: Verifying price update");
    let updated_trade = db::get_trade_by_mint(&test_mint)?
        .ok_or_else(|| anyhow::anyhow!("Could not find trade by mint"))?;
    
    if updated_trade.current_price != new_price {
        error!("Trade price not updated correctly. Expected {}, got {}", 
               new_price, updated_trade.current_price);
        return Err(anyhow::anyhow!("Price update failed"));
    }
    info!("✅ Price update verified");
    
    // Step 7: Mark the trade as sold
    info!("Step 7: Marking trade as sold");
    let sell_price = 0.03;
    let sell_liquidity = 4.5;
    db::update_trade_sold(trade_id, sell_price, sell_liquidity)?;
    info!("✅ Trade marked as sold");
    
    // Step 8: Verify the trade status was updated to sold
    info!("Step 8: Verifying trade was marked as sold");
    let sold_trade = db::get_trade_by_mint(&test_mint)?
        .ok_or_else(|| anyhow::anyhow!("Could not find trade by mint"))?;
    
    if sold_trade.status != "sold" || sold_trade.sell_price != sell_price {
        error!("Trade not marked as sold correctly. Status: {}, Sell price: {}", 
               sold_trade.status, sold_trade.sell_price);
        return Err(anyhow::anyhow!("Trade status update failed"));
    }
    info!("✅ Trade status update verified");
    
    // Step 9: Test update_trade_sold_by_mint function with a new trade
    info!("Step 9: Testing update_trade_sold_by_mint function");
    // Insert another test trade
    let test_mint2 = format!("TEST_MINT2_{}", get_current_timestamp_ms());
    db::insert_trade(
        &test_mint2,
        test_tokens,
        buy_price,
        buy_liquidity,
        detection_time,
        buy_time
    )?;
    info!("Created second test trade with mint: {}", test_mint2);
    
    // Try to update it by mint
    match db::update_trade_sold_by_mint(&test_mint2, 0.05, 4.0, "Test sell".to_string(), 0.04) {
        Ok(_) => info!("✅ update_trade_sold_by_mint succeeded"),
        Err(e) => {
            error!("Failed to update trade by mint: {}", e);
            return Err(anyhow::anyhow!("update_trade_sold_by_mint failed: {}", e));
        }
    }
    
    // Verify the update
    let sold_trade2 = db::get_trade_by_mint(&test_mint2)?
        .ok_or_else(|| anyhow::anyhow!("Could not find second trade by mint"))?;
    
    if sold_trade2.status != "sold" || sold_trade2.sell_price != 0.05 {
        error!("Second trade not marked as sold correctly. Status: {}, Sell price: {}", 
               sold_trade2.status, sold_trade2.sell_price);
        return Err(anyhow::anyhow!("Second trade status update failed"));
    }
    info!("✅ Second trade status update verified");
    
    // Step 10: Test concurrent trade price updates (simulating price monitoring)
    info!("Step 10: Testing concurrent price updates");
    
    // Insert 5 test trades for concurrent updates
    let mut test_mints = Vec::new();
    for i in 0..5 {
        let test_mint = format!("CONCURRENT_TEST_MINT_{}", i);
        test_mints.push(test_mint.clone());
        db::insert_trade(
            &test_mint,
            test_tokens,
            buy_price,
            buy_liquidity,
            detection_time,
            buy_time
        )?;
        info!("Created concurrent test trade #{} with mint: {}", i, test_mint);
    }
    
    // Update prices concurrently
    let mut handles = Vec::new();
    for (i, mint) in test_mints.iter().enumerate() {
        let mint_clone = mint.clone();
        let handle = tokio::spawn(async move {
            // Simulate some processing time
            tokio::time::sleep(Duration::from_millis(10)).await;
            
            // Get the trade
            match db::get_trade_by_mint(&mint_clone) {
                Ok(Some(trade)) => {
                    let id = trade.id.unwrap();
                    let new_price = 0.01 * (i as f64 + 2.0); // Different price for each trade
                    
                    // Update the price
                    if let Err(e) = db::update_trade_price(id, new_price) {
                        error!("Failed to update price for trade {}: {}", mint_clone, e);
                        return Err(e);
                    }
                    
                    Ok(())
                },
                Ok(None) => {
                    error!("Could not find trade with mint {}", mint_clone);
                    Err(anyhow::anyhow!("Trade not found"))
                },
                Err(e) => {
                    error!("Error getting trade with mint {}: {}", mint_clone, e);
                    Err(e)
                }
            }
        });
        
        handles.push(handle);
    }
    
    // Wait for all updates to complete
    for handle in handles {
        if let Err(e) = handle.await? {
            error!("Concurrent update failed: {}", e);
            return Err(anyhow::anyhow!("Concurrent update failed: {}", e));
        }
    }
    info!("✅ Concurrent price updates completed successfully");
    
    // Verify all prices were updated correctly
    for (i, mint) in test_mints.iter().enumerate() {
        let trade = db::get_trade_by_mint(mint)?
            .ok_or_else(|| anyhow::anyhow!("Could not find concurrent trade by mint"))?;
        
        let expected_price = 0.01 * (i as f64 + 2.0);
        if (trade.current_price - expected_price).abs() > 0.000001 {
            error!("Trade price not updated correctly. For mint {}, expected {}, got {}", 
                   mint, expected_price, trade.current_price);
            return Err(anyhow::anyhow!("Concurrent price update verification failed"));
        }
    }
    info!("✅ All concurrent price updates verified");
    
    // Final verification - count pending trades
    let final_count = db::count_pending_trades()?;
    info!("Final pending trades count: {} (expected 5)", final_count);
    if final_count != 5 { // The 5 concurrent trades should still be pending
        error!("Expected 5 pending trades, found {}", final_count);
        return Err(anyhow::anyhow!("Final trade count mismatch"));
    }
    
    info!("🎉 ALL DATABASE TESTS PASSED! 🎉");
    info!("Database functionality for price monitoring is working correctly.");
    
    Ok(())
}

// Helper function to get current timestamp in milliseconds
fn get_current_timestamp_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_millis() as i64
} 