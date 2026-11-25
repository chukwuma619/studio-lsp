use secp256k1::PublicKey;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tentacle_multiaddr::{Multiaddr, Protocol};

use crate::policy::Policy;

#[derive(Debug, PartialEq, Eq)]
pub enum SnippetError {
    InvalidPubkey,
    InvalidMultiaddr(String),
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
    let (pubkey, pubkey_bytes) = normalize_pubkey(pubkey).ok_or(SnippetError::InvalidPubkey)?;
    let multiaddr = multiaddr.trim();
    check_multiaddr(multiaddr, &pubkey_bytes).map_err(SnippetError::InvalidMultiaddr)?;
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

fn normalize_pubkey(raw: &str) -> Option<(String, [u8; 33])> {
    let hex = raw
        .trim()
        .strip_prefix("0x")
        .or_else(|| raw.trim().strip_prefix("0X"))
        .unwrap_or(raw.trim());
    if hex.len() != 66 || !hex.chars().all(|character| character.is_ascii_hexdigit()) {
        return None;
    }
    let decoded = hex::decode(hex).ok()?;
    let bytes: [u8; 33] = decoded.try_into().ok()?;
    if bytes[0] != 0x02 && bytes[0] != 0x03 {
        return None;
    }
    PublicKey::from_slice(&bytes).ok()?;
    Some((hex.to_ascii_lowercase(), bytes))
}

fn check_multiaddr(multiaddr: &str, pubkey: &[u8; 33]) -> Result<(), String> {
    if !multiaddr.starts_with('/') || multiaddr.chars().any(|character| character.is_whitespace()) {
        return Err("multiaddr must start with / and contain no spaces".to_string());
    }
    let address: Multiaddr = multiaddr
        .parse()
        .map_err(|_| "multiaddr is not a Fiber multiaddr".to_string())?;
    let mut host = false;
    let mut tcp = false;
    let mut peer_id = None;
    for protocol in address.iter() {
        match protocol {
            Protocol::Ip4(_) | Protocol::Ip6(_) | Protocol::Dns4(_) | Protocol::Dns6(_) => {
                host = true;
            }
            Protocol::Tcp(_) => tcp = true,
            Protocol::P2P(id) => {
                if peer_id.is_some() {
                    return Err("multiaddr must contain one /p2p/ peer id".to_string());
                }
                peer_id = Some(id.into_owned());
            }
            _ => {}
        }
    }
    if !host || !tcp {
        return Err("multiaddr must include a host and a tcp port".to_string());
    }
    let Some(found) = peer_id else {
        return Err("multiaddr must include a /p2p/ peer id".to_string());
    };
    let expected = peer_id_bytes(pubkey);
    if found.as_slice() != expected.as_slice() {
        return Err(format!(
            "multiaddr /p2p/ peer id does not match the pubkey (expected {})",
            bs58::encode(&expected).into_string()
        ));
    }
    Ok(())
}

/// Tentacle peer id: SHA-256 multihash of the compressed secp256k1 key.
/// Code 0x12 fits in one unsigned-varint byte, followed by the 32-byte length.
fn peer_id_bytes(pubkey: &[u8]) -> Vec<u8> {
    let digest = Sha256::digest(pubkey);
    let mut bytes = Vec::with_capacity(34);
    bytes.push(0x12);
    bytes.push(0x20);
    bytes.extend_from_slice(&digest);
    bytes
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

    fn identity() -> (String, String) {
        let secret = secp256k1::SecretKey::from_slice(&[2u8; 32]).expect("secret");
        let public = secp256k1::PublicKey::from_secret_key(secp256k1::SECP256K1, &secret);
        let bytes = public.serialize();
        let pubkey = hex::encode(bytes);
        let peer = bs58::encode(peer_id_bytes(&bytes)).into_string();
        let multiaddr = format!("/dns4/studio-lsp.example/tcp/8228/p2p/{peer}");
        (pubkey, multiaddr)
    }

    #[test]
    fn peer_id_matches_tentacle_secio() {
        let secret = secp256k1::SecretKey::from_slice(&[2u8; 32]).expect("secret");
        let public = secp256k1::PublicKey::from_secret_key(secp256k1::SECP256K1, &secret);
        let bytes = public.serialize();
        let expected = tentacle_secio::PeerId::from_public_key(
            &tentacle_secio::PublicKey::from_raw_key(bytes.to_vec()),
        )
        .into_bytes();
        assert_eq!(peer_id_bytes(&bytes), expected);
    }

    #[test]
    fn snippet_matches_the_fee_and_leaves_room_for_the_reserve() {
        let policy = Policy::from_toml(include_str!("../policy/testnet.toml")).unwrap();
        let (pubkey, multiaddr) = identity();
        let raw = studio_entry(&policy, &format!("0X{pubkey}"), &multiaddr).unwrap();
        let parsed: Parsed = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.mode, "lsp");
        assert_eq!(parsed.label, "Studio LSP");
        assert_eq!(parsed.pubkey, pubkey);
        assert_eq!(parsed.multiaddr, multiaddr);
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
        let (pubkey, multiaddr) = identity();
        let manifest = r#"{
  "testnet": { "mode": "hub", "relayId": "node1" },
  "mainnet": { "mode": "hub", "relayId": "node1" }
}
"#;
        let raw = merge_liquidity_manifest(&policy, &pubkey, &multiaddr, manifest).unwrap();
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

    #[test]
    fn rejects_a_hex_string_that_is_not_a_public_key() {
        let policy = Policy::from_toml(include_str!("../policy/testnet.toml")).unwrap();
        let not_a_key = format!("02{}", "00".repeat(32));
        assert_eq!(
            studio_entry(&policy, &not_a_key, "/ip4/127.0.0.1/tcp/8228/p2p/QmExample"),
            Err(SnippetError::InvalidPubkey)
        );
    }

    #[test]
    fn rejects_a_multiaddr_whose_peer_id_does_not_match() {
        let policy = Policy::from_toml(include_str!("../policy/testnet.toml")).unwrap();
        let (pubkey, _) = identity();
        let error = studio_entry(
            &policy,
            &pubkey,
            "/ip4/127.0.0.1/tcp/8228/p2p/Qmes1EBD4yNo9Ywkfe6eRw9tG1nVNGLDmMud1xJMsoYFKy",
        )
        .unwrap_err();
        match error {
            SnippetError::InvalidMultiaddr(message) => {
                assert!(message.contains("does not match"), "{message}")
            }
            other => panic!("expected a multiaddr mismatch, got {other:?}"),
        }
    }
}
