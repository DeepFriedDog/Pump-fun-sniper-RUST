use anyhow::{Context, Result};
use log::{info, warn, error};
use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex, Once};

static INIT: Once = Once::new();
static mut DB_CONNECTION: Option<Arc<Mutex<Connection>>> = None;
const DB_PATH: &str = "./database.db";

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
pub async fn init_db(reset_pending: bool) -> Result<()> {
    info!("Starting database initialization...");

    let conn = Connection::open(DB_PATH).context("Failed to open database file")?;

    info!("Successfully opened the database file: {}", DB_PATH);
    info!("Creating (or verifying) the 'trades' table...");

    conn.execute(
        "CREATE TABLE IF NOT EXISTS trades (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            mint TEXT NOT NULL,
            tokens TEXT NOT NULL,
            buy_price REAL,
            current_price REAL,
            sell_price REAL,
            buy_liquidity REAL DEFAULT 0,
            sell_liquidity REAL DEFAULT 0,
            status TEXT CHECK(status IN ('pending', 'sold')),
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            detection_time INTEGER DEFAULT 0,
            buy_time INTEGER DEFAULT 0,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    )
    .context("Failed to create trades table")?;

    info!("Table 'trades' created or confirmed to exist.");
    info!("Creating (or verifying) the trigger 'update_trades_updated_at'...");

    conn.execute(
        "CREATE TRIGGER IF NOT EXISTS update_trades_updated_at
        AFTER UPDATE ON trades
        FOR EACH ROW
        BEGIN
            UPDATE trades
            SET updated_at = CURRENT_TIMESTAMP
            WHERE rowid = NEW.rowid;
        END;",
        [],
    )
    .context("Failed to create update trigger")?;

    info!("Trigger 'update_trades_updated_at' created or confirmed to exist.");

    // Run migration to add any missing columns
    migrate_db(&conn)?;

    // If reset_pending is true, mark all pending trades as sold
    if reset_pending {
        info!("Resetting any pending trades...");
        conn.execute(
            "UPDATE trades SET status = 'sold', updated_at = CURRENT_TIMESTAMP WHERE status = 'pending'",
            [],
        ).context("Failed to reset pending trades")?;
        info!("All pending trades have been reset to 'sold'.");
    }

    // Add this call right before the "Database initialization complete!" log message
    // Around where check_db_schema is called
    match get_db_connection() {
        Ok(conn) => {
            if let Err(e) = fix_column_types(&conn).await {
                warn!("Failed to fix column types: {}", e);
            } else {
                info!("Column types verified/fixed successfully");
            }
        },
        Err(e) => {
            warn!("Cannot fix column types: {}", e);
        }
    }

    info!("Database initialization complete!");

    unsafe {
        INIT.call_once(|| {
            DB_CONNECTION = Some(Arc::new(Mutex::new(conn)));
        });
    }

    Ok(())
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

/// Get a safe reference to the database connection
pub fn get_db_connection() -> Result<Arc<Mutex<Connection>>> {
    unsafe {
        if let Some(conn) = &DB_CONNECTION {
            Ok(Arc::clone(conn))
        } else {
            Err(anyhow::anyhow!(
                "Database not initialized. Call init_db() first."
            ))
        }
    }
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

/// Insert a new trade into the database
#[inline]
pub fn insert_trade(
    mint: &str, 
    tokens: &str, 
    buy_price: f64, 
    buy_liquidity: f64, 
    detection_time: i64, 
    buy_time: i64
) -> Result<()> {
    let db = get_db_connection()?;
    let conn = db.lock().unwrap();

    conn.execute(
        "INSERT INTO trades (mint, tokens, buy_price, current_price, sell_price, buy_liquidity, sell_liquidity, status, detection_time, buy_time)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![mint, tokens, buy_price, 0.0, 0.0, buy_liquidity, 0.0, "pending", detection_time, buy_time],
    )?;

    info!(
        "Bought token: {} -> inserted into 'trades' (pending). Detection to buy: {}ms",
        mint, buy_time - detection_time
    );

    Ok(())
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

/// Count pending trades
#[inline]
pub fn count_pending_trades() -> Result<i64> {
    let db = get_db_connection()?;
    let conn = db.lock().unwrap();

    let count: i64 = conn.query_row(
        "SELECT COUNT(*) as pendingCount FROM trades WHERE status='pending'",
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
    let conn = Connection::open(DB_PATH)?;
    
    // First, find the trade ID by mint
    let mut stmt = conn.prepare("SELECT id FROM trades WHERE mint = ? AND sold = 0 LIMIT 1")?;
    let mut rows = stmt.query(params![mint])?;
    
    if let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        
        // Now update the trade as sold
        conn.execute(
            "UPDATE trades SET sold = 1, sell_price = ?, sell_liquidity = ?, sell_time = ?, sell_reason = ? WHERE id = ?",
            params![
                sell_price,
                sell_liquidity,
                chrono::Utc::now().to_rfc3339(),
                reason,
                id
            ],
        )?;
        
        info!("Updated trade {} as sold with price: {}", id, sell_price);
        Ok(())
    } else {
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

/// Fix column type issues in the trades table
pub async fn fix_column_types(conn: &Arc<Mutex<Connection>>) -> Result<(), anyhow::Error> {
    let conn_guard = conn.lock().unwrap();
    
    info!("Fixing column types in trades table...");
    
    // Check if buy_liquidity column exists and if it's the wrong type
    let column_info = conn_guard.query_row(
        "PRAGMA table_info(trades)",
        [],
        |row| {
            let name: String = row.get(1)?;
            let type_name: String = row.get(2)?;
            Ok((name, type_name))
        }
    );
    
    if let Ok((name, type_name)) = column_info {
        if name == "buy_liquidity" && type_name != "REAL" {
            info!("Fixing buy_liquidity column type from {} to REAL...", type_name);
            
            // Create a new table with correct column types
            conn_guard.execute(
                "
                CREATE TABLE trades_new (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    mint TEXT NOT NULL,
                    name TEXT,
                    amount REAL,
                    status TEXT,
                    signature TEXT,
                    liquidity REAL,
                    price REAL,
                    trade_status TEXT,
                    detection_to_buy_ms TEXT,
                    buy_liquidity REAL,
                    created_at DATETIME,
                    updated_at DATETIME
                )
                ",
                [],
            )?;
            
            // Copy data from the old table to the new one
            conn_guard.execute(
                "
                INSERT INTO trades_new(id, mint, name, amount, status, signature, liquidity, price, trade_status, detection_to_buy_ms, buy_liquidity, created_at, updated_at)
                SELECT id, mint, name, amount, status, signature, liquidity, price, trade_status, detection_to_buy_ms, CAST(buy_liquidity AS REAL), created_at, updated_at
                FROM trades
                ",
                [],
            )?;
            
            // Drop the old table and rename the new one
            conn_guard.execute("DROP TABLE trades", [])?;
            conn_guard.execute("ALTER TABLE trades_new RENAME TO trades", [])?;
            
            info!("Column buy_liquidity type fixed successfully!");
        }
    }
    
    Ok(())
}
