use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use studio_lsp::apply::{self, ApplyError};
use studio_lsp::policy::Policy;
use studio_lsp::snippet::{self, SnippetError};

#[derive(Parser)]
#[command(
    name = "studio-lsp",
    about = "Operate Studio LSP, the Fiber liquidity service for Fiber Studio."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show the fee and funding limits in a policy file.
    Check {
        #[arg(long)]
        policy: PathBuf,
    },
    /// Write Studio LSP's JIT service settings into an fnn config.yml.
    Apply {
        #[arg(long)]
        policy: PathBuf,
        #[arg(long)]
        fnn_config: PathBuf,
    },
    /// Print the Fiber Studio liquidity.json entry for this node.
    Snippet {
        #[arg(long)]
        policy: PathBuf,
        #[arg(long)]
        pubkey: String,
        #[arg(long)]
        multiaddr: String,
    },
    /// Write this node into Fiber Studio's liquidity.json for the policy network.
    Publish {
        #[arg(long)]
        policy: PathBuf,
        #[arg(long)]
        pubkey: String,
        #[arg(long)]
        multiaddr: String,
        #[arg(long)]
        liquidity_json: PathBuf,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Check { policy } => match load_policy(&policy) {
            Ok(policy) => {
                println!("{}", summary(&policy));
                ExitCode::SUCCESS
            }
            Err(message) => fail(message),
        },
        Command::Apply { policy, fnn_config } => match apply_policy(&policy, &fnn_config) {
            Ok(()) => {
                println!(
                    "Wrote Studio LSP service settings to {}",
                    fnn_config.display()
                );
                ExitCode::SUCCESS
            }
            Err(message) => fail(message),
        },
        Command::Snippet {
            policy,
            pubkey,
            multiaddr,
        } => match load_policy(&policy).and_then(|policy| {
            snippet::studio_entry(&policy, &pubkey, &multiaddr).map_err(snippet_message)
        }) {
            Ok(entry) => {
                println!("{entry}");
                ExitCode::SUCCESS
            }
            Err(message) => fail(message),
        },
        Command::Publish {
            policy,
            pubkey,
            multiaddr,
            liquidity_json,
        } => match publish(&policy, &pubkey, &multiaddr, &liquidity_json) {
            Ok(()) => {
                println!("Wrote the Studio LSP entry to {}", liquidity_json.display());
                ExitCode::SUCCESS
            }
            Err(message) => fail(message),
        },
    }
}

fn publish(
    policy_path: &PathBuf,
    pubkey: &str,
    multiaddr: &str,
    liquidity_json: &PathBuf,
) -> Result<(), String> {
    let policy = load_policy(policy_path)?;
    let current = fs::read_to_string(liquidity_json)
        .map_err(|error| format!("failed to read {}: {error}", liquidity_json.display()))?;
    let rendered = snippet::merge_liquidity_manifest(&policy, pubkey, multiaddr, &current)
        .map_err(snippet_message)?;
    let file_name = liquidity_json
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("liquidity.json");
    let temporary = liquidity_json.with_file_name(format!(".{file_name}.tmp"));
    fs::write(&temporary, rendered)
        .map_err(|error| format!("failed to write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, liquidity_json)
        .map_err(|error| format!("failed to replace {}: {error}", liquidity_json.display()))?;
    Ok(())
}

fn load_policy(path: &PathBuf) -> Result<Policy, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    Policy::from_toml(&raw)
}

fn apply_policy(policy_path: &PathBuf, config_path: &PathBuf) -> Result<(), String> {
    let policy = load_policy(policy_path)?;
    let raw = fs::read_to_string(config_path)
        .map_err(|error| format!("failed to read {}: {error}", config_path.display()))?;
    let rendered = apply::render(&policy, &raw).map_err(apply_message)?;
    let file_name = config_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.yml");
    let temporary = config_path.with_file_name(format!(".{file_name}.tmp"));
    fs::write(&temporary, &rendered)
        .map_err(|error| format!("failed to write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, config_path)
        .map_err(|error| format!("failed to replace {}: {error}", config_path.display()))?;
    Ok(())
}

fn summary(policy: &Policy) -> String {
    format!(
        "Studio LSP on {}\nopen fee: {} shannons\nminimum payment: {} shannons\nmaximum funding: {} shannons\naccept funding: {} shannons\ninbound cushion: {} shannons\nwallet payment cap: {} shannons\nchannels opening at once: {}",
        policy.chain,
        policy.open_fee_shannons,
        policy.min_amount_shannons,
        policy.max_funding_shannons,
        policy.accept_funding_shannons,
        policy.inbound_cushion_shannons(),
        policy.receiver_max_amount_shannons(),
        policy.max_inflight_opens
    )
}

fn apply_message(error: ApplyError) -> String {
    match error {
        ApplyError::MissingFiberSection => "config is missing a fiber: section".to_string(),
        ApplyError::MissingChain => "config fiber section is missing chain".to_string(),
        ApplyError::ChainMismatch { found, expected } => {
            format!("config chain is {found}, policy chain is {expected}")
        }
        ApplyError::UnsupportedConfig(message) | ApplyError::InvalidPolicy(message) => message,
    }
}

fn snippet_message(error: SnippetError) -> String {
    match error {
        SnippetError::InvalidPubkey => {
            "pubkey must be a compressed secp256k1 public key (66 hex characters)".to_string()
        }
        SnippetError::InvalidMultiaddr(message) => message,
        SnippetError::InvalidManifest(message) => message,
    }
}

fn fail(message: String) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(1)
}
