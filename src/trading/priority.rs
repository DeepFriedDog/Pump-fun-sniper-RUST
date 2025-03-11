//! Transaction priority handling for high-speed trading

use log::debug;

/// Transaction priority levels for fee calculation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityLevel {
    /// No priority fee
    None,
    /// Low priority fee (25th percentile)
    Low,
    /// Medium priority fee (50th percentile)
    Medium,
    /// High priority fee (75th percentile)
    High,
    /// Maximum priority fee (90th percentile)
    Max,
    /// Ultra priority fee (95th percentile) - only for critical transactions
    Ultra
}

impl PriorityLevel {
    /// Convert priority level to micro-lamports multiplier
    pub fn to_multiplier(&self) -> u64 {
        match self {
            PriorityLevel::None => 0,
            PriorityLevel::Low => 1_000,
            PriorityLevel::Medium => 10_000,
            PriorityLevel::High => 100_000,
            PriorityLevel::Max => 1_000_000,
            PriorityLevel::Ultra => 10_000_000,
        }
    }
    
    /// Get priority level from string representation
    pub fn from_string(priority: &str) -> Self {
        match priority.to_lowercase().as_str() {
            "none" => PriorityLevel::None,
            "low" => PriorityLevel::Low,
            "medium" => PriorityLevel::Medium,
            "high" => PriorityLevel::High,
            "max" => PriorityLevel::Max,
            "ultra" => PriorityLevel::Ultra,
            _ => {
                debug!("Unknown priority level: {}, using Medium", priority);
                PriorityLevel::Medium
            }
        }
    }
}

/// Calculate priority fee in micro-lamports based on level
pub fn calculate_priority_fee(level: PriorityLevel) -> u64 {
    // Base fee in micro-lamports (1 lamport = 1,000,000 micro-lamports)
    let base_fee = 5_000; // 0.000005 SOL base fee
    
    // Apply level multiplier
    base_fee * level.to_multiplier()
} 