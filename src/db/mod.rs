use anyhow::{Context, Result};
use log::info;
use rusqlite::{Connection, params};
use std::sync::{Arc, Mutex, Once};

static INIT: Once = Once::new();
static mut DB_CONNECTION: Option<Arc<Mutex<Connection>>> = None;

/// A structure representing a trade
#[derive(Debug, Clone)]
pub struct Trade {
    pub id: Option<i64>,
    pub mint: String,
    pub tokens: String,
    pub buy_price: f64,
    pub current_price: f64, // Used for calculations in monitor_tokens
    pub sell_price: f64,    // Used when selling tokens
    pub status: String,     // Used to determine if a token is pending or sold
    pub created_at: String,
    pub updated_at: String,
}

/// Initialize the database with option to reset pending trades
pub fn init_db(reset_pending: bool) -> Result<()> {
    info!("Starting database initialization...");
    
    let conn = Connection::open("./database.db")
        .context("Failed to open database file")?;
    
    info!("Successfully opened the database file: ./database.db");
    info!("Creating (or verifying) the 'trades' table...");
    
    conn.execute(
        "CREATE TABLE IF NOT EXISTS trades (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            mint TEXT NOT NULL,
            tokens TEXT NOT NULL,
            buy_price REAL,
            current_price REAL,
            sell_price REAL,
            status TEXT CHECK(status IN ('pending', 'sold')),
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    ).context("Failed to create trades table")?;
    
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
    ).context("Failed to create update trigger")?;
    
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
pub fn get_db() -> Result<Arc<Mutex<Connection>>> {
    unsafe {
        // Using `let conn_ref = DB_CONNECTION.as_ref()` to avoid the static_mut_refs warning
        // while still safely accessing the static variable
        match DB_CONNECTION.as_ref() {
            Some(conn) => Ok(Arc::clone(conn)),
            None => Err(anyhow::anyhow!("Database not initialized. Call init_db() first."))
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
        let created_at: String = row.get(7)?;
        let updated_at: String = row.get(8)?;
        
        Ok(Trade {
            id: row.get(0)?,
            mint: row.get(1)?,
            tokens: row.get(2)?,
            buy_price: row.get(3)?,
            current_price: row.get(4)?,
            sell_price: row.get(5)?,
            status: row.get(6)?,
            created_at: created_at,
            updated_at: updated_at,
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
pub fn insert_trade(mint: &str, tokens: &str, buy_price: f64) -> Result<()> {
    let db = get_db()?;
    let conn = db.lock().unwrap();
    
    conn.execute(
        "INSERT INTO trades (mint, tokens, buy_price, current_price, sell_price, status)
        VALUES (?, ?, ?, ?, ?, ?)",
        params![mint, tokens, buy_price, 0.0, 0.0, "pending"],
    )?;
    
    info!("Bought token: {} -> inserted into 'trades' (pending).", mint);
    
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
pub fn update_trade_sold(id: i64, sell_price: f64) -> Result<()> {
    let db = get_db()?;
    let conn = db.lock().unwrap();
    
    conn.execute(
        "UPDATE trades
        SET sell_price = ?,
            status = 'sold',
            updated_at = datetime('now','localtime')
        WHERE id = ?",
        params![sell_price, id],
    )?;
    
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