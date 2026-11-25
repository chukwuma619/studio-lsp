use std::str::FromStr;
use std::time::Duration;

use clap_serde_derive::ClapSerde;
use fnn::fiber::channel::ChannelActorStateStore;
use fnn::fiber::network::{AcceptChannelCommand, OpenChannelCommand, PeerDisconnectReason};
use fnn::fiber::payment::SendPaymentCommand;
use fnn::fiber::{
    ChannelState, FiberConfig, Hash256, NetworkActorCommand, NetworkActorMessage,
    NetworkServiceEvent, Pubkey,
};
use fnn::gen_rand_sha256_hash;
use fnn::invoice::CkbInvoice;
use fnn::invoice::InvoiceBuilder;
use fnn::rpc::invoice::{InvoiceRpcServerImpl, NewInvoiceParams};
use fnn::tests::{
    init_tracing, wait_for_network_graph_update, wait_until_node_supports_trampoline_routing,
    NetworkNode, NetworkNodeConfigBuilder, HUGE_CKB_AMOUNT,
};
use ractor::call;
use secp256k1::SECP256K1;
use tokio::sync::Mutex;

use studio_lsp::apply;
use studio_lsp::policy::{Policy, CELL_RESERVE_SHANNONS, SHANNONS_PER_CKB};

const TESTNET_TEMPLATE: &str = include_str!("../../fiber/config/testnet/config.yml");
const MAINNET_TEMPLATE: &str = include_str!("../../fiber/config/mainnet/config.yml");

/// The Fiber test harness shares one root actor, so the node tests take turns.
static NODE_TESTS: Mutex<()> = Mutex::const_new(());

