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
    MissingChain,
    ChainMismatch { found: String, expected: String },
    UnsupportedConfig(String),
    InvalidPolicy(String),
}

pub fn render(policy: &Policy, config_yaml: &str) -> Result<String, ApplyError> {
    policy
        .validate()
        .map_err(|error| ApplyError::InvalidPolicy(error.to_string()))?;
    let trailing_newline = config_yaml.ends_with('\n');
    let source: Vec<String> = config_yaml.lines().map(str::to_string).collect();
    let (section_start, section_end) = fiber_section_bounds(&source)?;
    ensure_block_section(&source, section_start, section_end)?;
    let found = required_chain(&source, section_start, section_end)?;
    if found != policy.chain {
        return Err(ApplyError::ChainMismatch {
            found,
            expected: policy.chain.clone(),
        });
    }

    let indent = section_indent(&source, section_start, section_end);
    let owned: Vec<String> = service_lines(policy, &indent);
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
            in_fiber = is_block_fiber_header(&line);
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
            let is_fiber = is_block_fiber_header(&line);
            with_fiber.push(line);
            if is_fiber {
                with_fiber.extend(owned.iter().cloned());
                inserted = true;
            }
        }
        out = with_fiber;
    }
    if !inserted {
        return Err(ApplyError::UnsupportedConfig(
            "could not write service settings into the fiber: section".into(),
        ));
    }

    ensure_written(&out, policy)?;
    let mut rendered = out.join("\n");
    if trailing_newline {
        rendered.push('\n');
    }
    Ok(rendered)
}

fn service_lines(policy: &Policy, indent: &str) -> Vec<String> {
    let name = yaml_double_quoted(policy.announced_node_name.trim());
    vec![
        format!("{indent}announced_node_name: {name}"),
        format!("{indent}jit_service_enabled: true"),
        format!("{indent}jit_open_fee: {}", policy.open_fee_shannons),
        format!("{indent}jit_min_amount: {}", policy.min_amount_shannons),
        format!("{indent}jit_max_funding: {}", policy.max_funding_shannons),
        format!(
            "{indent}jit_inbound_cushion: {}",
            policy.inbound_cushion_shannons()
        ),
        format!(
            "{indent}jit_max_inflight_opens: {}",
            policy.max_inflight_opens
        ),
        format!(
            "{indent}auto_accept_channel_ckb_funding_amount: {}",
            policy.accept_funding_shannons
        ),
        format!(
            "{indent}open_channel_auto_accept_min_ckb_funding_amount: {MIN_WALLET_FUNDING_SHANNONS}"
        ),
    ]
}

/// Escape `\` and `"` so a name cannot close or extend the YAML scalar.
fn yaml_double_quoted(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            _ => escaped.push(character),
        }
    }
    format!("\"{escaped}\"")
}

fn fiber_section_bounds(lines: &[String]) -> Result<(usize, usize), ApplyError> {
    let mut start = None;
    for (index, line) in lines.iter().enumerate() {
        if is_block_fiber_header(line) {
            if start.is_some() {
                return Err(ApplyError::UnsupportedConfig(
                    "config has more than one fiber: section".into(),
                ));
            }
            start = Some(index);
        } else if is_inline_fiber(line) {
            return Err(ApplyError::UnsupportedConfig(
                "fiber: must be a block, not an inline map".into(),
            ));
        }
    }
    let start = start.ok_or(ApplyError::MissingFiberSection)?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, line)| is_top_level_key(line))
        .map(|(index, _)| index)
        .unwrap_or(lines.len());
    Ok((start, end))
}

fn ensure_block_section(lines: &[String], start: usize, end: usize) -> Result<(), ApplyError> {
    for line in &lines[start + 1..end] {
        if let Some(key) = buried_managed_key(line) {
            return Err(ApplyError::UnsupportedConfig(format!(
                "cannot safely edit {key}; keep it as its own line in the fiber: section"
            )));
        }
        if buried_chain(line) {
            return Err(ApplyError::UnsupportedConfig(
                "cannot safely read chain; keep chain as its own line in the fiber: section".into(),
            ));
        }
    }
    Ok(())
}

