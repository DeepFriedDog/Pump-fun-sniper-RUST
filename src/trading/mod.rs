/// Attempts to execute a sell transaction with retries based on MAX_RETRIES config
/// Uses a fast retry strategy with minimal delays
async fn execute_sell_with_retry<F, Fut, T>(&self, f: F) -> Result<T, Error>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, Error>>,
{
    let max_retries = self.config.max_retries;
    let mut attempt = 0;
    let mut last_error = None;
    
    while attempt <= max_retries {
        if attempt > 0 {
            info!("Sell retry attempt {} of {}", attempt, max_retries);
            // Fast retry with minimal backoff: 100ms, 200ms, 300ms
            let delay = 100 * attempt as u64;
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        
        match f().await {
            Ok(result) => {
                if attempt > 0 {
                    info!("Successfully completed sell transaction after {} retries", attempt);
                }
                return Ok(result);
            }
            Err(e) => {
                // Only retry on transient errors
                if self.is_transient_error(&e) {
                    warn!("Transient error during sell (attempt {}/{}): {}", 
                          attempt + 1, max_retries + 1, e);
                    last_error = Some(e);
                    attempt += 1;
                } else {
                    // Non-transient errors should fail immediately
                    error!("Non-transient error during sell: {}", e);
                    return Err(e);
                }
            }
        }
    }
    
    Err(last_error.unwrap_or_else(|| Error::custom(format!("Failed sell after {} retries", max_retries))))
}

/// Determine if an error is transient and should be retried
fn is_transient_error(&self, error: &Error) -> bool {
    // Check for common transient errors that should be retried
    match error {
        Error::Custom(msg) => {
            msg.contains("blockhash not found") || 
            msg.contains("Transaction simulation failed") ||
            msg.contains("timeout") ||
            msg.contains("Connection refused") ||
            msg.contains("Too many requests") ||
            msg.contains("rate limited") ||
            msg.contains("socket hang up") ||
            msg.contains("429") ||  // HTTP 429 Too Many Requests
            msg.contains("503")     // HTTP 503 Service Unavailable
        },
        Error::SolanaClient(e) => {
            // Check for Solana-specific transient errors
            format!("{}", e).contains("timeout") ||
            format!("{}", e).contains("blockhash not found")
        },
        _ => false
    }
}

pub async fn buy_token(&self, token_mint: &Pubkey) -> Result<Signature, Error> {
    info!("Buying token: {}", token_mint);
    
    // No retry for buy transactions - must be fast
    let (transaction, _) = self.prepare_buy_transaction(token_mint).await?;
    self.submit_transaction(&transaction).await
}

pub async fn sell_token(&self, position: &Position) -> Result<Signature, Error> {
    info!("Selling token: {}", position.token_mint);
    
    // Use retry mechanism for sell transactions
    self.execute_sell_with_retry(|| async {
        let (transaction, _) = self.prepare_sell_transaction(position).await?;
        self.submit_transaction(&transaction).await
    }).await
} 