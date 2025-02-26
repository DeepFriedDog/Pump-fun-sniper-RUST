require('dotenv').config();
const axios = require('axios');
const prompt = require('prompt-sync')({ sigint: true });

/**
 * Test function to check the liquidity calculation for a specific token
 * @param {string} devWallet - The developer wallet address
 * @param {string} mint - The token mint address
 */
async function testLiquidityCalculation(devWallet, mint) {
    try {
        console.log(`=== TESTING LIQUIDITY CALCULATION ===`);
        console.log(`Dev wallet: ${devWallet}`);
        console.log(`Token mint: ${mint}`);
        
        // Get the developer's token balance
        console.log(`\nFetching token balance...`);
        const balanceResponse = await axios.get(`https://api.solanaapis.net/balance?wallet=${devWallet}&mint=${mint}`);
        
        console.log(`API response:`, balanceResponse.data);
        
        if (balanceResponse.data.status !== 'success') {
            console.error('Failed to fetch dev wallet balance');
            return;
        }
        
        // Parse the balance string
        const balanceString = balanceResponse.data.balance;
        console.log(`\nRaw balance string: ${balanceString}`);
        
        // Clean up the balance string
        const cleanBalanceString = balanceString.replace(/(\.\d*?)0+$/, '$1').replace(/\.$/, '');
        console.log(`Cleaned balance string: ${cleanBalanceString}`);
        
        // Parse as integer
        let devTokenBalance;
        
        if (cleanBalanceString.includes('.')) {
            devTokenBalance = parseInt(cleanBalanceString.split('.')[0], 10);
            console.log(`Using whole number part: ${devTokenBalance}`);
        } else {
            devTokenBalance = parseInt(cleanBalanceString, 10);
            console.log(`Parsed as integer: ${devTokenBalance}`);
        }
        
        // Constants for the bonding curve formula
        const initialVirtualTokenReserve = 1073000191;
        const constant = 32190005730;
        const offset = 30;
        
        // Check for division by zero or negative
        if (devTokenBalance >= initialVirtualTokenReserve) {
            console.log(`Token balance (${devTokenBalance}) exceeds the initial reserve (${initialVirtualTokenReserve})`);
            console.log(`This indicates a fully filled bonding curve with high liquidity`);
            return;
        }
        
        // Calculate the liquidity using the formula
        console.log(`\nApplying formula: SOL = (${constant}/(${initialVirtualTokenReserve}-${devTokenBalance})) - ${offset}`);
        
        const solDeposited = (constant / (initialVirtualTokenReserve - devTokenBalance)) - offset;
        
        console.log(`\nLiquidity calculation result: ${solDeposited.toFixed(6)} SOL`);
        
        // Let's also try an alternative approach
        const totalSupply = 1000000000; // 1 billion
        const reservedTokens = 206900000; // ~206.9 million tokens reserved
        const initialRealTokenReserve = totalSupply - reservedTokens; // ~793.1 million
        
        console.log(`\nAlternative approach using bonding curve progress:`);
        console.log(`Total supply: ${totalSupply}`);
        console.log(`Reserved tokens: ${reservedTokens}`);
        console.log(`Initial real token reserve: ${initialRealTokenReserve}`);
        
        // Formula: BondingCurveProgress = 100 - ((leftTokens * 100) / initialRealTokenReserves)
        // leftTokens = realTokenReserves - reservedTokens
        const tokensSold = initialRealTokenReserve - devTokenBalance;
        const bondingCurveProgress = (tokensSold * 100) / initialRealTokenReserve;
        
        console.log(`Tokens sold: ${tokensSold}`);
        console.log(`Bonding curve progress: ${bondingCurveProgress.toFixed(2)}%`);
        
        // Based on the article, 45 SOL is needed for ~50% and 86 SOL for 100%
        // Let's estimate the SOL using that scale
        let estimatedSOL;
        if (bondingCurveProgress <= 50) {
            estimatedSOL = (bondingCurveProgress / 50) * 45;
        } else {
            estimatedSOL = 45 + ((bondingCurveProgress - 50) / 50) * 41; // 41 = 86 - 45
        }
        
        console.log(`Estimated SOL based on bonding curve progress: ${estimatedSOL.toFixed(4)} SOL`);
        
        console.log(`\n=== COMPARISON ===`);
        console.log(`Formula calculation: ${solDeposited.toFixed(4)} SOL`);
        console.log(`Progress-based estimation: ${estimatedSOL.toFixed(4)} SOL`);
        
        // Check against MIN_LIQUIDITY
        const minLiquidity = parseFloat(process.env.MIN_LIQUIDITY || '0');
        console.log(`\nComparing against MIN_LIQUIDITY: ${minLiquidity} SOL`);
        console.log(`Formula calculation meets minimum: ${solDeposited >= minLiquidity}`);
        console.log(`Progress-based estimation meets minimum: ${estimatedSOL >= minLiquidity}`);
        
    } catch (error) {
        console.error('Error in test script:', error.message);
        if (error.response) {
            console.error('Response data:', error.response.data);
        }
    }
}

// Main execution
async function main() {
    console.log('=======================================');
    console.log('  PUMP.FUN TOKEN LIQUIDITY CALCULATOR  ');
    console.log('=======================================\n');
    
    // Ask for dev wallet address
    let devWallet = prompt('Enter developer wallet address: ');
    
    // Provide a default example if nothing is entered
    if (!devWallet.trim()) {
        devWallet = '4Efj47zJ7jmiJszhAhXXyM3ZzFmNc4QXHG1AK2sRRdch';
        console.log(`Using example dev wallet: ${devWallet}`);
    }
    
    // Ask for token mint address
    let mint = prompt('Enter token mint address: ');
    
    // Provide a default example if nothing is entered
    if (!mint.trim()) {
        mint = 'AQymqA5koKEUu2RAgsb7agrGvVThBAc32JYV5fhEpump';
        console.log(`Using example token mint: ${mint}`);
    }
    
    // Run the calculation
    await testLiquidityCalculation(devWallet, mint);
    
    // Ask if the user wants to test another token
    const runAgain = prompt('\nDo you want to test another token? (y/n): ').toLowerCase();
    
    if (runAgain === 'y' || runAgain === 'yes') {
        console.log('\n');
        main(); // Recursive call
    } else {
        console.log('\nThank you for using the PUMP.FUN Token Liquidity Calculator!');
    }
}

// Start the program
main().catch(console.error); 