fn required_chain(lines: &[String], start: usize, end: usize) -> Result<String, ApplyError> {
    let mut values = Vec::new();
    for line in &lines[start + 1..end] {
        if is_active_key(line, "chain") {
            let value = assignment_value(line, "chain").ok_or(ApplyError::MissingChain)?;
            values.push(value);
        }
    }
    match values.as_slice() {
        [value] => Ok(value.clone()),
        [] => Err(ApplyError::MissingChain),
        _ => Err(ApplyError::UnsupportedConfig(
            "fiber section has more than one chain".into(),
        )),
    }
}

fn ensure_written(lines: &[String], policy: &Policy) -> Result<(), ApplyError> {
    let (start, end) = fiber_section_bounds(lines)?;
    ensure_block_section(lines, start, end)?;
    let found = required_chain(lines, start, end)?;
    if found != policy.chain {
        return Err(ApplyError::ChainMismatch {
            found,
            expected: policy.chain.clone(),
        });
    }
    for key in SERVICE_KEYS {
        let count = count_active(lines, start, end, key);
        if count != 1 {
            return Err(ApplyError::UnsupportedConfig(format!(
                "expected one {key} line in the fiber: section, found {count}"
            )));
        }
    }
    for key in RECEIVER_KEYS {
        let count = count_active(lines, start, end, key);
        if count != 0 {
            return Err(ApplyError::UnsupportedConfig(format!(
                "wallet setting {key} is still active"
            )));
        }
    }
    Ok(())
}

fn section_indent(lines: &[String], start: usize, end: usize) -> String {
    lines[start + 1..end]
        .iter()
        .find_map(|line| {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let indent_len = line.len() - line.trim_start().len();
            Some(line[..indent_len].to_string())
        })
        .unwrap_or_else(|| "  ".to_string())
}

fn count_active(lines: &[String], start: usize, end: usize, key: &str) -> usize {
    lines[start + 1..end]
        .iter()
        .filter(|line| is_active_key(line, key))
        .count()
}

fn is_block_fiber_header(line: &str) -> bool {
    if line.starts_with(' ') || line.starts_with('\t') {
        return false;
    }
    let trimmed = line.trim();
    if trimmed == "fiber:" {
        return true;
    }
    trimmed
        .strip_prefix("fiber:")
        .is_some_and(|rest| rest.trim_start().starts_with('#'))
}

fn is_inline_fiber(line: &str) -> bool {
    if line.starts_with(' ') || line.starts_with('\t') {
        return false;
    }
    let trimmed = line.trim();
    trimmed.starts_with("fiber:") && !is_block_fiber_header(line)
}

fn is_top_level_key(line: &str) -> bool {
    if line.starts_with(' ') || line.starts_with('\t') {
        return false;
    }
    let trimmed = line.trim();
    !trimmed.is_empty() && !trimmed.starts_with('#') && trimmed.contains(':')
}

fn is_active_key(line: &str, key: &str) -> bool {
    let code = code_portion(line);
    let Some(rest) = code.strip_prefix(key) else {
        return false;
    };
    rest.starts_with(':')
}

fn is_owned_key(line: &str) -> bool {
    SERVICE_KEYS.iter().any(|key| is_active_key(line, key))
}

fn is_receiver_key(line: &str) -> bool {
    RECEIVER_KEYS.iter().any(|key| is_active_key(line, key))
}

fn buried_managed_key(line: &str) -> Option<&'static str> {
    let code = code_portion(line);
    if code.is_empty() {
        return None;
    }
    SERVICE_KEYS
        .into_iter()
        .chain(RECEIVER_KEYS)
        .find(|key| contains_key_token(code, key) && !is_active_key(line, key))
}

