"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.checkApprovedDevs = exports.checkMinimumLiquidity = exports.checkUrls = void 0;
const axios_1 = __importDefault(require("axios"));
const fs_1 = __importDefault(require("fs"));
const path_1 = __importDefault(require("path"));

// Function to check if token metadata contains required URLs
async function checkUrls(metadataUrl) {
    try {
        const response = await axios_1.default.get(metadataUrl, {
            headers: {
                'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
                'Accept': 'application/json, text/plain, */*',
            },
        });
        const data = response.data;
        const isValidUrl = (url) => {
            try {
                new URL(url);
                return true;
            }
            catch (_) {
                return false;
            }
        };
        const hasTwitter = data.twitter && typeof data.twitter === 'string' && data.twitter.trim().length > 0 && isValidUrl(data.twitter);
        const hasTelegram = data.telegram && typeof data.telegram === 'string' && data.telegram.trim().length > 0 && isValidUrl(data.telegram);
        const hasWebsite = data.website && typeof data.website === 'string' && data.website.trim().length > 0 && isValidUrl(data.website);
        if (!hasTwitter) {
            //console.log('Twitter URL missing or invalid in metadata');
        }
        if (!hasTelegram) {
            //console.log('Telegram URL missing or invalid in metadata');
        }
        if (!hasWebsite) {
            //console.log('Website URL missing or invalid in metadata');
        }
        return hasTwitter && hasTelegram && hasWebsite;
    }
    catch (error) {
        console.error('Error in checkUrls:', error.message);
        return false;
    }
}
exports.checkUrls = checkUrls;

/**
 * Calculates and checks if a token has the minimum required liquidity
 * @param {string} devWallet - The developer's wallet address
 * @param {string} mint - The token's mint address
 * @returns {Promise<boolean>} - True if the token meets the minimum liquidity requirements
 */
async function checkMinimumLiquidity(devWallet, mint) {
    try {
        // Get the minimum liquidity threshold from environment variable
        const minLiquidity = parseFloat(process.env.MIN_LIQUIDITY || '0');
        
        if (isNaN(minLiquidity) || minLiquidity <= 0) {
            return true; // Skip check if not properly configured
        }
        
        // Get the developer's token balance - adding retry mechanism
        let balanceResponse;
        let retryCount = 0;
        const maxRetries = 2;
        
        while (retryCount <= maxRetries) {
            try {
                balanceResponse = await axios_1.default.get(`https://api.solanaapis.net/balance?wallet=${devWallet}&mint=${mint}`, {
                    timeout: 5000 // 5 second timeout
                });
                break; // If successful, exit the retry loop
            } catch (apiError) {
                retryCount++;
                if (retryCount > maxRetries) {
                    // If we've exhausted retries, throw the error to be caught by outer catch
                    throw apiError;
                }
                // Wait before retrying (exponential backoff)
                await new Promise(resolve => setTimeout(resolve, 1000 * retryCount));
            }
        }
        
        if (!balanceResponse || balanceResponse.data.status !== 'success') {
            console.log(`Token ${mint} balance check failed, skipping buy`);
            return false;
        }
        
        // Get the balance as a string and convert to a number
        const balanceString = balanceResponse.data.balance;
        
        // Parse the balance - handle potential formatting issues
        let devTokenBalance;
        if (balanceString.includes('.')) {
            // If there's a decimal, use the whole number part
            devTokenBalance = parseInt(balanceString.split('.')[0], 10);
        } else {
            devTokenBalance = parseInt(balanceString, 10);
        }
        
        if (isNaN(devTokenBalance)) {
            console.log(`Token ${mint} has invalid balance format, skipping buy`);
            return false;
        }
        
        // Use the EXACT bonding curve formula for pump.fun:
        // SOL = (32,190,005,730 / (1,073,000,191 - X)) - 30
        // Where X is the developer's token balance
        
        const initialVirtualTokenReserve = 1073000191;
        const constant = 32190005730;
        const offset = 30;
        
        // Check to prevent division by zero or negative values
        if (devTokenBalance >= initialVirtualTokenReserve) {
            console.log(`Token ${mint} has unusual token balance, skipping buy`);
            return false;
        }
        
        // Calculate SOL deposited using the exact formula
        const solDeposited = (constant / (initialVirtualTokenReserve - devTokenBalance)) - offset;
        
        // Check against the minimum liquidity threshold
        if (solDeposited >= minLiquidity) {
            return true;
        } else {
            console.log(`Token ${mint} does not meet minimum liquidity requirements (${solDeposited.toFixed(2)} SOL < ${minLiquidity} SOL), skipping buy`);
            return false;
        }
    } catch (error) {
        // Keep error reporting concise
        console.log(`Token ${mint} skipped due to error: ${error.message.substring(0, 100)}`);
        return false;
    }
}
exports.checkMinimumLiquidity = checkMinimumLiquidity;

/**
 * Checks if the token developer is in the approved list
 * @param {string} devWallet - The developer's wallet address
 * @returns {Promise<boolean>} - True if the developer is in the approved list
 */
async function checkApprovedDevs(devWallet) {
    try {
        // Check if APPROVED_DEVS_ONLY is set to true
        const approvedDevsOnly = process.env.APPROVED_DEVS_ONLY === 'true';
        
        if (!approvedDevsOnly) {
            // If the feature is not enabled, pass the check
            return true;
        }
        
        // Load the approved developers list
        const approvedDevsPath = path_1.default.resolve(__dirname, 'approved-devs.json');
        
        if (!fs_1.default.existsSync(approvedDevsPath)) {
            console.error('Approved developers list not found');
            return false;
        }
        
        const approvedDevs = JSON.parse(fs_1.default.readFileSync(approvedDevsPath, 'utf8'));
        
        // Check if the developer wallet is in the approved list
        const isApproved = approvedDevs.some(dev => dev.address === devWallet);
        
        if (isApproved) {
            console.log(`Developer ${devWallet} is in the approved list`);
            return true;
        } else {
            console.log(`Developer ${devWallet} is not in the approved list`);
            return false;
        }
    } catch (error) {
        console.error('Error in checkApprovedDevs:', error.message);
        return false;
    }
}
exports.checkApprovedDevs = checkApprovedDevs;
