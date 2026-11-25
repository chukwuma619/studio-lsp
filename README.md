# Studio LSP

Studio LSP is the Fiber liquidity service for [Fiber Studio](https://www.getfiberstudio.com/). A Fiber Studio wallet registers with Studio LSP. When someone pays that wallet and it has no inbound channel, Studio LSP opens a private CKB channel and forwards the payment. Fiber Studio stays the wallet. The node is [fnn](https://github.com/nervosnetwork/fiber). This repository only writes that node's liquidity settings. It does not start a node.

A send spends CKB the wallet already holds. Studio LSP accepts that channel and locks `accept_funding_shannons` on its own side, which is what the wallet can later receive on the same channel. A receive with no channel uses a just-in-time open: Studio LSP funds it, up to `max_funding_shannons`, and the sender pays `open_fee_shannons`.

## Policy

Amounts are shannons. 1 CKB is 100,000,000 shannons.

| File | Network |
| --- | --- |
| `policy/testnet.toml` | testnet, 10 CKB open fee, 100 CKB minimum payment, 10,000 CKB maximum funding, 1,000 CKB on a wallet-funded channel |
| `policy/mainnet.toml` | mainnet, same fee and minimum, 1,000 CKB maximum funding, 1,000 CKB on a wallet-funded channel |

Edit a policy before you fund the node. `max_funding_shannons` is CKB Studio LSP locks on chain. `open_fee_shannons` is what the sender pays when a new channel has to be opened. An explicit fee of 0 charges nothing. Leaving the fee unset makes `fnn` refuse the open.

```bash
cargo run -- check --policy policy/testnet.toml
```

## Run a node

Install `fnn` and create its data directory before you run `apply`. Start `fnn` once with the official config for that network so it writes `config.yml` and the node key, then stop it. `apply` only fills the service settings in that file. It removes wallet settings (`jit_lsp_pubkey`, `jit_max_amount`, `jit_max_open_fee`) if they are present, so this node does not also behave like a Fiber Studio wallet.

```bash
cargo run -- apply --policy policy/testnet.toml --fnn-config /path/to/config.yml
```

That writes:

- `announced_node_name: "Studio LSP"`
- `jit_service_enabled: true`
- `jit_open_fee`, `jit_min_amount`, `jit_max_funding`, `jit_max_inflight_opens`
- `auto_accept_channel_ckb_funding_amount` and `open_channel_auto_accept_min_ckb_funding_amount`

Then fund the node's CKB wallet and start `fnn` with that config. The node needs a public listen address in `announced_addrs`. Payments can only open a channel while Studio LSP is online and holds enough CKB under `max_funding_shannons`.

`jit_inbound_cushion` is written for you. It is the spendable part of `accept_funding_shannons`. On a just-in-time receive, Studio LSP locks the payment, the cell reserve, and this cushion, capped by `max_funding_shannons`. After the payment settles, the cushion is still inbound, so the wallet can receive again on that channel.

Read the pubkey and listen multiaddr from `fnn` after the first start. The full server setup, funding, and release steps are in [DEPLOY.md](DEPLOY.md).

## Point Fiber Studio at Studio LSP

```bash
cargo run -- snippet \
  --policy policy/testnet.toml \
  --pubkey 02... \
  --multiaddr /dns4/your-host/tcp/8228/p2p/Qm...
```

Or write that entry straight into the wallet manifest. This replaces only the network named in the policy and leaves the other network as it is.

```bash
cargo run -- publish \
  --policy policy/testnet.toml \
  --pubkey 02... \
  --multiaddr /dns4/your-host/tcp/8228/p2p/Qm... \
  --liquidity-json ../fiber-studio/app/shared/liquidity.json
```

The payment cap is the funding maximum minus the 100 CKB channel reserve. The open-fee cap equals the Studio LSP fee, which is the highest fee Fiber Studio will accept from this node.

Rebuild and ship Fiber Studio after that change. The next time a Fiber Studio node starts, it connects to Studio LSP only, registers for incoming payments, and opens a private channel with the user's CKB when they send.

## Develop

```bash
cargo test
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```

`cargo test` starts real Fiber nodes. The first build takes several minutes.