fn buried_chain(line: &str) -> bool {
    let code = code_portion(line);
    !code.is_empty() && contains_key_token(code, "chain") && !is_active_key(line, "chain")
}

/// True when `key` is used as a YAML key (`key:`) rather than only as text in a value.
fn contains_key_token(code: &str, key: &str) -> bool {
    let mut search_from = 0;
    while let Some(offset) = code[search_from..].find(key) {
        let index = search_from + offset;
        let before_ok = index == 0 || {
            let before = code.as_bytes()[index - 1];
            !before.is_ascii_alphanumeric() && before != b'_'
        };
        let after = index + key.len();
        if before_ok && code[after..].starts_with(':') {
            return true;
        }
        search_from = after;
    }
    false
}

fn assignment_value(line: &str, key: &str) -> Option<String> {
    if !is_active_key(line, key) {
        return None;
    }
    let rest = code_portion(line)
        .strip_prefix(key)?
        .strip_prefix(':')?
        .trim();
    let unquoted = unquote(rest);
    if unquoted.is_empty() {
        None
    } else {
        Some(unquoted.to_string())
    }
}

fn unquote(value: &str) -> &str {
    let value = value.trim();
    if value.len() >= 2 {
        let first = value.chars().next().unwrap();
        let last = value.chars().next_back().unwrap();
        if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
            let start = first.len_utf8();
            let end = value.len() - last.len_utf8();
            return &value[start..end];
        }
    }
    value.split_whitespace().next().unwrap_or("")
}

/// YAML code on this line, without a full-line or trailing comment.
fn code_portion(line: &str) -> &str {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return "";
    }
    let mut quote = None;
    for (index, character) in trimmed.char_indices() {
        match quote {
            Some(mark) if character == mark => quote = None,
            Some(_) => {}
            None if character == '"' || character == '\'' => quote = Some(character),
            None if character == '#' => return trimmed[..index].trim_end(),
            None => {}
        }
    }
    trimmed
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
        assert!(rendered.contains("jit_inbound_cushion: 90000000000"));
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
    fn strips_a_managed_key_written_without_a_space() {
        let input = "\
fiber:
  listening_addr: \"/ip4/0.0.0.0/tcp/8228\"
  chain: testnet
  jit_service_enabled:false
";
        let rendered = render(&policy(), input).unwrap();
        assert_eq!(rendered.matches("jit_service_enabled:").count(), 1);
        assert!(rendered.contains("jit_service_enabled: true"));
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

    #[test]
    fn refuses_a_config_without_chain() {
        let input = "fiber:\n  listening_addr: \"/ip4/0.0.0.0/tcp/8228\"\n";
        assert_eq!(render(&policy(), input), Err(ApplyError::MissingChain));
    }

    #[test]
    fn refuses_an_inline_fiber_section() {
        let input = "fiber: { chain: testnet, listening_addr: \"/ip4/127.0.0.1/tcp/8228\" }\n";
        assert_eq!(
            render(&policy(), input),
            Err(ApplyError::UnsupportedConfig(
                "fiber: must be a block, not an inline map".into()
            ))
        );
    }

    #[test]
    fn refuses_a_managed_key_hidden_in_a_flow_map() {
        let input = "\
fiber:
  listening_addr: \"/ip4/0.0.0.0/tcp/8228\"
  chain: testnet
  extra: { jit_service_enabled: false }
";
        assert_eq!(
            render(&policy(), input),
            Err(ApplyError::UnsupportedConfig(
                "cannot safely edit jit_service_enabled; keep it as its own line in the fiber: section".into()
            ))
        );
    }

    #[test]
    fn quotes_a_name_without_letting_a_backslash_escape_the_closer() {
        assert_eq!(yaml_double_quoted(r"a\b"), r#""a\\b""#);
        assert_eq!(yaml_double_quoted("a\"b"), r#""a\"b""#);
    }
}
