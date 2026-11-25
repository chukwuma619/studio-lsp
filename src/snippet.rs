use serde::Serialize;
use serde_json::Value;

use crate::policy::Policy;

#[derive(Debug, PartialEq, Eq)]
pub enum SnippetError {
    InvalidPubkey,
    InvalidMultiaddr,
    InvalidManifest(String),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StudioEntry {
    mode: &'static str,
    label: String,
    pubkey: String,
    multiaddr: String,
    max_amount_shannons: String,
    max_open_fee_shannons: String,
}

/// Fiber Studio `liquidity.json` entry for one network.
pub fn studio_entry(
    policy: &Policy,
    pubkey: &str,
    multiaddr: &str,
) -> Result<String, SnippetError> {
    let pubkey = normalize_pubkey(pubkey).ok_or(SnippetError::InvalidPubkey)?;
    let multiaddr = multiaddr.trim();
    if !multiaddr.starts_with('/') || multiaddr.chars().any(|character| character.is_whitespace()) {
        return Err(SnippetError::InvalidMultiaddr);
    }
    let entry = StudioEntry {
        mode: "lsp",
        label: "Studio LSP".to_string(),
        pubkey,
        multiaddr: multiaddr.to_string(),
        max_amount_shannons: policy.receiver_max_amount_shannons().to_string(),
        max_open_fee_shannons: policy.open_fee_shannons.to_string(),
    };
    Ok(serde_json::to_string_pretty(&entry).expect("studio entry serializes"))
}

/// Replace the policy's network entry inside Fiber Studio `liquidity.json`.
pub fn merge_liquidity_manifest(
    policy: &Policy,
    pubkey: &str,
    multiaddr: &str,
    manifest: &str,
) -> Result<String, SnippetError> {
    let entry: Value = serde_json::from_str(&studio_entry(policy, pubkey, multiaddr)?)
        .expect("studio entry is json");
    let mut document: Value = serde_json::from_str(manifest)
        .map_err(|error| SnippetError::InvalidManifest(error.to_string()))?;
    let object = document
        .as_object_mut()
        .ok_or_else(|| SnippetError::InvalidManifest("liquidity.json must be an object".into()))?;
    if !object.contains_key(&policy.chain) {
        return Err(SnippetError::InvalidManifest(format!(
            "liquidity.json has no {} entry",
            policy.chain
        )));
    }
    object.insert(policy.chain.clone(), entry);
    let mut rendered = serde_json::to_string_pretty(&document).expect("manifest serializes");
    rendered.push('\n');
    Ok(rendered)
}

fn normalize_pubkey(raw: &str) -> Option<String> {
    let hex = raw
        .trim()
        .strip_prefix("0x")
        .or_else(|| raw.trim().strip_prefix("0X"))
        .unwrap_or(raw.trim());
    if hex.len() == 66 && hex.chars().all(|character| character.is_ascii_hexdigit()) {
        Some(hex.to_ascii_lowercase())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Policy, CELL_RESERVE_SHANNONS};
    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Parsed {
        mode: String,
        label: String,
        pubkey: String,
        multiaddr: String,
        max_amount_shannons: String,
        max_open_fee_shannons: String,
    }

    #[test]
    fn snippet_matches_the_fee_and_leaves_room_for_the_reserve() {
        let policy = Policy::from_toml(include_str!("../policy/testnet.toml")).unwrap();
        let pubkey = format!("02{}", "ab".repeat(32));
        let raw = studio_entry(
            &policy,
            &format!("0X{pubkey}"),
            "/dns4/studio-lsp.example/tcp/8228/p2p/QmExample",
        )
        .unwrap();
        let parsed: Parsed = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.mode, "lsp");
        assert_eq!(parsed.label, "Studio LSP");
        assert_eq!(parsed.pubkey, pubkey);
        assert_eq!(
            parsed.multiaddr,
            "/dns4/studio-lsp.example/tcp/8228/p2p/QmExample"
        );
        assert_eq!(
            parsed.max_open_fee_shannons,
            policy.open_fee_shannons.to_string()
        );
        let max_amount: u64 = parsed.max_amount_shannons.parse().unwrap();
        assert_eq!(
            max_amount + CELL_RESERVE_SHANNONS,
            policy.max_funding_shannons
        );
    }

    #[test]
    fn publish_replaces_one_network_and_keeps_the_other() {
        let policy = Policy::from_toml(include_str!("../policy/testnet.toml")).unwrap();
        let pubkey = format!("02{}", "ab".repeat(32));
        let manifest = r#"{
  "testnet": { "mode": "hub", "relayId": "node1" },
  "mainnet": { "mode": "hub", "relayId": "node1" }
}
"#;
        let raw = merge_liquidity_manifest(
            &policy,
            &pubkey,
            "/dns4/studio-lsp.example/tcp/8228/p2p/QmExample",
            manifest,
        )
        .unwrap();
        let document: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(document["testnet"]["mode"], "lsp");
        assert_eq!(document["testnet"]["pubkey"], pubkey);
        assert_eq!(document["mainnet"]["mode"], "hub");
        assert_eq!(document["mainnet"]["relayId"], "node1");
        assert!(raw.ends_with('\n'));
    }

    #[test]
    fn rejects_a_short_pubkey() {
        let policy = Policy::from_toml(include_str!("../policy/testnet.toml")).unwrap();
        assert_eq!(
            studio_entry(&policy, "02aa", "/ip4/127.0.0.1/tcp/8228"),
            Err(SnippetError::InvalidPubkey)
        );
    }
}
