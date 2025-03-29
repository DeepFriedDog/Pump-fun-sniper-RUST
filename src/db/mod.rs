use anyhow::{Context, Result};
use log::{info, warn, error};
use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex, Once};
use std::sync::atomic::{AtomicBool, Ordering};
use std::fs;

/// Database path
const DB_PATH: &str = "./database.db";

/// Returns the path to the test database file
pub fn get_test_db_path() -> &'static str {
    "./test_database.db"
}

// Initialize the Once static for the database connection
static INIT: Once = Once::new();

/// Global database connection (created once)
static mut DB_CONNECTION: Option<Arc<Mutex<Connection>>> = None;

/// A structure representing a trade
#[derive(Debug, Clone)]
pub struct Trade {
    pub id: Option<i64>,
    pub mint: String,
    pub tokens: String,
    pub buy_price: f64,
    #[allow(dead_code)] // Used for calculations in monitor_tokens but flagged as unused
    pub current_price: f64,
    #[allow(dead_code)] // Used when selling tokens but flagged as unused
    pub sell_price: f64,
    pub buy_liquidity: f64,
    pub sell_liquidity: f64,
    #[allow(dead_code)] // Used to determine if a token is pending or sold but flagged as unused
    pub status: String,
    pub created_at: String,
    pub detection_time: i64,  // Timestamp in milliseconds for token detection
    pub buy_time: i64,        // Timestamp in milliseconds for successful buy
    #[allow(dead_code)] // Used for tracking when records are updated but flagged as unused
    pub updated_at: String,
}

/// Initialize the database with option to reset pending trades
pub fn init_db(reset_pending: bool) -> Result<()> {
    info!("Initializing database, reset_pending={}", reset_pending);
    
    // Create the database directory if it doesn't exist
    let db_path = std::path::Path::new(DB_PATH);
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)
            .context("Failed to create database directory")?;
    }
    
    // Connect to the SQLite database (creates it if it doesn't exist)
    let conn = Connection::open(DB_PATH)
        .context("Failed to open or create database")?;
    
    // Enable WAL mode for improved concurrency and reliability
    match conn.execute("PRAGMA journal_mode=WAL;", []) {
        Ok(_) => {
            // Check if WAL mode was enabled successfully
            match conn.query_row("PRAGMA journal_mode;", [], |row| row.get::<_, String>(0)) {
                Ok(mode) => {
                    if mode.to_lowercase() == "wal" {
                        info!("WAL mode enabled for database");
                    } else {
                        warn!("Failed to enable WAL mode: current mode is {}", mode);
                    }
                },
                Err(e) => {
                    warn!("Failed to check journal mode: {}", e);
                }
            }
        },
        Err(e) => {
            warn!("Failed to enable WAL mode: {}", e);
        }
    }
    
    // Set synchronous mode to NORMAL for better performance
    match conn.execute("PRAGMA synchronous=NORMAL;", []) {
        Ok(_) => info!("Synchronous mode set to NORMAL"),
        Err(e) => warn!("Failed to set synchronous mode: {}", e)
    }
    
    info!("Successfully opened and configured database: {}", DB_PATH);
    
    // Create the trades table if it doesn't exist
    conn.execute(
        "CREATE TABLE IF NOT EXISTS trades (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            mint TEXT NOT NULL,
            token_name TEXT,
            tokens TEXT NOT NULL,
            buy_price REAL NOT NULL,
            current_price REAL,
            buy_liquidity REAL,
            signature TEXT,
            status TEXT CHECK(status IN ('pending', 'sold')) NOT NULL DEFAULT 'pending',
            detection_time INTEGER,
            buy_time INTEGER,
            sell_time INTEGER,
            sell_price REAL,
            profit_loss REAL,
            sold_reason TEXT,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    ).context("Failed to create trades table")?;
    
    info!("Table structure verified");
    
    // Add any missing columns
    migrate_db(&conn)?;
    
    // Reset pending trades if necessary
    if reset_pending {
        reset_pending_trades(&conn)?;
    }
    
    // Fix column types
    fix_column_types(&conn)?;
    
    // Store the connection for later use
    let db_mutex = Arc::new(Mutex::new(conn));
    unsafe {
        INIT.call_once(|| {
            DB_CONNECTION = Some(db_mutex);
        });
    }
    
    info!("Database initialization complete!");
    Ok(())
}

