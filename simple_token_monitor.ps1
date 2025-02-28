# Simple Token Monitor with Minimalist Output
# Monitors fast_detector_debug.log and displays simplified token creation information

Write-Host "🔍 Simple Token Monitor Started - Watching for new tokens with 'recent' commitment level" -ForegroundColor Cyan
Write-Host "Press Ctrl+C to stop monitoring" -ForegroundColor Yellow
Write-Host ""

# Initialize variables
$logFile = "fast_detector_debug.log"
$lastPosition = 0

if (-not (Test-Path $logFile)) {
    Write-Host "❌ Error: Log file $logFile not found" -ForegroundColor Red
    exit
}

$fileInfo = Get-Item $logFile
$lastPosition = $fileInfo.Length

# Start monitoring loop
try {
    while ($true) {
        Start-Sleep -Milliseconds 100
        
        if (Test-Path $logFile) {
            $fileInfo = Get-Item $logFile
            if ($fileInfo.Length -gt $lastPosition) {
                # Read new content
                $reader = New-Object System.IO.StreamReader($logFile)
                $reader.BaseStream.Seek($lastPosition, [System.IO.SeekOrigin]::Begin) | Out-Null
                $newContent = $reader.ReadToEnd()
                $reader.Close()
                
                # Update position for next read
                $lastPosition = $fileInfo.Length
                
                # Process new content
                $lines = $newContent -split "`r?`n"
                
                # Variables to track token info
                $tokenName = $null
                $tokenSymbol = $null
                $tokenMint = $null
                $liquidity = "No initial liquidity"
                
                foreach ($line in $lines) {
                    # Extract token name and symbol from ultra-fast detection line
                    if ($line -match 'ULTRA-FAST DETECTION: Token (.*?) detected in') {
                        $tokenName = $matches[1].Trim()
                    }
                    
                    # Extract token name and symbol from regular detection line
                    if ($line -match 'Name: (.*?)$') {
                        $tokenName = $matches[1].Trim()
                    }
                    
                    if ($line -match 'Symbol: (.*?)$') {
                        $tokenSymbol = $matches[1].Trim()
                    }
                    
                    # Extract mint address
                    if ($line -match 'Mint: ([A-Za-z0-9]{32,44}pump)') {
                        $tokenMint = $matches[1].Trim()
                    }
                    
                    # Extract liquidity info
                    if ($line -match 'liquidity ([\d\.]+) SOL') {
                        $liquidity = "$($matches[1]) SOL"
                    }
                    
                    # When we have all the required info, display it and reset
                    if ($tokenName -and $tokenMint) {
                        $hasLiquidity = $liquidity -ne "No initial liquidity"
                        $liquiditySymbol = if ($hasLiquidity) { "✓" } else { "❌" }
                        
                        # Format token name with symbol if available
                        $displayName = if ($tokenSymbol -and $tokenSymbol -ne $tokenName) { 
                            "$tokenName ($tokenSymbol)" 
                        } else { 
                            $tokenName 
                        }
                        
                        Write-Host "🪙 NEW TOKEN CREATED! $displayName, $tokenMint (contract address), liquidity: $liquidity $liquiditySymbol" -ForegroundColor Green
                        
                        # Reset variables for next token
                        $tokenName = $null
                        $tokenSymbol = $null
                        $tokenMint = $null
                        $liquidity = "No initial liquidity"
                    }
                }
            }
        }
    }
}
catch {
    Write-Host "Error: $_" -ForegroundColor Red
}
finally {
    Write-Host "`nMonitoring stopped" -ForegroundColor Yellow
} 