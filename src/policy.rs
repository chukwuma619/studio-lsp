use serde::Deserialize;
use thiserror::Error;

/// On-chain cell reserve Studio already budgets when it opens a channel.
pub const SHANNONS_PER_CKB: u64 = 100_000_000;
pub const CELL_RESERVE_SHANNONS: u64 = 99 * SHANNONS_PER_CKB;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Policy {
    pub chain: String,
    #[serde(default = "default_node_name")]
    pub announced_node_name: String,
    pub open_fee_shannons: u64,
    #[serde(default)]
    pub min_amount_shannons: u64,
    pub max_funding_shannons: u64,
    #[serde(default = "default_inflight_opens")]
    pub max_inflight_opens: usize,
}

fn default_node_name() -> String {
    "Studio LSP".to_string()
}

fn default_inflight_opens() -> usize {
    8
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("chain must be testnet or mainnet")]
    Chain,
    #[error("announced node name must be a single line without quotes")]
    NodeName,
    #[error(
        "max funding must cover the {CELL_RESERVE_SHANNONS} shannon cell reserve plus a payment"
    )]
    Funding,
    #[error("minimum payment is larger than the largest payment Studio LSP can fund")]
    Minimum,
    #[error("max in-flight opens must be at least 1")]
    Inflight,
}

impl Policy {
    pub fn from_toml(raw: &str) -> Result<Self, String> {
        let policy: Self = toml::from_str(raw).map_err(|error| error.to_string())?;
        policy.validate().map_err(|error| error.to_string())?;
        Ok(policy)
    }

    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.chain != "testnet" && self.chain != "mainnet" {
            return Err(PolicyError::Chain);
        }
        let name = self.announced_node_name.trim();
        if name.is_empty()
            || name.chars().any(|character| {
                character.is_control() || character == '"' || character == '#' || character == '\n'
            })
        {
            return Err(PolicyError::NodeName);
        }
        if self.max_inflight_opens == 0 {
            return Err(PolicyError::Inflight);
        }
        if self.max_funding_shannons <= CELL_RESERVE_SHANNONS {
            return Err(PolicyError::Funding);
        }
        if self.min_amount_shannons > self.receiver_max_amount_shannons() {
            return Err(PolicyError::Minimum);
        }
        Ok(())
    }

    /// Largest payment a Fiber Studio wallet should accept on a Studio LSP channel.
    /// Studio LSP adds the cell reserve on top of the payment before funding.
    pub fn receiver_max_amount_shannons(&self) -> u64 {
        self.max_funding_shannons
            .saturating_sub(CELL_RESERVE_SHANNONS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_policies_fund_a_payment_above_the_reserve() {
        for raw in [
            include_str!("../policy/testnet.toml"),
            include_str!("../policy/mainnet.toml"),
        ] {
            let policy = Policy::from_toml(raw).expect("shipped policy");
            assert_eq!(policy.announced_node_name, "Studio LSP");
            assert!(policy.open_fee_shannons > 0);
            assert!(policy.receiver_max_amount_shannons() >= policy.min_amount_shannons);
            assert!(policy.max_funding_shannons > policy.receiver_max_amount_shannons());
        }
    }

    #[test]
    fn rejects_a_funding_cap_that_cannot_cover_the_reserve() {
        let error = Policy::from_toml(
            r#"
            chain = "testnet"
            open_fee_shannons = 0
            max_funding_shannons = 1
            "#,
        )
        .unwrap_err();
        assert!(error.contains("cell reserve"));
    }
}
