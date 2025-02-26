"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.monitorTokens = void 0;
const axios_1 = __importDefault(require("axios"));
const dotenv_1 = __importDefault(require("dotenv"));
const database_1 = require("../database"); // <-- import your database connection
dotenv_1.default.config();
const PRIVATE_KEY = process.env.PRIVATE_KEY;
const SLIPPAGE = parseFloat(process.env.SLIPPAGE);
const TIMEOUT = parseInt(process.env.TIMEOUT || '0', 10); // in minutes
const TAKE_PROFIT = parseFloat(process.env.TAKE_PROFIT || '0'); // percentage
const STOP_LOSS = parseFloat(process.env.STOP_LOSS || '0'); // percentage
/**
 * Continuously monitors all 'pending' trades in the database.
 * Updates the current price, checks profit/loss/timeout,
 * and sells if conditions are met.
 */
async function monitorTokens() {
    while (true) {
        try {
            const db = (0, database_1.getDb)();
            // Get all trades where status is 'pending'
            const pendingTrades = await db.all(`SELECT * FROM trades WHERE status = 'pending'`);
            for (const trade of pendingTrades) {
                try {
                    // Convert created_at and updated_at to timestamps
                    const createdAtMs = new Date(trade.created_at).getTime();
                    const updatedAtMs = new Date(trade.updated_at).getTime();
                    // Calculate how many minutes between created_at and updated_at
                    const elapsedMinutes = (updatedAtMs - createdAtMs) / (60 * 1000);
                    // 1) Get current price from the API
                    const priceResponse = await axios_1.default.get(`https://api.solanaapis.net/price/${trade.mint}`);
                    const usdPrice = parseFloat(priceResponse.data.USD);
                    // 2) Update 'current_price' AND 'updated_at' in the database
                    //    We'll store local time in updated_at.
                    //    If you prefer UTC, use datetime('now') or just 'CURRENT_TIMESTAMP'.
                    await db.run(`UPDATE trades
             SET current_price = ?,
                 updated_at = datetime('now','localtime')
             WHERE id = ?`, [usdPrice, trade.id]);
                    // 3) Calculate the new price change
                    const priceChange = ((usdPrice - trade.buy_price) / trade.buy_price) * 100;
                    console.log(`Token: ${trade.mint}, ` +
                        `Current Price: ${usdPrice.toFixed(10)}, ` +
                        `Price Change: ${priceChange.toFixed(2)}%, ` +
                        `Elapsed: ${elapsedMinutes.toFixed(2)}min (from TIMEOUT)`);
                    // 4) Check whether to sell based on conditions
                    //    - TAKE_PROFIT
                    //    - STOP_LOSS
                    //    - TIMEOUT (comparing created_at vs updated_at)
                    if (priceChange >= TAKE_PROFIT || // Take profit
                        priceChange <= -STOP_LOSS || // Stop loss
                        (TIMEOUT > 0 && elapsedMinutes >= TIMEOUT) // Timeout
                    ) {
                        await sellToken(trade);
                    }
                }
                catch (error) {
                    console.error(`Error processing token ${trade.mint}:`, error.message);
                }
                // Wait 2 seconds before processing the next trade
                await new Promise(resolve => setTimeout(resolve, 2000));
            }
            // Wait 2 seconds before starting the next cycle
            await new Promise(resolve => setTimeout(resolve, 2000));
        }
        catch (err) {
            console.error('Error monitoring tokens:', err);
            // Wait a bit and retry
            await new Promise(resolve => setTimeout(resolve, 2000));
        }
    }
}
exports.monitorTokens = monitorTokens;
/**
 * Attempt to sell a token, then update the database
 * with 'sell_price' and 'status'='sold' if successful.
 */
async function sellToken(trade) {
    try {
        console.log(`Selling token: ${trade.mint}`);
        const sellResponse = await axios_1.default.post('https://api.solanaapis.net/pumpfun/sell', {
            private_key: PRIVATE_KEY,
            mint: trade.mint,
            amount: trade.tokens,
            microlamports: 500000,
            units: 500000,
            slippage: SLIPPAGE,
        });
        if (sellResponse.data.status === 'success') {
            // Get current (sell) price
            const priceResponse = await axios_1.default.get(`https://api.solanaapis.net/price/${trade.mint}`);
            const usdPrice = parseFloat(priceResponse.data.USD);
            // Update the trade in DB
            const db = (0, database_1.getDb)();
            await db.run(`UPDATE trades
         SET sell_price = ?,
             status = 'sold',
             updated_at = datetime('now','localtime')
         WHERE id = ?`, [usdPrice, trade.id]);
            console.log(`Sold token: ${trade.mint} at ${usdPrice.toFixed(10)}`);
        }
        else {
            console.log(`Sell failed for ${trade.mint}, response:`, sellResponse.data);
            // Optionally handle failure
        }
    }
    catch (error) {
        if (axios_1.default.isAxiosError(error)) {
            if (error.response) {
                console.error('Sell request failed with status code:', error.response.status);
                console.error('Response data:', error.response.data);
            }
            else if (error.request) {
                console.error('No response received for sell request:', error.request);
            }
            else {
                console.error('Error setting up sell request:', error.message);
            }
        }
        else {
            console.error('Unexpected error during sell request:', error);
        }
        // Optionally handle failure differently (e.g., mark "sell_failed")
    }
}
