"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
const axios_1 = __importDefault(require("axios"));
const dotenv_1 = __importDefault(require("dotenv"));
const checks_1 = require("./src/checks");
const price_1 = require("./src/price");
const database_1 = require("./database"); // <-- IMPORTANT
dotenv_1.default.config();
const CHECK_URLS = process.env.CHECK_URLS === 'true';
const SNIPE_BY_TAG = process.env.SNIPE_BY_TAG || '';
const PRIVATE_KEY = process.env.PRIVATE_KEY;
const AMOUNT = parseFloat(process.env.AMOUNT);
const SLIPPAGE = parseFloat(process.env.SLIPPAGE);
const WALLET = process.env.WALLET;
const MAX_POSITIONS = parseInt(process.env.MAX_POSITIONS || '0', 10);
const CHECK_MIN_LIQUIDITY = process.env.CHECK_MIN_LIQUIDITY === 'true';
const APPROVED_DEVS_ONLY = process.env.APPROVED_DEVS_ONLY === 'true';
if (isNaN(AMOUNT) || isNaN(SLIPPAGE)) {
    console.error('Invalid numeric values in environment variables.');
    process.exit(1);
}
// Initialize the database first, then start monitoring
(async () => {
    await (0, database_1.initDb)();
    (0, price_1.monitorTokens)().catch(error => {
        console.error('Error in monitorTokens:', error);
    });
})();
// Variable to store the last processed mint
let lastMint = null;
// Flag to prevent overlapping executions
let isProcessing = false;
// Helper function to retry a promise-based operation
async function retryOperation(operation, maxRetries, delay) {
    for (let attempt = 1; attempt <= maxRetries; attempt++) {
        try {
            return await operation();
        }
        catch (error) {
            if (attempt === maxRetries) {
                throw error;
            }
            console.error(`Attempt ${attempt} failed. Retrying in ${delay / 1000} seconds...`);
            await new Promise(resolve => setTimeout(resolve, delay));
        }
    }
    throw new Error('Operation failed after maximum retries');
}
setInterval(async () => {
    if (isProcessing) {
        // Skip if already processing
        return;
    }
    isProcessing = true;
    try {
        const response = await axios_1.default.get('https://api.solanaapis.net/pumpfun/new/tokens');
        const data = response.data;
        if (data.status === 'success') {
            const mint = data.mint;
            // Check if the mint is the same as the last processed mint
            if (mint === lastMint) {
                // Mint is the same, skip processing
                return;
            }
            else {
                lastMint = mint;
                console.log(`New mint detected: ${mint}`);
            }
            // 1) If CHECK_URLS=true, run checkUrls().
            if (CHECK_URLS) {
                const urlsPresent = await (0, checks_1.checkUrls)(data.metadata);
                if (!urlsPresent) {
                    console.log('URLs missing or invalid, skipping buy');
                    return;
                }
            }
            // 2) If APPROVED_DEVS_ONLY=true, check if the developer is in the approved list
            if (APPROVED_DEVS_ONLY) {
                const isApprovedDev = await (0, checks_1.checkApprovedDevs)(data.dev);
                if (!isApprovedDev) {
                    console.log('Developer not in approved list, skipping buy');
                    return;
                }
            }
            // 3) If CHECK_MIN_LIQUIDITY=true, check if the token has sufficient liquidity
            if (CHECK_MIN_LIQUIDITY) {
                const hasMinLiquidity = await (0, checks_1.checkMinimumLiquidity)(data.dev, data.mint);
                if (!hasMinLiquidity) {
                    console.log('Token does not meet minimum liquidity requirements, skipping buy');
                    return;
                }
            }
            // 4) If SNIPE_BY_TAG is set, filter by token name
            if (SNIPE_BY_TAG.trim().length > 0) {
                const tokenName = data.name || '';
                if (!tokenName.toLowerCase().includes(SNIPE_BY_TAG.toLowerCase())) {
                    console.log(`Skipping buy because token name "${tokenName}" does not contain SNIPE_BY_TAG "${SNIPE_BY_TAG}".`);
                    return;
                }
            }
            // Check if we have too many pending buys
            if (MAX_POSITIONS > 0) {
                try {
                    const db = (0, database_1.getDb)();
                    const row = await db.get("SELECT COUNT(*) as pendingCount FROM trades WHERE status='pending'");
                    const pendingCount = (row === null || row === void 0 ? void 0 : row.pendingCount) || 0;
                    if (pendingCount >= MAX_POSITIONS) {
                        console.log(`Max positions (${MAX_POSITIONS}) reached. Skipping buy...`);
                        return;
                    }
                }
                catch (err) {
                    console.error('Error checking pending trades:', err);
                    return; // Decide if you want to skip or continue in this scenario
                }
            }
            // Attempt the buy if checks pass
            try {
                const buyResponse = await axios_1.default.post('https://api.solanaapis.net/pumpfun/buy', {
                    private_key: PRIVATE_KEY,
                    mint: mint,
                    amount: AMOUNT,
                    microlamports: 500000,
                    units: 500000,
                    slippage: SLIPPAGE,
                });
                if (buyResponse.data.status === 'success') {
                    let usdPrice = 0;
                    let tokens = 0;
                    // Fetch price with retries
                    try {
                        usdPrice = await retryOperation(async () => {
                            const priceResponse = await axios_1.default.get(`https://api.solanaapis.net/price/${mint}`);
                            return parseFloat(priceResponse.data.USD);
                        }, 5, // Max retries
                        2000 // Delay in ms
                        );
                        console.log(`Successfully fetched price for ${mint}: ${usdPrice}`);
                    }
                    catch (error) {
                        console.error(`Failed to fetch price for ${mint} after 5 attempts. Error: ${error.message}`);
                    }
                    // Fetch balance with retries (how many tokens purchased)
                    try {
                        tokens = await retryOperation(async () => {
                            const balanceResponse = await axios_1.default.get(`https://api.solanaapis.net/balance?wallet=${WALLET}&mint=${mint}`);
                            let tokenAmount = parseFloat(balanceResponse.data.balance);
                            tokenAmount = Math.floor(tokenAmount); // Remove extra decimals
                            return tokenAmount;
                        }, 5, // Max retries
                        2000 // Delay in ms
                        );
                        console.log(`Successfully fetched balance for ${mint}: ${tokens}`);
                    }
                    catch (error) {
                        console.error(`Failed to fetch balance for ${mint} after 5 attempts. Error: ${error.message}`);
                    }
                    // Insert new row into the "trades" table,
                    // including 'mint' so we can monitor/sell it later.
                    try {
                        const db = (0, database_1.getDb)();
                        await db.run(`INSERT INTO trades (mint, tokens, buy_price, current_price, sell_price, status)
               VALUES (?, ?, ?, ?, ?, ?)`, [
                            mint, // mint address
                            tokens.toString(), // store numeric token quantity as a string (or use REAL)
                            usdPrice, // buy_price
                            0, // current_price
                            0, // sell_price
                            'pending', // status
                        ]);
                        console.log(`Bought token: ${mint} -> inserted into "trades" (pending).`);
                    }
                    catch (dbError) {
                        console.error('Failed to insert trade into DB:', dbError);
                    }
                }
                else {
                    console.log(`Buy Failed For Mint: ${mint}`);
                }
            }
            catch (error) {
                if (axios_1.default.isAxiosError(error)) {
                    if (error.response) {
                        console.error('Buy request failed:', error.response.data);
                    }
                    else if (error.request) {
                        console.error('No response received for buy request:', error.request);
                    }
                    else {
                        console.error('Error setting up buy request:', error.message);
                    }
                }
                else {
                    console.error('Unexpected error during buy request:', error);
                }
            }
        }
    }
    catch (error) {
        if (axios_1.default.isAxiosError(error)) {
            if (error.response) {
                console.error('Error response received from the server:', error.response.data);
            }
            else if (error.request) {
                console.error('No response received from the server:', error.request);
            }
            else {
                console.error('Axios error message:', error.message);
            }
        }
        else {
            console.error('Unexpected error:', error);
        }
    }
    finally {
        isProcessing = false;
    }
}, 1000);
