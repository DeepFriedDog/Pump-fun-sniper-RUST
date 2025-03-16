use rusqlite::{Connection, Result};
use std::path::Path;

const DB_PATH: &str = "./database.db";

fn main() -> Result<()> {
    println!("Database Schema Fix Utility");
    println!("-------------------------");
    
    if !Path::new(DB_PATH).exists() {
        println!("Database file not found at: {}", DB_PATH);
        return Ok(());
    }
    
    println!("Connecting to database: {}", DB_PATH);
    let conn = Connection::open(DB_PATH)?;
    
    // Drop any existing temporary tables to start fresh
    conn.execute("DROP TABLE IF EXISTS trades_new", [])?;
    conn.execute("DROP TABLE IF EXISTS trades_backup", [])?;
    
    // Check if the trades table exists
    let table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='trades'",
            [],
            |row| row.get(0)
        )?;
    
    if !table_exists {
        println!("Trades table does not exist, nothing to fix.");
        return Ok(());
    }
    
    // First create a backup of the original table
    println!("Creating backup of trades table...");
    conn.execute("CREATE TABLE trades_backup AS SELECT * FROM trades", [])?;
    
    // Check if buy_liquidity column exists
    let has_buy_liquidity: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('trades') WHERE name = 'buy_liquidity'",
            [],
            |row| row.get(0)
        )?;
    
    if has_buy_liquidity == 0 {
        println!("buy_liquidity column doesn't exist - no need to fix");
        conn.execute("DROP TABLE trades_backup", [])?;
        return Ok(());
    }
    
    // Get the type of buy_liquidity
    let buy_liquidity_type: String = conn
        .query_row(
            "SELECT type FROM pragma_table_info('trades') WHERE name = 'buy_liquidity'",
            [],
            |row| row.get(0)
        )?;
    
    println!("Current buy_liquidity column type: {}", buy_liquidity_type);
    
    // Only need to fix if the type is not REAL
    if buy_liquidity_type == "REAL" {
        println!("buy_liquidity column is already REAL - no need to fix");
        conn.execute("DROP TABLE trades_backup", [])?;
        return Ok(());
    }
    
    // Copy the structure of trades table with buy_liquidity as REAL
    println!("Creating new trades table with correct buy_liquidity type...");
    
    // Get all column definitions
    let mut stmt = conn.prepare("PRAGMA table_info(trades)")?;
    let columns = stmt.query_map([], |row| {
        let id: i32 = row.get(0)?;
        let name: String = row.get(1)?;
        let type_name: String = row.get(2)?;
        let notnull: bool = row.get(3)?;
        let default_value: Option<String> = row.get(4)?;
        let pk: bool = row.get(5)?;
        
        Ok((id, name, type_name, notnull, default_value, pk))
    })?;
    
    // Build CREATE TABLE statement
    let mut create_stmt = String::from("CREATE TABLE trades_new (\n");
    
    for column in columns {
        if let Ok((id, name, mut type_name, notnull, default_value, pk)) = column {
            // Change buy_liquidity type to REAL
            if name == "buy_liquidity" {
                type_name = String::from("REAL");
            }
            
            let mut col_def = format!("  {} {}", name, type_name);
            
            if pk {
                col_def.push_str(" PRIMARY KEY");
                if name == "id" && type_name == "INTEGER" {
                    col_def.push_str(" AUTOINCREMENT");
                }
            }
            
            if notnull {
                col_def.push_str(" NOT NULL");
            }
            
            if let Some(def_val) = default_value {
                col_def.push_str(&format!(" DEFAULT {}", def_val));
            }
            
            col_def.push_str(",\n");
            create_stmt.push_str(&col_def);
        }
    }
    
    // Remove trailing comma and close parenthesis
    create_stmt.pop(); // Remove \n
    create_stmt.pop(); // Remove ,
    create_stmt.push_str("\n)");
    
    println!("Creating table with:\n{}", create_stmt);
    conn.execute(&create_stmt, [])?;
    
    // Get column names for the copy
    let mut column_names = Vec::new();
    let mut stmt = conn.prepare("PRAGMA table_info(trades)")?;
    let cols = stmt.query_map([], |row| {
        let name: String = row.get(1)?;
        Ok(name)
    })?;
    
    for col in cols {
        if let Ok(name) = col {
            column_names.push(name);
        }
    }
    
    let _cols_str = column_names.join(", ");
    
    // Copy all data with CAST for buy_liquidity
    let mut select_parts = Vec::new();
    for name in &column_names {
        if name == "buy_liquidity" {
            select_parts.push(format!("CAST({} AS REAL) AS {}", name, name));
        } else {
            select_parts.push(name.clone());
        }
    }
    
    let select_str = select_parts.join(", ");
    
    let copy_stmt = format!("INSERT INTO trades_new SELECT {} FROM trades", select_str);
    println!("Copying data with:\n{}", copy_stmt);
    
    match conn.execute(&copy_stmt, []) {
        Ok(rows) => {
            println!("Successfully copied {} rows to new table", rows);
            
            // Replace the tables
            conn.execute("DROP TABLE trades", [])?;
            conn.execute("ALTER TABLE trades_new RENAME TO trades", [])?;
            println!("Successfully replaced trades table");
            
            // Clean up
            conn.execute("DROP TABLE trades_backup", [])?;
            println!("✅ Database schema fixed successfully!");
        },
        Err(e) => {
            println!("Error copying data: {}", e);
            println!("Restoring from backup...");
            conn.execute("DROP TABLE trades", [])?;
            conn.execute("ALTER TABLE trades_backup RENAME TO trades", [])?;
            println!("Restored from backup");
            return Err(e);
        }
    }
    
    Ok(())
} 