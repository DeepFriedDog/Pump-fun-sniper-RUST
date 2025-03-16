// src/bin/db_cleanup.rs
// A utility to clean up phantom positions from the database

use rusqlite::{Connection, Result};
use std::env;
use std::process;

fn main() -> Result<()> {
    println!("Database Cleanup Utility");
    println!("------------------------");
    
    // Connect to the database
    let db_path = "./database.db";
    println!("Connecting to database: {}", db_path);
    let conn = Connection::open(db_path)?;
    
    // Check if we need to delete a specific mint
    let args: Vec<String> = env::args().collect();
    if args.len() > 1 && args[1] == "--delete-mint" {
        if args.len() < 3 {
            println!("Error: Missing mint address after --delete-mint");
            process::exit(1);
        }
        
        let mint = &args[2];
        println!("Deleting trades for mint: {}", mint);
        
        let rows_affected = conn.execute(
            "DELETE FROM trades WHERE mint = ?1",
            [mint]
        )?;
        
        if rows_affected > 0 {
            println!("Success: Deleted {} trades for mint {}", rows_affected, mint);
        } else {
            println!("No trades found for mint {}", mint);
        }
        
        return Ok(());
    }
    
    // If no specific mint is provided, clean up all known phantom tokens
    println!("Cleaning up known phantom tokens...");
    
    // Add the dark phantom token and any other problematic tokens here
    let phantom_tokens = vec![
        "9Nj7Ev4TAGiy747ULaJoAkecZu3QaTYZgMBxm3xsoKWH", // dark phantom
    ];
    
    let mut total_removed = 0;
    
    for token in phantom_tokens {
        println!("Checking for token: {}", token);
        let rows_affected = conn.execute(
            "DELETE FROM trades WHERE mint = ?1",
            [token]
        )?;
        
        if rows_affected > 0 {
            println!("Removed {} entries for token {}", rows_affected, token);
            total_removed += rows_affected;
        } else {
            println!("No entries found for token {}", token);
        }
    }
    
    println!("Cleanup complete. Removed {} total phantom entries.", total_removed);
    Ok(())
} 