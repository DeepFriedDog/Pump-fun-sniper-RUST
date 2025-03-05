const axios = require('axios');

// Endpoints from .env
const ENDPOINTS = {
  "Elastic Node": "https://solana-mainnet.core.chainstack.com/b04d312222d7be6eefd6b31d84a303ab",
  "Trader Node (Warp)": "https://nd-287-937-291.p2pify.com/a1da60ab93c65ba0a442df954fea529e"
};

// Auth from .env
const AUTH = {
  username: "frosty-archimedes",
  password: "thank-angles-choice-unsaid-spooky-woven"
};

// Simple RPC request payload
const payload = {
  jsonrpc: "2.0",
  id: 1,
  method: "getHealth",
  params: []
};

async function testLatency(endpoint, name, iterations = 10) {
  console.log(`\n Testing ${name} latency...`);
  
  const results = [];
  
  for (let i = 0; i < iterations; i++) {
    const start = Date.now();
    
    try {
      const response = await axios.post(endpoint, payload, {
        headers: { 'Content-Type': 'application/json' },
        auth: name === "Elastic Node" ? AUTH : undefined, // Only use auth for Elastic Node
        timeout: 5000
      });
      
      const latency = Date.now() - start;
      results.push(latency);
      console.log(`Request ${i+1}: ${latency}ms`);
    } catch (error) {
      console.error(`Request ${i+1} failed:`, 
                   error.response ? `Status: ${error.response.status}` : error.message);
    }
  }
  
  if (results.length > 0) {
    const avg = results.reduce((sum, val) => sum + val, 0) / results.length;
    const min = Math.min(...results);
    const max = Math.max(...results);
    
    console.log(`\n Results for ${name}:`);
    console.log(`Min: ${min}ms`);
    console.log(`Max: ${max}ms`);
    console.log(`Avg: ${avg.toFixed(2)}ms`);
    
    return { avg, min, max };
  }
  
  return null;
}

async function main() {
  console.log(' Solana RPC Endpoint Latency Test');
  
  for (const [name, endpoint] of Object.entries(ENDPOINTS)) {
    await testLatency(endpoint, name);
  }
}

main().catch(console.error);
