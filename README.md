# Studio LSP

Studio LSP is the Fiber liquidity service for [Fiber Studio](https://www.getfiberstudio.com/). A Fiber Studio wallet registers with Studio LSP. When someone pays that wallet and it has no inbound channel, Studio LSP opens a private CKB channel and forwards the payment. Fiber Studio stays the wallet. Official `fnn` stays the node. Studio LSP is the service you run.

The channel protocol is Fiber's just-in-time open. Studio LSP turns it on and sets the fee and funding limits.

## Policy

Amounts are shannons. 1 CKB is 100,000,000 shannons.

| File | Network |
| --- | --- |
| `policy/testnet.toml` | testnet, 10 CKB open fee, 100 CKB minimum payment, 10,000 CKB maximum funding |
| `policy/mainnet.toml` | mainnet, same fee and minimum, 1,000 CKB maximum funding |

Edit a policy before you fund the node. `max_funding_shannons` is CKB Studio LSP locks on chain. `open_fee_shannons` is what the sender pays when a new channel has to be opened. An explicit fee of 0 charges nothing. Leaving the fee unset makes `fnn` refuse the open.

```bash
cargo run -- check --policy policy/testnet.toml
```

## Run a node

Start from the official `fnn` config for that network. Studio LSP only fills the service settings. It removes wallet settings (`jit_lsp_pubkey`, `jit_max_amount`, `jit_max_open_fee`) if they are present, so this node does not also behave like a Fiber Studio wallet.

```bash
cargo run -- apply --policy policy/testnet.toml --fnn-config /path/to/config.yml
```

That writes:

- `announced_node_name: "Studio LSP"`
- `jit_service_enabled: true`
- `jit_open_fee`, `jit_min_amount`, `jit_max_funding`, `jit_max_inflight_opens`

Then fund the node's CKB wallet and start official `fnn` with that config. The node needs a public listen address in `announced_addrs`. Payments can only open a channel while Studio LSP is online and holds enough CKB under `max_funding_shannons`.

Read the pubkey and listen multiaddr from `fnn` after the first start.

## Point Fiber Studio at Studio LSP

```bash
cargo run -- snippet \
  --policy policy/testnet.toml \
  --pubkey 02... \
  --multiaddr /dns4/your-host/tcp/8228/p2p/Qm...
```

Put the printed object on `testnet` or `mainnet` in Fiber Studio's `app/shared/liquidity.json`. The payment cap is the funding maximum minus the 99 CKB cell reserve. The open-fee cap equals the Studio LSP fee, which is the highest fee Fiber Studio will accept from this node.

Rebuild Fiber Studio after that change. The next time a Fiber Studio node starts, it registers with Studio LSP.

## Develop

```bash
cargo test
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```
