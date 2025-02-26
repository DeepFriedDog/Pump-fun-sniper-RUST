# PumpFun Sniper Bot Enhancements

## New Token Quality Features

The sniper bot has been enhanced with additional quality control checks to help you make better investment decisions. These features can be enabled or disabled in your `.env` file.

### Minimum Liquidity Check

This feature verifies that a token has sufficient liquidity (SOL deposited by the developer) before executing a buy.

**Configuration:**
```
CHECK_MIN_LIQUIDITY=true|false    # Enable/disable the minimum liquidity check
MIN_LIQUIDITY=0.5                 # Minimum SOL liquidity required (example: 0.5 SOL)
```

The liquidity is calculated using pump.fun's bonding curve formula:
- SOL = (32,190,005,730 / (1,073,000,191 - DevTokenBalance)) - 30

Where `DevTokenBalance` is the number of tokens held by the developer. This gives an estimate of how much SOL has been deposited to create the token's liquidity.

### Approved Developers Check

This feature allows you to only purchase tokens created by developers you trust.

**Configuration:**
```
APPROVED_DEVS_ONLY=true|false     # Enable/disable the approved developers check
```

The list of approved developers is stored in `src/approved-devs.json`. To add more developers to this list, edit the file manually.

## How It Works

When a new token is detected:

1. The bot checks if the token's metadata contains all required URLs (if `CHECK_URLS=true`)
2. The bot checks if the developer is in the approved list (if `APPROVED_DEVS_ONLY=true`)
3. The bot checks if the token meets the minimum liquidity requirement (if `CHECK_MIN_LIQUIDITY=true`)
4. The bot checks if the token name contains the specified tag (if `SNIPE_BY_TAG` is set)
5. If all enabled checks pass, the bot executes the buy

## Benefits

- **Avoid Low Liquidity Tokens**: Skip tokens with insufficient liquidity, which can be difficult to sell later
- **Trust Specific Developers**: Only buy tokens from developers with a proven track record
- **Customizable**: All features can be enabled/disabled independently based on your strategy

To use these new features, update your `.env` file with the appropriate settings. 