"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.getDb = exports.initDb = void 0;
const sqlite3_1 = __importDefault(require("sqlite3"));
const sqlite_1 = require("sqlite");
let db;
async function initDb() {
    try {
        console.log('Starting database initialization...');
        db = await (0, sqlite_1.open)({
            filename: './database.db',
            driver: sqlite3_1.default.Database,
        });
        console.log('Successfully opened the database file: ./database.db');
        console.log('Creating (or verifying) the "trades" table...');
        await db.exec(`
      CREATE TABLE IF NOT EXISTS trades (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        mint TEXT NOT NULL,
        tokens TEXT NOT NULL,
        buy_price REAL,
        current_price REAL,
        sell_price REAL,
        status TEXT CHECK(status IN ('pending', 'sold')),
        created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
        updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
      );
    `);
        console.log('Table "trades" created or confirmed to exist.');
        console.log('Creating (or verifying) the trigger "update_trades_updated_at"...');
        await db.exec(`
      CREATE TRIGGER IF NOT EXISTS update_trades_updated_at
      AFTER UPDATE ON trades
      FOR EACH ROW
      BEGIN
        UPDATE trades
        SET updated_at = CURRENT_TIMESTAMP
        WHERE rowid = NEW.rowid;
      END;
    `);
        console.log('Trigger "update_trades_updated_at" created or confirmed to exist.');
        console.log('Database initialization complete.');
    }
    catch (error) {
        console.error('Failed to initialize the database:', error);
        throw error;
    }
}
exports.initDb = initDb;
function getDb() {
    if (!db) {
        throw new Error('Database not initialized. Call initDb() first.');
    }
    return db;
}
exports.getDb = getDb;
// ======= ADD THIS TO ACTUALLY RUN initDb() WHEN CALLING ts-node database.ts =======
(async function main() {
    try {
        await initDb();
    }
    catch (err) {
        console.error(err);
    }
})();
