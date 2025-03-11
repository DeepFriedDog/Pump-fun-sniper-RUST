use anyhow::{Context, Result};
use log::info;
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
pub fn init_db(reset_pending: bool) -> Result<()> {
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

    // If reset_pending is true, mark all pending trades as sold
    if reset_pending {
        info!("Resetting any pending trades...");
        conn.execute(
            "UPDATE trades SET status = 'sold', updated_at = CURRENT_TIMESTAMP WHERE status = 'pending'",
            [],
        ).context("Failed to reset pending trades")?;
        info!("All pending trades have been reset to 'sold'.");
    }

    info!("Database initialization complete.");

    unsafe {
        INIT.call_once(|| {
            DB_CONNECTION = Some(Arc::new(Mutex::new(conn)));
        });
    }

    Ok(())
}

/// Get a connection to the database
#[inline]
#[allow(static_mut_refs)]
pub fn get_db() -> Result<Arc<Mutex<Connection>>> {
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
    let db = get_db()?;
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
    let db = get_db()?;
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
    let db = get_db()?;
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
    let db = get_db()?;
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
    let db = get_db()?;
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
    let db = get_db()?;
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
    let db = get_db()?;
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
