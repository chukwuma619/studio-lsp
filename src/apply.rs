use crate::policy::{Policy, SHANNONS_PER_CKB};

const SERVICE_KEYS: [&str; 9] = [
    "announced_node_name",
    "jit_service_enabled",
    "jit_open_fee",
    "jit_min_amount",
    "jit_max_funding",
    "jit_inbound_cushion",
    "jit_max_inflight_opens",
    "auto_accept_channel_ckb_funding_amount",
    "open_channel_auto_accept_min_ckb_funding_amount",
];

/// Smallest wallet funding this service will accept. Matches Fiber's default of 100 CKB.
const MIN_WALLET_FUNDING_SHANNONS: u64 = 100 * SHANNONS_PER_CKB;

/// Wallet-side keys. A Studio LSP node serves channels; it does not register as a receiver.
const RECEIVER_KEYS: [&str; 3] = ["jit_lsp_pubkey", "jit_max_amount", "jit_max_open_fee"];

#[derive(Debug, PartialEq, Eq)]
pub enum ApplyError {
    MissingFiberSection,
    ChainMismatch { found: String, expected: String },
}

pub fn render(policy: &Policy, config_yaml: &str) -> Result<String, ApplyError> {
    let trailing_newline = config_yaml.ends_with('\n');
    let source: Vec<String> = config_yaml.lines().map(str::to_string).collect();
    let (section_start, section_end) =
        fiber_section_bounds(&source).ok_or(ApplyError::MissingFiberSection)?;
    if let Some(found) = active_value(&source, section_start, section_end, "chain") {
        if found != policy.chain {
            return Err(ApplyError::ChainMismatch {
                found,
                expected: policy.chain.clone(),
            });
        }
    }

    let owned: Vec<String> = service_lines(policy);
    let mut stripped = Vec::with_capacity(source.len());
    for (index, line) in source.iter().enumerate() {
        let in_fiber = index > section_start && index < section_end;
        if in_fiber && (is_owned_key(line) || is_receiver_key(line)) {
            continue;
        }
        stripped.push(line.clone());
    }

    let mut out = Vec::with_capacity(stripped.len() + owned.len());
    let mut inserted = false;
    let mut in_fiber = false;
    for line in stripped {
        if is_top_level_key(&line) {
            in_fiber = line.trim() == "fiber:";
        }
        let anchor = in_fiber && !inserted && is_active_key(&line, "listening_addr");
        out.push(line);
        if anchor {
            out.extend(owned.iter().cloned());
            inserted = true;
        }
    }
    if !inserted {
        let mut with_fiber = Vec::with_capacity(out.len() + owned.len());
        for line in out {
            let is_fiber = line.trim() == "fiber:";
            with_fiber.push(line);
            if is_fiber {
                with_fiber.extend(owned.iter().cloned());
            }
        }
        out = with_fiber;
    }

    let mut rendered = out.join("\n");
    if trailing_newline {
        rendered.push('\n');
    }
    Ok(rendered)
}

fn service_lines(policy: &Policy) -> Vec<String> {
    let name = policy.announced_node_name.trim();
    vec![
        format!("  announced_node_name: \"{name}\""),
        "  jit_service_enabled: true".to_string(),
        format!("  jit_open_fee: {}", policy.open_fee_shannons),
        format!("  jit_min_amount: {}", policy.min_amount_shannons),
        format!("  jit_max_funding: {}", policy.max_funding_shannons),
        format!(
            "  jit_inbound_cushion: {}",
            policy.inbound_cushion_shannons()
        ),
        format!("  jit_max_inflight_opens: {}", policy.max_inflight_opens),
        format!(
            "  auto_accept_channel_ckb_funding_amount: {}",
            policy.accept_funding_shannons
        ),
        format!("  open_channel_auto_accept_min_ckb_funding_amount: {MIN_WALLET_FUNDING_SHANNONS}"),
    ]
}

fn fiber_section_bounds(lines: &[String]) -> Option<(usize, usize)> {
    let start = lines.iter().position(|line| line.trim() == "fiber:")?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, line)| is_top_level_key(line))
        .map(|(index, _)| index)
        .unwrap_or(lines.len());
    Some((start, end))
}

fn is_top_level_key(line: &str) -> bool {
    if line.starts_with(' ') || line.starts_with('\t') {
        return false;
    }
    let trimmed = line.trim();
    !trimmed.is_empty() && !trimmed.starts_with('#') && trimmed.contains(':')
}

fn is_active_key(line: &str, key: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return false;
    }
    let body = trimmed.trim();
    body == format!("{key}:") || body.starts_with(&format!("{key}: "))
}

fn is_owned_key(line: &str) -> bool {
    SERVICE_KEYS.iter().any(|key| is_active_key(line, key))
}

fn is_receiver_key(line: &str) -> bool {
    RECEIVER_KEYS.iter().any(|key| is_active_key(line, key))
}

fn active_value(lines: &[String], start: usize, end: usize, key: &str) -> Option<String> {
    lines
        .iter()
        .enumerate()
        .find(|(index, line)| *index > start && *index < end && is_active_key(line, key))
        .and_then(|(_, line)| {
            let body = line.trim().trim_start_matches(&format!("{key}:")).trim();
            let unquoted = body.trim_matches('"').trim();
            if unquoted.is_empty() {
                None
            } else {
                Some(unquoted.to_string())
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Policy;

    fn policy() -> Policy {
        Policy::from_toml(include_str!("../policy/testnet.toml")).unwrap()
    }

    #[test]
    fn writes_the_service_and_strips_wallet_keys() {
        let input = "\
fiber:
  listening_addr: \"/ip4/0.0.0.0/tcp/8228\"
  # jit_service_enabled: false
  jit_lsp_pubkey: \"02aa\"
  jit_max_amount: 5
  chain: testnet
rpc:
  listening_addr: \"127.0.0.1:8227\"
";
        let rendered = render(&policy(), input).unwrap();
        assert!(rendered.contains("announced_node_name: \"Studio LSP\""));
        assert!(rendered.contains("jit_service_enabled: true"));
        assert!(rendered.contains("jit_open_fee: 1000000000"));
        assert!(rendered.contains("jit_max_funding: 1000000000000"));
        assert!(rendered.contains("jit_inbound_cushion: 90100000000"));
        assert!(rendered.contains("auto_accept_channel_ckb_funding_amount: 100000000000"));
        assert!(rendered.contains("open_channel_auto_accept_min_ckb_funding_amount: 10000000000"));
        assert!(rendered.contains("# jit_service_enabled: false"));
        assert!(!rendered.contains("jit_lsp_pubkey"));
        assert!(!rendered.contains("jit_max_amount"));
        assert!(rendered.contains("listening_addr: \"127.0.0.1:8227\""));
        let again = render(&policy(), &rendered).unwrap();
        assert_eq!(rendered, again);
    }

    #[test]
    fn refuses_a_mainnet_config_for_a_testnet_policy() {
        let input = "fiber:\n  listening_addr: \"/ip4/0.0.0.0/tcp/8228\"\n  chain: mainnet\n";
        assert_eq!(
            render(&policy(), input),
            Err(ApplyError::ChainMismatch {
                found: "mainnet".into(),
                expected: "testnet".into(),
            })
        );
    }
}
