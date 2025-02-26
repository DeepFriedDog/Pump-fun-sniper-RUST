const readline = require('readline');
const fs = require('fs');
const path = require('path');

const envVariables = [
    { key: 'PRIVATE_KEY', description: "Your wallet's private key in base58 format (e.g., Phantom wallet private key)." },
    { key: 'WALLET', description: "The public key of your wallet." },
    { key: 'AMOUNT', description: "The amount (in SOL) to spend on each token." },
    { key: 'SLIPPAGE', description: "Slippage tolerance (e.g., 10 for 10%)." },
    { key: 'PROTECTION', description: "Set to true for MEV protection or false for maximum speed." },
    { key: 'BUY_TIP', description: "BloXroute TIP for buy transactions (in SOL)." },
    { key: 'SELL_TIP', description: "BloXroute TIP for sell transactions (in SOL)." },
    { key: 'TAKE_PROFIT', description: "Profit-taking percentage (e.g., 10 for 10%)." },
    { key: 'STOP_LOSS', description: "Stop-loss percentage (e.g., 10 for 10%)." },
    { key: 'TIMEOUT', description: "Timeout value (in minutes) for automatic sell if no stop loss or take profit is triggered." },
    { key: 'CHECK_URLS', description: "Set to true to validate token metadata (Telegram, Twitter, Website URLs), or false to skip validation." },
    { key: 'SNIPE_BY_TAG', description: "Set a value to snipe only tokens with that name in token metadata (empty if not needed)." },
    { key: 'MAX_POSITIONS', description: "Number of positions to maintain concurrently (minimum 1)." }
];

const rl = readline.createInterface({
    input: process.stdin,
    output: process.stdout
});

let envData = "";

function askQuestion(index) {
    if (index >= envVariables.length) {
        fs.writeFileSync(path.join(__dirname, '.env'), envData);
        console.log('Created .env file with provided configuration.');
        rl.close();
        return;
    }

    const variable = envVariables[index];
    rl.question(`Enter value for ${variable.key} (${variable.description}): `, (answer) => {
        envData += `${variable.key}=${answer.trim()}\n`;
        askQuestion(index + 1);
    });
}

askQuestion(0); 