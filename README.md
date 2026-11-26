# Studio LSP

Studio LSP is a public Fiber liquidity node. [Fiber Studio](https://www.getfiberstudio.com/) connects to it on startup and writes the receiver settings for you. Any other Fiber node can use the same hosted node: `fnn`, [fiber-wasm](https://github.com/nervosnetwork/fiber/tree/main/crates/fiber-wasm), and [@nervosnetwork/fiber-js](https://www.npmjs.com/package/@nervosnetwork/fiber-js). The node is [fnn](https://github.com/nervosnetwork/fiber). This repository only writes that node's liquidity settings. It does not start a node.

A wallet registers with Studio LSP. When someone pays that wallet and it has no inbound channel, Studio LSP opens a private CKB channel and forwards the payment. A send spends CKB the wallet already holds. Studio LSP accepts that channel and locks `accept_funding_shannons` on its own side, which is what the wallet can later receive on the same channel. A receive with no channel uses a just-in-time open: Studio LSP funds it, up to `max_funding_shannons`, and the sender pays `open_fee_shannons`.

## Connect your own Fiber node

Fiber Studio already does this. The steps below are for a node you run yourself.

The hosted node is on **testnet**. Mainnet is not served here.

| | Value |
| --- | --- |
| Pubkey | `0323292c86026e71a3a70d2b1ca5a43dee8f1d6bc3deb990c78799eafa2f71fe1e` |
| Multiaddr | `/ip4/89.167.51.39/tcp/8228/p2p/QmbR399QwPLJTfW8QE3LukD1f5aHZENqw6CkaT2AYhZZZS` |
| Open fee the sender pays when a receive opens a channel | 10 CKB (`1000000000` shannons) |
| Smallest payment that may open a channel | 100 CKB |
| Largest funded balance above the 100 CKB reserve | 9,900 CKB (`990000000000` shannons) |
| CKB Studio LSP locks when you open a channel to send | 1,000 CKB, once your funding is at least 100 CKB |

These match the testnet entry in Fiber Studio's `app/shared/liquidity.json`.

On a testnet `config.yml`, set the receiver keys and leave the liquidity-node keys (`jit_service_enabled`, `jit_open_fee`, `jit_max_funding`, and the rest) unset. Those belong on Studio LSP, not on your node. Restart after editing.

```yaml
fiber:
  chain: testnet
  jit_lsp_pubkey: "0323292c86026e71a3a70d2b1ca5a43dee8f1d6bc3deb990c78799eafa2f71fe1e"
  jit_max_amount: 990000000000
  jit_max_open_fee: 1000000000
```

`jit_max_amount` is the funded balance above the channel reserve. `jit_max_open_fee` is the highest open fee your node will quote. It matches the 10 CKB fee this node charges. With the fee unset, your node accepts only a free open, and a paid receive will not register.

Then dial the node and save it so your node redials after a restart:

```bash
fnn-cli peer connect_peer \
  --address /ip4/89.167.51.39/tcp/8228/p2p/QmbR399QwPLJTfW8QE3LukD1f5aHZENqw6CkaT2AYhZZZS \
  --save true
```

The same call over RPC:

```bash
curl -s http://127.0.0.1:8227 \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"connect_peer","params":[{"address":"/ip4/89.167.51.39/tcp/8228/p2p/QmbR399QwPLJTfW8QE3LukD1f5aHZENqw6CkaT2AYhZZZS","save":true}]}'
```

Once that peer is connected, your node registers for incoming payments on its own. A receive with no channel opens a private channel here. To send, open a private CKB channel to that pubkey (`public: false`) after the peer is connected. Studio LSP accepts it and locks 1,000 CKB on its side.

### fiber-wasm and fiber-js

Put the same three keys in the YAML string you pass to `fiber()` or `Fiber.start`. `getDefaultConfig("testnet", ...)` is the right starting config. After the node is up, `connectPeer` is the same dial:

```js
await fiber.connectPeer({
  address: "/ip4/89.167.51.39/tcp/8228/p2p/QmbR399QwPLJTfW8QE3LukD1f5aHZENqw6CkaT2AYhZZZS",
  save: true,
});
```

That address is TCP. A native `fnn` process dials it directly. A browser build of fiber-wasm dials WebSocket, and this node publishes a TCP address only, so a page cannot open the session until a `/dns4/<host>/tcp/443/wss/p2p/QmbR399QwPLJTfW8QE3LukD1f5aHZENqw6CkaT2AYhZZZS` address is announced for the same pubkey. The receiver keys stay the same either way.

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