#[test]
fn shipped_policies_become_fnn_configs() {
    for (policy_toml, template) in [
        (include_str!("../policy/testnet.toml"), TESTNET_TEMPLATE),
        (include_str!("../policy/mainnet.toml"), MAINNET_TEMPLATE),
    ] {
        let (policy, config) = applied_config(policy_toml, template);
        assert_eq!(config.chain, policy.chain);
        assert!(config.jit_service_enabled());
        assert!(config.jit_channel_advertised());
        assert!(config.gen_node_features().supports_jit_channel());
        assert_eq!(
            config.jit_open_fee(),
            Some(u128::from(policy.open_fee_shannons))
        );
        assert_eq!(
            config.jit_min_amount(),
            u128::from(policy.min_amount_shannons)
        );
        assert_eq!(
            config.jit_max_funding(),
            u128::from(policy.max_funding_shannons)
        );
        assert_eq!(
            config.jit_inbound_cushion(),
            u128::from(policy.inbound_cushion_shannons())
        );
        assert_eq!(config.jit_max_inflight_opens(), policy.max_inflight_opens);
        assert_eq!(
            config.auto_accept_channel_ckb_funding_amount(),
            policy.accept_funding_shannons
        );
        assert_eq!(
            config.open_channel_auto_accept_min_ckb_funding_amount(),
            100 * SHANNONS_PER_CKB
        );
        assert!(config.jit_lsp_pubkey().is_none());
        assert!(config.jit_max_amount().is_none());
        assert!(config.jit_max_open_fee().is_none());
        assert!(!config.jit_channel_config().open_fee_required());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shipped_policy_holds_for_a_normal_fiber_wallet() {
    let _turn = NODE_TESTS.lock().await;
    init_tracing();
    let (policy, config) = applied_config(include_str!("../policy/testnet.toml"), TESTNET_TEMPLATE);
    let (payer, mut lsp, mut wallet) = connect_payer_and_wallet(config, &policy).await;

    let below_min = u128::from(policy.min_amount_shannons) - 1;
    let too_small = invoice(&wallet, below_min).await;
    let rejected = payer
        .send_payment(payment(
            &too_small,
            u128::from(policy.open_fee_shannons) * 2,
        ))
        .await
        .expect("payment was submitted");
    payer.wait_until_failed(rejected.payment_hash).await;
    assert!(
        channels_with(&wallet, lsp.pubkey).is_empty(),
        "a payment under the policy minimum must not open a channel"
    );

    let quoted = invoice(&wallet, u128::from(policy.min_amount_shannons)).await;
    assert_eq!(
        quoted.jit_open_fee(),
        Some(u128::from(policy.open_fee_shannons))
    );
    assert_eq!(
        quoted.liquidity_provider().map(|key| key.serialize()),
        Some(lsp.pubkey.serialize())
    );
    let fee_refused = payer
        .send_payment(payment(&quoted, 1))
        .await
        .expect_err("open fee above the payer cap");
    assert!(
        fee_refused.contains("JIT open fee"),
        "unexpected fee error: {fee_refused}"
    );
    assert!(channels_with(&wallet, lsp.pubkey).is_empty());

    let amount = u128::from(policy.min_amount_shannons);
    let paid = payer
        .send_payment(payment(
            &quoted,
            amount + u128::from(policy.open_fee_shannons),
        ))
        .await
        .expect("send the minimum payment");
    let channel_id = wait_for_channel(&mut wallet, lsp.pubkey).await;
    wait_for_channel(&mut lsp, wallet.pubkey).await;
    payer.wait_until_success(paid.payment_hash).await;

    let state = wallet.get_channel_actor_state(channel_id);
    assert_eq!(state.state, ChannelState::ChannelReady);
    assert!(!state.is_public());
    assert!(!state.is_one_way);
    assert_eq!(wallet.get_local_balance_from_channel(channel_id), amount);
    assert_eq!(
        wallet.get_remote_balance_from_channel(channel_id),
        u128::from(policy.inbound_cushion_shannons()),
        "inbound left for the wallet, lsp reserve {}",
        state.local_reserved_ckb_amount
    );

    let again = invoice(&wallet, amount).await;
    let second = payer
        .send_payment(payment(
            &again,
            amount + u128::from(policy.open_fee_shannons),
        ))
        .await
        .expect("send a second payment through the cushion");
    payer.wait_until_success(second.payment_hash).await;
    assert_eq!(
        channels_with(&wallet, lsp.pubkey),
        vec![channel_id],
        "the cushion should carry another payment on the same channel"
    );
    assert_eq!(
        wallet.get_local_balance_from_channel(channel_id),
        amount * 2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn advertised_maximum_opens_and_one_shannon_over_does_not() {
    let _turn = NODE_TESTS.lock().await;
    init_tracing();
    let payment_cap = 50_000_u64;
    let policy_toml = format!(
        r#"
        chain = "testnet"
        open_fee_shannons = 1_000
        min_amount_shannons = 1_000
        max_funding_shannons = {}
        accept_funding_shannons = {}
        max_inflight_opens = 1
        "#,
        u128::from(CELL_RESERVE_SHANNONS) + u128::from(payment_cap),
        u128::from(CELL_RESERVE_SHANNONS) + u128::from(payment_cap),
    );
    let (policy, config) = applied_config(&policy_toml, TESTNET_TEMPLATE);
    assert_eq!(policy.receiver_max_amount_shannons(), payment_cap);
    let (payer, mut lsp, mut wallet) = connect_payer_and_wallet(config, &policy).await;

    let over = invoice(&wallet, u128::from(payment_cap) + 1).await;
    let rejected = payer
        .send_payment(payment(&over, 1_000_000))
        .await
        .expect("over-cap payment was submitted");
    payer.wait_until_failed(rejected.payment_hash).await;
    assert!(
        channels_with(&wallet, lsp.pubkey).is_empty(),
        "one shannon above the advertised cap must not open a channel"
    );

    let exact = invoice(&wallet, u128::from(payment_cap)).await;
    let paid = payer
        .send_payment(payment(&exact, 1_000_000))
        .await
        .expect("payment at the advertised cap");
    let channel_id = wait_for_channel(&mut wallet, lsp.pubkey).await;
    wait_for_channel(&mut lsp, wallet.pubkey).await;
    payer.wait_until_success(paid.payment_hash).await;
    assert_eq!(channels_with(&wallet, lsp.pubkey).len(), 1);
    assert_eq!(
        wallet.get_local_balance_from_channel(channel_id),
        u128::from(payment_cap)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wallet_funded_channel_locks_the_accept_amount() {
    let _turn = NODE_TESTS.lock().await;
    init_tracing();
    let (policy, config) = applied_config(include_str!("../policy/testnet.toml"), TESTNET_TEMPLATE);
    let mut lsp = start_lsp(config).await;
    let mut wallet = start_wallet(&lsp, &policy).await;
    wallet.connect_to(&mut lsp).await;
    wait_for_registration(&mut wallet, &mut lsp).await;

    // Fiber compares this minimum with the amount left after the cell reserve.
    let minimum = u128::from(
        lsp.fiber_config
            .open_channel_auto_accept_min_ckb_funding_amount(),
    );
    let reserved = u128::from(CELL_RESERVE_SHANNONS);
    open_channel(&mut wallet, lsp.pubkey, minimum + reserved, false, None)
        .await
        .expect("funding that leaves the auto-accept minimum is a valid open");
    let channel_id = wait_for_channel(&mut wallet, lsp.pubkey).await;
    wait_for_channel(&mut lsp, wallet.pubkey).await;

    let state = wallet.get_channel_actor_state(channel_id);
    assert_eq!(state.state, ChannelState::ChannelReady);
    assert!(!state.is_public());
    assert_eq!(wallet.get_local_balance_from_channel(channel_id), minimum);
    assert_eq!(
        wallet.get_remote_balance_from_channel(channel_id),
        u128::from(policy.inbound_cushion_shannons()),
        "ckb the wallet can receive, lsp reserve {}",
        lsp.get_channel_actor_state(channel_id)
            .local_reserved_ckb_amount
    );
    assert_eq!(channels_with(&lsp, wallet.pubkey), vec![channel_id]);

    let mut short = start_wallet(&lsp, &policy).await;
    short.connect_to(&mut lsp).await;
    open_channel(&mut short, lsp.pubkey, minimum + reserved - 1, false, None)
        .await
        .expect("one shannon under the minimum is still a fundable cell");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        channels_with(&lsp, short.pubkey).is_empty(),
        "funding under the auto-accept minimum must not become a channel"
    );
    assert!(channels_with(&short, lsp.pubkey)
        .iter()
        .all(|id| { short.get_channel_actor_state(*id).state != ChannelState::ChannelReady }));
}

#[tokio::test(flavor = "multi_thread")]
async fn open_of_only_the_cell_reserve_does_not_become_a_channel() {
    let _turn = NODE_TESTS.lock().await;
    init_tracing();
    let (policy, config) = applied_config(include_str!("../policy/testnet.toml"), TESTNET_TEMPLATE);
    let mut lsp = start_lsp(config).await;
    let mut wallet = start_wallet(&lsp, &policy).await;
    wallet.connect_to(&mut lsp).await;
    wait_for_registration(&mut wallet, &mut lsp).await;

    open_channel(
        &mut wallet,
        lsp.pubkey,
        u128::from(CELL_RESERVE_SHANNONS),
        false,
        Some(0),
    )
    .await
    .expect("a 100 CKB cell is fundable");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        channels_with(&lsp, wallet.pubkey).is_empty(),
        "an open with nothing left after the reserve must not be accepted"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wallet_sends_through_the_lsp_and_receives_on_the_same_channel() {
    let _turn = NODE_TESTS.lock().await;
    init_tracing();
    let (policy, config) = applied_config(include_str!("../policy/testnet.toml"), TESTNET_TEMPLATE);
    let (payer, mut lsp, mut wallet) = connect_payer_and_wallet(config, &policy).await;

    let minimum = u128::from(
        lsp.fiber_config
            .open_channel_auto_accept_min_ckb_funding_amount(),
    );
    let reserved = u128::from(CELL_RESERVE_SHANNONS);
    open_channel(&mut wallet, lsp.pubkey, minimum + reserved, false, Some(0))
        .await
        .expect("wallet funds a channel it can send from");
    let channel_id = wait_for_channel(&mut wallet, lsp.pubkey).await;
    wait_for_channel(&mut lsp, wallet.pubkey).await;

    let send_amount = u128::from(SHANNONS_PER_CKB);
    let payer_channel = channels_with(&payer, lsp.pubkey)[0];
    let payer_before = payer.get_local_balance_from_channel(payer_channel);
    let preimage = gen_rand_sha256_hash();
    let outgoing = InvoiceBuilder::new(fnn::invoice::Currency::Fibd)
        .amount(Some(send_amount))
        .payment_preimage(preimage)
        .payee_pub_key(payer.pubkey.into())
        .liquidity_provider(lsp.pubkey.into())
        .allow_trampoline_routing(true)
        .build_with_sign(|hash| SECP256K1.sign_ecdsa_recoverable(hash, &payer.private_key.0))
        .expect("payer invoice");
    payer.insert_invoice(outgoing.clone(), Some(preimage));
    let sent = wallet
        .send_payment(payment(&outgoing, send_amount))
        .await
        .expect("wallet pays the payer through the lsp");
    wallet.wait_until_success(sent.payment_hash).await;
    assert_eq!(channels_with(&wallet, lsp.pubkey), vec![channel_id]);
    assert_eq!(
        payer.get_local_balance_from_channel(payer_channel),
        payer_before + send_amount
    );
    let after_send = wallet.get_local_balance_from_channel(channel_id);
    assert!(after_send < minimum);

    let receive_amount = 2 * u128::from(SHANNONS_PER_CKB);
    let incoming = invoice(&wallet, receive_amount).await;
    let paid = payer
        .send_payment(payment(
            &incoming,
            receive_amount + u128::from(policy.open_fee_shannons),
        ))
        .await
        .expect("payer pays the wallet on the open channel");
    payer.wait_until_success(paid.payment_hash).await;
    assert_eq!(
        channels_with(&wallet, lsp.pubkey),
        vec![channel_id],
        "inbound on the wallet-funded channel should not open another one"
    );
    assert_eq!(
        wallet.get_local_balance_from_channel(channel_id),
        after_send + receive_amount
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn payment_larger_than_the_cushion_opens_another_channel() {
    let _turn = NODE_TESTS.lock().await;
    init_tracing();
    // Mainnet funds 1,000 CKB and accepts 1,000 CKB, so one maximum payment
    // fills the channel. The next payment has to open another one.
    let (policy, config) = applied_config(include_str!("../policy/mainnet.toml"), MAINNET_TEMPLATE);
    let (payer, lsp, mut wallet) = connect_payer_and_wallet(config, &policy).await;
    let first_amount = u128::from(policy.receiver_max_amount_shannons());
    let second_amount = u128::from(policy.min_amount_shannons);

    let first = invoice(&wallet, first_amount).await;
    let first_payment = payer
        .send_payment(payment(
            &first,
            first_amount + u128::from(policy.open_fee_shannons),
        ))
        .await
        .expect("payment that fills the channel");
    let first_channel = wait_for_channel(&mut wallet, lsp.pubkey).await;
    payer.wait_until_success(first_payment.payment_hash).await;
    assert_eq!(wallet.get_remote_balance_from_channel(first_channel), 0);

    let second = invoice(&wallet, second_amount).await;
    let second_payment = payer
        .send_payment(payment(
            &second,
            second_amount + u128::from(policy.open_fee_shannons),
        ))
        .await
        .expect("payment larger than the remaining inbound");
    payer.wait_until_success(second_payment.payment_hash).await;
    let channels = channels_with(&wallet, lsp.pubkey);
    assert_eq!(channels.len(), 2, "the lsp should open a second channel");
    let received: u128 = channels
        .iter()
        .map(|channel_id| wallet.get_local_balance_from_channel(*channel_id))
        .sum();
    assert_eq!(received, first_amount + second_amount);
    assert!(channels.contains(&first_channel));
}

#[tokio::test(flavor = "multi_thread")]
async fn unregistered_node_does_not_get_a_channel() {
    let _turn = NODE_TESTS.lock().await;
    init_tracing();
    let (policy, config) = applied_config(include_str!("../policy/testnet.toml"), TESTNET_TEMPLATE);
    let (payer, mut lsp, _wallet) = connect_payer_and_wallet(config, &policy).await;
    let mut stranger = NetworkNode::new_with_node_name("stranger").await;
    stranger.connect_to(&mut lsp).await;

    let preimage = gen_rand_sha256_hash();
    let invoice = InvoiceBuilder::new(fnn::invoice::Currency::Fibd)
        .amount(Some(u128::from(policy.min_amount_shannons)))
        .payment_preimage(preimage)
        .payee_pub_key(stranger.pubkey.into())
        .liquidity_provider(lsp.pubkey.into())
        .allow_trampoline_routing(true)
        .build_with_sign(|hash| SECP256K1.sign_ecdsa_recoverable(hash, &stranger.private_key.0))
        .expect("build invoice");
    stranger.insert_invoice(invoice.clone(), Some(preimage));

    let sent = payer
        .send_payment(payment(
            &invoice,
            u128::from(policy.min_amount_shannons) + u128::from(policy.open_fee_shannons),
        ))
        .await
        .expect("payment was submitted");
    payer.wait_until_failed(sent.payment_hash).await;
    assert!(channels_with(&stranger, lsp.pubkey).is_empty());
    assert!(channels_with(&lsp, stranger.pubkey).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn payment_above_existing_outbound_and_under_the_funding_cap_opens() {
    let _turn = NODE_TESTS.lock().await;
    init_tracing();
    let outbound = 5_000_u64;
    let payment_amount = 20_000_u64;
    let policy_toml = format!(
        r#"
        chain = "testnet"
        open_fee_shannons = 1_000
        min_amount_shannons = 1_000
        max_funding_shannons = {}
        accept_funding_shannons = {}
        max_inflight_opens = 1
        "#,
        u128::from(CELL_RESERVE_SHANNONS) + u128::from(payment_amount) + u128::from(outbound),
        u128::from(CELL_RESERVE_SHANNONS) + u128::from(outbound),
    );
    let (policy, config) = applied_config(&policy_toml, TESTNET_TEMPLATE);
    let (payer, lsp, mut wallet) = connect_payer_and_wallet(config, &policy).await;

    let amount = u128::from(payment_amount);
    let invoice = invoice(&wallet, amount).await;
    let paid = payer
        .send_payment(payment(
            &invoice,
            amount + u128::from(policy.open_fee_shannons),
        ))
        .await
        .expect("payment under the funding cap");
    let channel_id = wait_for_channel(&mut wallet, lsp.pubkey).await;
    payer.wait_until_success(paid.payment_hash).await;
    assert_eq!(wallet.get_local_balance_from_channel(channel_id), amount);
}

#[tokio::test(flavor = "multi_thread")]
async fn payer_reaches_the_lsp_through_another_node() {
    let _turn = NODE_TESTS.lock().await;
    init_tracing();
    let (policy, config) = applied_config(include_str!("../policy/testnet.toml"), TESTNET_TEMPLATE);
    let mut lsp = start_lsp(config).await;
    let mut relay = NetworkNode::new_with_node_name("relay").await;
    let mut payer = NetworkNode::new_with_node_name("payer").await;
    let mut wallet = start_wallet(&lsp, &policy).await;

    relay.connect_to(&mut lsp).await;
    open_channel(&mut relay, lsp.pubkey, HUGE_CKB_AMOUNT, true, Some(0))
        .await
        .expect("relay opens to the lsp");
    let relay_channel = wait_for_channel(&mut relay, lsp.pubkey).await;
    wait_for_channel(&mut lsp, relay.pubkey).await;
    let funding_outpoint = relay
        .get_channel_actor_state(relay_channel)
        .must_get_funding_transaction_outpoint();
    let funding_tx = relay
        .get_transaction_view_from_hash(funding_outpoint.tx_hash().into())
        .await
        .expect("funding tx");
    // Each test node has its own chain. The payer can learn the public channel
    // only after that funding transaction is on its chain.
    payer.submit_tx(funding_tx).await;

    payer.connect_to(&mut relay).await;
    open_channel(&mut payer, relay.pubkey, HUGE_CKB_AMOUNT, true, Some(0))
        .await
        .expect("payer opens to the relay");
    accept_channel(&mut relay, payer.pubkey, HUGE_CKB_AMOUNT).await;
    wait_for_channel(&mut payer, relay.pubkey).await;
    wait_for_channel(&mut relay, payer.pubkey).await;

    let mut seen = 0;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        seen = payer.get_network_graph_channels().await.len();
        if seen >= 2 {
            break;
        }
    }
    assert!(
        seen >= 2,
        "the payer needs a route across the relay to the lsp, saw {seen}"
    );

    wallet.connect_to(&mut lsp).await;
    wait_for_registration(&mut wallet, &mut lsp).await;
    let amount = u128::from(policy.min_amount_shannons);
    let incoming = invoice(&wallet, amount).await;
    let paid = payer
        .send_payment(payment(
            &incoming,
            amount + u128::from(policy.open_fee_shannons),
        ))
        .await
        .expect("pay through the relay");
    let channel_id = wait_for_channel(&mut wallet, lsp.pubkey).await;
    payer.wait_until_success(paid.payment_hash).await;
    assert_eq!(wallet.get_local_balance_from_channel(channel_id), amount);
    assert!(channels_with(&payer, lsp.pubkey).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn zero_open_fee_still_opens_a_channel() {
    let _turn = NODE_TESTS.lock().await;
    init_tracing();
    let policy_toml = format!(
        r#"
        chain = "testnet"
        open_fee_shannons = 0
        min_amount_shannons = 1_000
        max_funding_shannons = {}
        accept_funding_shannons = {}
        max_inflight_opens = 1
        "#,
        u128::from(CELL_RESERVE_SHANNONS) + 50_000,
        u128::from(CELL_RESERVE_SHANNONS) + 50_000,
    );
    let (policy, config) = applied_config(&policy_toml, TESTNET_TEMPLATE);
    let (payer, lsp, mut wallet) = connect_payer_and_wallet(config, &policy).await;
    let amount = u128::from(policy.min_amount_shannons);
    let incoming = invoice(&wallet, amount).await;
    assert_eq!(incoming.jit_open_fee(), Some(0));
    let paid = payer
        .send_payment(payment(&incoming, amount))
        .await
        .expect("a zero open fee is payable");
    let channel_id = wait_for_channel(&mut wallet, lsp.pubkey).await;
    payer.wait_until_success(paid.payment_hash).await;
    assert_eq!(wallet.get_local_balance_from_channel(channel_id), amount);
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_receiver_does_not_get_a_channel() {
    let _turn = NODE_TESTS.lock().await;
    init_tracing();
    let (policy, config) = applied_config(include_str!("../policy/testnet.toml"), TESTNET_TEMPLATE);
    let (payer, mut lsp, wallet) = connect_payer_and_wallet(config, &policy).await;
    let amount = u128::from(policy.min_amount_shannons);
    let incoming = invoice(&wallet, amount).await;

    lsp.network_actor
        .send_message(NetworkActorMessage::new_command(
            NetworkActorCommand::DisconnectPeer(
                wallet.pubkey,
                PeerDisconnectReason::Requested,
                None,
            ),
        ))
        .expect("lsp alive");
    lsp.expect_debug_event("JitChannelUnregistered").await;

    let sent = payer
        .send_payment(payment(
            &incoming,
            amount + u128::from(policy.open_fee_shannons),
        ))
        .await
        .expect("payment was submitted");
    payer.wait_until_failed(sent.payment_hash).await;
    assert!(channels_with(&wallet, lsp.pubkey).is_empty());
    assert!(channels_with(&lsp, wallet.pubkey).is_empty());
}

fn applied_config(policy_toml: &str, template: &str) -> (Policy, FiberConfig) {
    let policy = Policy::from_toml(policy_toml).expect("policy");
    let rendered = apply::render(&policy, &plant_wallet_keys(template)).expect("apply");
    for key in ["jit_lsp_pubkey", "jit_max_amount", "jit_max_open_fee"] {
        assert!(
            !active_setting(rendered.as_str(), key),
            "{key} is still an active setting"
        );
    }
    let config = parse_fiber(&rendered);
    config.validate().expect("fnn accepts the applied config");
    (policy, config)
}

fn plant_wallet_keys(template: &str) -> String {
    template.replacen(
        "\n  chain:",
        "\n  jit_lsp_pubkey: \"02aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n  jit_max_amount: 1\n  jit_max_open_fee: 1\n  chain:",
        1,
    )
}

fn parse_fiber(config_yaml: &str) -> FiberConfig {
    let section = fiber_section(config_yaml);
    let opt: <FiberConfig as ClapSerde>::Opt = serde_yaml::from_str(&section)
        .unwrap_or_else(|error| panic!("fnn rejected the applied config: {error}\n{section}"));
    FiberConfig::from(opt)
}

fn fiber_section(config_yaml: &str) -> String {
    let mut inside = false;
    let mut lines = Vec::new();
    for line in config_yaml.lines() {
        if !inside {
            if line.trim() == "fiber:" {
                inside = true;
            }
            continue;
        }
        if is_top_level_key(line) {
            break;
        }
        if let Some(stripped) = line.strip_prefix("  ") {
            lines.push(stripped.to_string());
        } else {
            lines.push(line.trim_start().to_string());
        }
    }
    assert!(inside, "config has no fiber section");
    lines.join("\n")
}

fn active_setting(config_yaml: &str, key: &str) -> bool {
    let prefix = format!("{key}:");
    config_yaml.lines().any(|line| {
        let trimmed = line.trim_start();
        !trimmed.starts_with('#') && trimmed.starts_with(&prefix)
    })
}

fn is_top_level_key(line: &str) -> bool {
    if line.starts_with(' ') || line.starts_with('\t') {
        return false;
    }
    let trimmed = line.trim();
    !trimmed.is_empty() && !trimmed.starts_with('#') && trimmed.contains(':')
}

async fn start_lsp(parsed: FiberConfig) -> NetworkNode {
    NetworkNode::new_with_config(
        NetworkNodeConfigBuilder::new()
            .node_name(Some("Studio LSP".to_string()))
            .fiber_config_updater(move |config| {
                config.jit_service_enabled = parsed.jit_service_enabled;
                config.jit_open_fee = parsed.jit_open_fee;
                config.jit_min_amount = parsed.jit_min_amount;
                config.jit_max_funding = parsed.jit_max_funding;
                config.jit_inbound_cushion = parsed.jit_inbound_cushion;
                config.jit_max_inflight_opens = parsed.jit_max_inflight_opens;
                config.auto_accept_channel_ckb_funding_amount =
                    parsed.auto_accept_channel_ckb_funding_amount;
                config.open_channel_auto_accept_min_ckb_funding_amount =
                    parsed.open_channel_auto_accept_min_ckb_funding_amount;
            })
            .build(),
    )
    .await
}

async fn start_wallet(lsp: &NetworkNode, policy: &Policy) -> NetworkNode {
    let pubkey = hex::encode(lsp.pubkey.serialize());
    let max_amount = u128::from(policy.receiver_max_amount_shannons());
    let max_fee = u128::from(policy.open_fee_shannons);
    NetworkNode::new_with_config(
        NetworkNodeConfigBuilder::new()
            .node_name(Some("fiber-wallet".to_string()))
            .fiber_config_updater(move |config| {
                config.jit_lsp_pubkey = Some(pubkey);
                config.jit_max_amount = Some(max_amount);
                config.jit_max_open_fee = Some(max_fee);
            })
            .build(),
    )
    .await
}

async fn connect_payer_and_wallet(
    config: FiberConfig,
    policy: &Policy,
) -> (NetworkNode, NetworkNode, NetworkNode) {
    let mut lsp = start_lsp(config).await;
    let mut payer = NetworkNode::new_with_node_name("payer").await;
    let mut wallet = start_wallet(&lsp, policy).await;

    payer.connect_to(&mut lsp).await;
    open_channel(&mut payer, lsp.pubkey, HUGE_CKB_AMOUNT, true, Some(0))
        .await
        .expect("payer opens a public channel");
    wait_for_channel(&mut payer, lsp.pubkey).await;
    wait_for_channel(&mut lsp, payer.pubkey).await;
    wait_for_network_graph_update(&payer, 1).await;
    wait_until_node_supports_trampoline_routing(&payer, &lsp).await;

    wallet.connect_to(&mut lsp).await;
    wait_for_registration(&mut wallet, &mut lsp).await;
    (payer, lsp, wallet)
}

async fn wait_for_registration(wallet: &mut NetworkNode, lsp: &mut NetworkNode) {
    wallet.expect_debug_event("JitChannelRegisterSent").await;
    lsp.expect_debug_event("JitChannelRegistered").await;
    wallet
        .expect_debug_event("JitChannelRegisterAccepted")
        .await;
}

async fn invoice(wallet: &NetworkNode, amount: u128) -> CkbInvoice {
    let rpc = InvoiceRpcServerImpl::new(
        wallet.store.clone(),
        Some(wallet.network_actor.clone()),
        Some(wallet.fiber_config.clone()),
    );
    let result = rpc
        .new_invoice(NewInvoiceParams {
            amount,
            description: Some("studio-lsp".to_string()),
            currency: fiber_json_types::Currency::Fibd,
            payment_preimage: None,
            payment_hash: None,
            expiry: None,
            fallback_address: None,
            final_expiry_delta: None,
            udt_type_script: None,
            hash_algorithm: None,
            allow_mpp: None,
            allow_trampoline_routing: None,
        })
        .await
        .expect("new invoice");
    CkbInvoice::from_str(&result.invoice_address).expect("invoice address")
}

fn payment(invoice: &CkbInvoice, max_fee_amount: u128) -> SendPaymentCommand {
    SendPaymentCommand {
        invoice: Some(invoice.to_string()),
        max_fee_amount: Some(max_fee_amount),
        ..Default::default()
    }
}

async fn accept_channel(node: &mut NetworkNode, peer: Pubkey, funding_amount: u128) {
    let temp_channel_id = node
        .expect_to_process_event(|event| match event {
            NetworkServiceEvent::ChannelPendingToBeAccepted(pubkey, channel_id)
                if *pubkey == peer =>
            {
                Some(*channel_id)
            }
            _ => None,
        })
        .await;
    let message = |rpc_reply| {
        NetworkActorMessage::Command(NetworkActorCommand::AcceptChannel(
            AcceptChannelCommand {
                temp_channel_id,
                funding_amount,
                shutdown_script: None,
                max_tlc_value_in_flight: None,
                max_tlc_number_in_flight: None,
                min_tlc_value: None,
                tlc_fee_proportional_millionths: Some(0),
                tlc_expiry_delta: None,
            },
            rpc_reply,
        ))
    };
    call!(node.network_actor, message)
        .expect("node alive")
        .expect("accept channel");
}

async fn open_channel(
    from: &mut NetworkNode,
    to: Pubkey,
    funding_amount: u128,
    public: bool,
    tlc_fee_proportional_millionths: Option<u128>,
) -> Result<(), String> {
    let message = |rpc_reply| {
        NetworkActorMessage::Command(NetworkActorCommand::OpenChannel(
            OpenChannelCommand {
                pubkey: to,
                funding_amount,
                public,
                one_way: false,
                shutdown_script: None,
                funding_udt_type_script: None,
                commitment_fee_rate: None,
                commitment_delay_epoch: None,
                funding_fee_rate: None,
                tlc_expiry_delta: None,
                tlc_min_value: None,
                tlc_fee_proportional_millionths,
                max_tlc_value_in_flight: None,
                max_tlc_number_in_flight: None,
            },
            rpc_reply,
        ))
    };
    call!(from.network_actor, message)
        .expect("node alive")
        .map(|_| ())
}

async fn wait_for_channel(node: &mut NetworkNode, peer: Pubkey) -> Hash256 {
    node.expect_to_process_event(|event| match event {
        NetworkServiceEvent::ChannelReady(pubkey, channel_id, _) if *pubkey == peer => {
            Some(*channel_id)
        }
        _ => None,
    })
    .await
}

fn channels_with(node: &NetworkNode, peer: Pubkey) -> Vec<Hash256> {
    node.store.get_channel_ids_by_pubkey(&peer)
}