/// Get database connection with proper error handling
pub fn get_db_connection() -> Result<Arc<Mutex<Connection>>> {
    unsafe {
        if let Some(conn) = &DB_CONNECTION {
            Ok(Arc::clone(conn))
        } else {
            Err(anyhow::anyhow!("Database not initialized. Call init_db() first."))
        }
    }
}

/// Insert a token trade with transaction support and verification
pub fn insert_trade(
    mint: &str, 
    token_name: &str,
    buy_price: f64, 
    buy_liquidity: f64, 
    detection_time: i64, 
    buy_time: i64,
    signature: Option<&str>
) -> Result<()> {
    info!("🔄 Inserting trade for token: {} ({})", mint, token_name);
    
    // Validate inputs
    if mint.is_empty() {
        return Err(anyhow::anyhow!("Mint address cannot be empty"));
    }
    
    if buy_price <= 0.0 {
        warn!("⚠️ Buy price is zero or negative: {}", buy_price);
        // Continue anyway, don't fail the operation
    }
    
    // Get connection
    let conn = match get_db_connection() {
        Ok(c) => c,
        Err(e) => {
            error!("❌ Database connection error: {}", e);
            // Try initializing the database if not already done
            match init_db(false) {
                Ok(_) => {
                    info!("✅ Database initialized successfully");
                    match get_db_connection() {
                        Ok(c) => {
                            info!("✅ Database connection established after initialization");
                            c
                        },
                        Err(e2) => {
                            error!("❌ Failed to get database connection even after initialization: {}", e2);
                            return Err(anyhow::anyhow!("Database connection failed: {}", e2));
                        }
                    }
                },
                Err(e) => {
                    error!("❌ Failed to initialize database: {}", e);
                    return Err(anyhow::anyhow!("Failed to initialize database: {}", e));
                }
            }
        }
    };
    
    let mut conn = match conn.lock() {
        Ok(c) => c,
        Err(e) => {
            error!("❌ Failed to lock database connection: {}", e);
            return Err(anyhow::anyhow!("Failed to lock database connection: Mutex poisoned"));
        }
    };
    
    // Begin transaction for reliability
    let tx = conn.transaction().context("Failed to start transaction")?;
    
    // Insert the trade - add signature if provided
    let result = if let Some(sig) = signature {
        tx.execute(
            "INSERT INTO trades (mint, token_name, tokens, buy_price, buy_liquidity, detection_time, buy_time, signature) 
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![mint, token_name, token_name, buy_price, buy_liquidity, detection_time, buy_time, sig],
        )
    } else {
        tx.execute(
            "INSERT INTO trades (mint, token_name, tokens, buy_price, buy_liquidity, detection_time, buy_time) 
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![mint, token_name, token_name, buy_price, buy_liquidity, detection_time, buy_time],
        )
    };
    
    match result {
        Ok(_) => info!("✅ Initial database insertion successful"),
        Err(e) => {
            error!("❌ Failed to insert trade record: {}", e);
            return Err(anyhow::anyhow!("Failed to insert trade record: {}", e));
        }
    }
    
    // Commit transaction
    match tx.commit() {
        Ok(_) => info!("✅ Transaction committed successfully"),
        Err(e) => {
            error!("❌ Failed to commit transaction: {}", e);
            return Err(anyhow::anyhow!("Failed to commit transaction: {}", e));
        }
    }
    
    // Verify the insertion with retries
    let mut retries = 3;
    let mut verified = false;
    let mut count = 0;
    
    while retries > 0 && !verified {
        match conn.query_row(
            "SELECT COUNT(*) FROM trades WHERE mint = ?",
            params![mint],
            |row| row.get(0)
        ) {
            Ok(c) => {
                count = c;
                if count > 0 {
                    verified = true;
                    break;
                }
            },
            Err(e) => {
                warn!("⚠️ Verification attempt failed (retries left: {}): {}", retries, e);
                retries -= 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    
    if verified {
        // Get the inserted record ID
        let id: i64 = match conn.query_row(
            "SELECT id FROM trades WHERE mint = ? ORDER BY id DESC LIMIT 1",
            params![mint],
            |row| row.get(0)
        ) {
            Ok(id) => id,
            Err(e) => {
                warn!("⚠️ Could not get record ID, but record exists: {}", e);
                -1
            }
        };
        
        info!("✅ Successfully inserted trade for token: {} ({}) with ID {}", mint, token_name, id);
        info!("  Detection to buy time: {}ms", buy_time - detection_time);
        
        // Verify full record by querying it
        match get_trade_by_mint(mint) {
            Ok(Some(trade)) => {
                info!("✅ Full record verification successful:");
                info!("  ID: {:?}", trade.id.unwrap_or(-1));
                info!("  Name: {}", trade.tokens);
                info!("  Buy Price: {:.8}", trade.buy_price);
                info!("  Buy Time: {}", trade.buy_time);
            },
            _ => {
                warn!("⚠️ Could not verify full record details, but count verification passed");
            }
        }
        
        Ok(())
    } else {
        error!("❌ Failed to verify record insertion after {} retries", 3 - retries);
        Err(anyhow::anyhow!("Record verification failed after insertion: count = {}", count))
    }
}

/// Test function to verify database functionality
pub fn test_db_insertion() -> Result<()> {
    info!("🧪 Testing database insertion...");
    
    // Generate test data
    let test_mint = format!("TEST_TOKEN_{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs());
    let test_token_name = format!("Test Token {}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs());
    let buy_price = 0.01;
    let buy_liquidity = 5.0;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let detection_time = now - 1000; // 1 second ago
    let test_signature = format!("TEST_SIG_{}", now);
    
    // Insert test trade
    insert_trade(
        &test_mint,
        &test_token_name,
        buy_price,
        buy_liquidity,
        detection_time,
        now,
        Some(&test_signature)
    )?;
    
    // Verify insertion with a direct query
    let conn = get_db_connection()?;
    let mut conn = conn.lock().unwrap();
    
    let result: String = conn.query_row(
        "SELECT mint FROM trades WHERE mint = ? LIMIT 1",
        params![test_mint],
        |row| row.get(0)
    ).context("Failed to retrieve test record")?;
    
    if result == test_mint {
        info!("🎉 Database test successful! Record verified in database.");
        Ok(())
    } else {
        error!("❌ Database test failed: Retrieved record doesn't match");
        Err(anyhow::anyhow!("Database test failed: Data mismatch"))
    }
}

/// Migrate the database to add any missing columns
fn migrate_db(conn: &Connection) -> Result<()> {
    info!("Checking database schema for missing columns...");
    
    // Check if buy_liquidity column exists
    let has_buy_liquidity = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('trades') WHERE name = 'buy_liquidity'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap_or(0);
    
    // Add buy_liquidity column if it doesn't exist
    if has_buy_liquidity == 0 {
        info!("Adding missing column 'buy_liquidity' to trades table");
        conn.execute(
            "ALTER TABLE trades ADD COLUMN buy_liquidity REAL DEFAULT 0",
            [],
        )?;
    }
    
    // Check if sell_liquidity column exists
    let has_sell_liquidity = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('trades') WHERE name = 'sell_liquidity'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap_or(0);
    
    // Add sell_liquidity column if it doesn't exist
    if has_sell_liquidity == 0 {
        info!("Adding missing column 'sell_liquidity' to trades table");
        conn.execute(
            "ALTER TABLE trades ADD COLUMN sell_liquidity REAL DEFAULT 0",
            [],
        )?;
    }
    
    // Check if detection_time column exists
    let has_detection_time = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('trades') WHERE name = 'detection_time'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap_or(0);
    
    // Add detection_time column if it doesn't exist
    if has_detection_time == 0 {
        info!("Adding missing column 'detection_time' to trades table");
        conn.execute(
            "ALTER TABLE trades ADD COLUMN detection_time INTEGER DEFAULT 0",
            [],
        )?;
    }
    
    // Check if buy_time column exists
    let has_buy_time = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('trades') WHERE name = 'buy_time'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap_or(0);
    
    // Add buy_time column if it doesn't exist
    if has_buy_time == 0 {
        info!("Adding missing column 'buy_time' to trades table");
        conn.execute(
            "ALTER TABLE trades ADD COLUMN buy_time INTEGER DEFAULT 0",
            [],
        )?;
    }
    
    info!("Database migration complete");
    Ok(())
}

/// Reset pending trades to sold (cleanup for crashes)
fn reset_pending_trades(conn: &Connection) -> Result<()> {
    info!("Resetting any pending trades...");
    conn.execute(
        "UPDATE trades SET status = 'sold', updated_at = CURRENT_TIMESTAMP WHERE status = 'pending'",
        [],
    ).context("Failed to reset pending trades")?;
    info!("All pending trades have been reset to 'sold'.");
    Ok(())
}

/// Fix column types to ensure they are correct
fn fix_column_types(conn: &Connection) -> Result<()> {
    info!("Fixing column types in trades table...");
    
    // Check if any types need fixing
    let mut needs_fixing = false;
    
    // Function to check if a column's type matches expected type
    let check_column_type = |column: &str, expected_type: &str| -> Result<bool> {
        let mut stmt = conn.prepare("PRAGMA table_info(trades)").context("Failed to get table info")?;
        let rows = stmt.query_map([], |row| {
            let name: String = row.get(1)?;
            let type_: String = row.get(2)?;
            Ok((name, type_))
        }).context("Failed to query column types")?;
        
        for row_result in rows {
            let (name, type_) = row_result?;
            if name == column && type_ != expected_type {
                warn!("Column '{}' has type '{}', expected '{}'", column, type_, expected_type);
                return Ok(true);
            }
        }
        
        Ok(false)
    };
    
    // Check types of numeric columns
    if check_column_type("buy_price", "REAL")? { needs_fixing = true; }
    if check_column_type("current_price", "REAL")? { needs_fixing = true; }
    if check_column_type("buy_liquidity", "REAL")? { needs_fixing = true; }
    if check_column_type("sell_price", "REAL")? { needs_fixing = true; }
    if check_column_type("profit_loss", "REAL")? { needs_fixing = true; }
    if check_column_type("detection_time", "INTEGER")? { needs_fixing = true; }
    if check_column_type("buy_time", "INTEGER")? { needs_fixing = true; }
    if check_column_type("sell_time", "INTEGER")? { needs_fixing = true; }
    
    if needs_fixing {
        warn!("Some columns have incorrect types, recreating table with correct types...");
        
        // Create a backup table
        conn.execute(
            "CREATE TABLE trades_backup AS SELECT * FROM trades",
            [],
        ).context("Failed to create backup table")?;
        
        // Drop the original table
        conn.execute("DROP TABLE trades", []).context("Failed to drop trades table")?;
        
        // Recreate the table with correct types
        conn.execute(
            "CREATE TABLE trades (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                mint TEXT NOT NULL,
                token_name TEXT,
                tokens TEXT NOT NULL,
                buy_price REAL NOT NULL,
                current_price REAL,
                buy_liquidity REAL,
                signature TEXT,
                status TEXT CHECK(status IN ('pending', 'sold')) NOT NULL DEFAULT 'pending',
                detection_time INTEGER,
                buy_time INTEGER,
                sell_time INTEGER,
                sell_price REAL,
                profit_loss REAL,
                sold_reason TEXT,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        ).context("Failed to recreate trades table")?;
        
        // Copy data back
        conn.execute(
            "INSERT INTO trades SELECT * FROM trades_backup",
            [],
        ).context("Failed to restore data from backup")?;
        
        // Drop the backup table
        conn.execute("DROP TABLE trades_backup", []).context("Failed to drop backup table")?;
        
        info!("Table recreated with correct column types");
    } else {
        info!("All columns have correct types, no fixes needed.");
    }
    
    Ok(())
}

/// Get all pending trades
#[inline]
pub fn get_pending_trades() -> Result<Vec<Trade>> {
    let db = get_db_connection()?;
    let conn = db.lock().unwrap();

    let mut stmt = conn.prepare("SELECT * FROM trades WHERE status = 'pending'")?;
    let trade_iter = stmt.query_map([], |row| {
        let created_at: String = row.get(9)?;
        let updated_at: String = row.get(12)?;

        Ok(Trade {
            id: row.get(0)?,
            mint: row.get(1)?,
            tokens: row.get(2)?,
            buy_price: row.get(3)?,
            current_price: row.get(4)?,
            sell_price: row.get(5)?,
            buy_liquidity: row.get(6)?,
            sell_liquidity: row.get(7)?,
            status: row.get(8)?,
            created_at,
            detection_time: row.get(10)?,
            buy_time: row.get(11)?,
            updated_at,
        })
    })?;

    let mut trades = Vec::new();
    for trade in trade_iter {
        trades.push(trade?);
    }

    Ok(trades)
}

/// Get all trades from the database
#[inline]
pub fn get_all_trades() -> Result<Vec<Trade>> {
    let db = get_db_connection()?;
    let conn = db.lock().unwrap();

    let mut stmt = conn.prepare("SELECT * FROM trades ORDER BY created_at DESC")?;
    let trade_iter = stmt.query_map([], |row| {
        let created_at: String = row.get(9)?;
        let updated_at: String = row.get(12)?;

        Ok(Trade {
            id: row.get(0)?,
            mint: row.get(1)?,
            tokens: row.get(2)?,
            buy_price: row.get(3)?,
            current_price: row.get(4)?,
            sell_price: row.get(5)?,
            buy_liquidity: row.get(6)?,
            sell_liquidity: row.get(7)?,
            status: row.get(8)?,
            created_at,
            detection_time: row.get(10)?,
            buy_time: row.get(11)?,
            updated_at,
        })
    })?;

    let mut trades = Vec::new();
    for trade in trade_iter {
        trades.push(trade?);
    }

    Ok(trades)
}

/// Update the current price of a trade
#[inline]
pub fn update_trade_price(id: i64, current_price: f64) -> Result<()> {
    let db = get_db_connection()?;
    let conn = db.lock().unwrap();

    conn.execute(
        "UPDATE trades
        SET current_price = ?,
            updated_at = datetime('now','localtime')
        WHERE id = ?",
        params![current_price, id],
    )?;

    Ok(())
}

/// Update a trade to sold status
#[inline]
pub fn update_trade_sold(id: i64, sell_price: f64, sell_liquidity: f64) -> Result<()> {
    let db = get_db_connection()?;
    let conn = db.lock().unwrap();

    conn.execute(
        "UPDATE trades
        SET status = 'sold', sell_price = ?, sell_liquidity = ?
        WHERE id = ?",
        params![sell_price, sell_liquidity, id],
    )?;

    info!("Updated trade {} to sold with price {} and liquidity {}", id, sell_price, sell_liquidity);

    Ok(())
}

/// Count the number of pending trades
pub fn count_pending_trades() -> Result<i64> {
    // Get the database connection
    let conn = get_db_connection()?;
    
    let db_conn = conn.lock().unwrap();
    let count: i64 = db_conn.query_row(
        "SELECT COUNT(*) FROM trades WHERE status = 'pending'",
        [],
        |row| row.get(0),
    )?;
    
    Ok(count)
}

/// Clear all pending trades
#[allow(dead_code)]
pub fn clear_pending_trades() -> Result<usize> {
    let db = get_db_connection()?;
    let conn = db.lock().unwrap();

    let affected_rows = conn.execute(
        "UPDATE trades SET status = 'sold', updated_at = CURRENT_TIMESTAMP WHERE status = 'pending'",
        [],
    )?;

    if affected_rows > 0 {
        info!("Cleared {} pending trades", affected_rows);
    }

    Ok(affected_rows)
}

/// Update a trade as sold by mint address
pub fn update_trade_sold_by_mint(mint: &str, sell_price: f64, sell_liquidity: f64, reason: String, current_price: f64) -> Result<()> {
    let db = get_db_connection()?;
    let conn = db.lock().unwrap();
    
    // First, find the trade ID by mint where status is pending
    let mut stmt = conn.prepare("SELECT id FROM trades WHERE mint = ? AND status = 'pending' LIMIT 1")?;
    let mut rows = stmt.query(params![mint])?;
    
    if let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        
        // Now update the trade as sold with sell price and liquidity
        conn.execute(
            "UPDATE trades 
            SET status = 'sold', 
                sell_price = ?, 
                sell_liquidity = ?, 
                current_price = ?,
                updated_at = datetime('now','localtime')
            WHERE id = ?",
            params![
                sell_price,
                sell_liquidity,
                current_price,
                id
            ],
        )?;
        
        info!("Updated trade {} as sold with price: {} SOL, reason: {}", id, sell_price, reason);
        Ok(())
    } else {
        warn!("No pending trade found with mint {}", mint);
        Err(rusqlite::Error::QueryReturnedNoRows.into())
    }
}

/// Get a trade by its mint address
pub fn get_trade_by_mint(mint: &str) -> Result<Option<Trade>> {
    let conn = get_db_connection()?;
    let conn_guard = conn.lock().unwrap();

    let mut stmt = conn_guard.prepare("SELECT id, mint, tokens, buy_price, current_price, sell_price, buy_liquidity, sell_liquidity, status, created_at, detection_time, buy_time, updated_at FROM trades WHERE mint = ? LIMIT 1")?;
    
    let mut trade_iter = stmt.query_map(params![mint], |row| {
        Ok(Trade {
            id: row.get(0)?,
            mint: row.get(1)?,
            tokens: row.get(2)?,
            buy_price: row.get(3)?,
            current_price: row.get(4)?,
            sell_price: row.get(5)?,
            buy_liquidity: row.get(6)?,
            sell_liquidity: row.get(7)?,
            status: row.get(8)?,
            created_at: row.get(9)?,
            detection_time: row.get(10)?,
            buy_time: row.get(11)?,
            updated_at: row.get(12)?,
        })
    })?;

    if let Some(trade) = trade_iter.next() {
        return Ok(Some(trade?));
    }

    Ok(None)
}

/// Update the status of a trade in the database
pub async fn update_trade_status(mint: &str, status: &str) -> Result<()> {
    let conn = get_db_connection()?;
    let conn_guard = conn.lock().unwrap();
    
    let now = chrono::Utc::now().naive_utc().to_string();
    
    match conn_guard.execute(
        "UPDATE trades SET status = ?1, updated_at = ?2 WHERE mint = ?3",
        params![status, now, mint],
    ) {
        Ok(rows_affected) => {
            if rows_affected > 0 {
                info!("Updated status to '{}' for trade with mint {}", status, mint);
            } else {
                warn!("No trade found with mint {} to update status", mint);
            }
            Ok(())
        },
        Err(e) => {
            error!("Failed to update trade status: {}", e);
            Err(anyhow::anyhow!("Failed to update trade status: {}", e))
        }
    }
}

fn verify_database_content(conn: &Connection) -> Result<(), rusqlite::Error> {
    info!("🔍 VERIFYING DATABASE CONTENT");
    
    // Check journal mode
    match conn.query_row("PRAGMA journal_mode;", [], |row| row.get::<_, String>(0)) {
        Ok(mode) => {
            info!("📝 Database journal mode: {}", mode);
        }
        Err(e) => {
            error!("Failed to check journal mode: {}", e);
        }
    }

    // Check synchronous mode - Fix: get as integer instead of string
    match conn.query_row("PRAGMA synchronous;", [], |row| row.get::<_, i64>(0)) {
        Ok(mode) => {
            // Convert numeric value to description
            let mode_desc = match mode {
                0 => "OFF",
                1 => "NORMAL",
                2 => "FULL",
                3 => "EXTRA",
                _ => "UNKNOWN",
            };
            info!("📝 Database synchronous mode: {} ({})", mode, mode_desc);
        }
        Err(e) => {
            error!("Failed to check synchronous mode: {}", e);
        }
    }

    Ok(())
}

/// Initialize a test database separate from the main database
pub async fn init_test_db() -> Result<()> {
    info!("Initializing TEST database...");
    
    let db_path = get_test_db_path();
    
    let conn = Connection::open(db_path).context("Failed to open test database file")?;
    
    // Set up the test database with the same structure
    // Enable WAL mode for better performance and concurrency
    let wal_result = conn.pragma_update(None, "journal_mode", &"WAL");
    match wal_result {
        Ok(()) => {
            info!("Set TEST database journal mode to WAL");
        },
        Err(e) => {
            warn!("Failed to enable WAL mode for TEST database: {}", e);
        }
    }
    
    // Set synchronous mode for better reliability
    let sync_result = conn.pragma_update(None, "synchronous", &1);
    match sync_result {
        Ok(()) => {
            info!("Set TEST database synchronous mode to NORMAL");
        },
        Err(e) => {
            warn!("Failed to set synchronous mode for TEST database: {}", e);
        }
    }
    
    info!("Successfully opened the TEST database file: {}", db_path);
    
    // Create the trades table if it doesn't exist
    info!("Creating (or verifying) the 'trades' table in TEST database...");
    conn.execute(
        "CREATE TABLE IF NOT EXISTS trades (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            mint TEXT NOT NULL,
            token_name TEXT,
            tokens TEXT NOT NULL,
            buy_price REAL NOT NULL,
            current_price REAL,
            buy_liquidity REAL,
            signature TEXT,
            status TEXT CHECK(status IN ('pending', 'sold')) NOT NULL DEFAULT 'pending',
            detection_time INTEGER,
            buy_time INTEGER,
            sell_time INTEGER,
            sell_price REAL,
            profit_loss REAL,
            sold_reason TEXT,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    ).context("Failed to create trades table in TEST database")?;
    
    info!("TEST database initialized successfully!");
    
    // Store the connection in a global static for testing
    let db_mutex = Arc::new(Mutex::new(conn));
    unsafe {
        INIT.call_once(|| {
            DB_CONNECTION = Some(db_mutex);
        });
    }
    
    Ok(())
}
