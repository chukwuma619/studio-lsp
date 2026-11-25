# Deploy Studio LSP

Studio LSP is one public Fiber node. Fiber Studio wallets stay on users' machines. You lock CKB so those wallets can receive. Each user locks their own CKB when they want to send.

Build `fnn` from the Fiber tree in this workspace. Studio LSP writes `jit_inbound_cushion`, and that key has to be one this `fnn` understands.

Amounts below are the shipped policies. 1 CKB is 100,000,000 shannons.

| | Testnet | Mainnet |
| --- | --- | --- |
| Open fee, paid by the sender when a receive opens a channel | 10 CKB | 10 CKB |
| Smallest payment that may open a channel | 100 CKB | 100 CKB |
| Most CKB this node will lock in one channel | 10,000 CKB | 1,000 CKB |
| CKB this node adds when a wallet opens a channel to send | 1,000 CKB | 1,000 CKB |
| Inbound left after a just-in-time payment | 901 CKB, until the payment plus the cell reserve already fills the 10,000 CKB cap | Whatever still fits under the 1,000 CKB cap after the payment and the 99 CKB reserve |

Edit `policy/testnet.toml` or `policy/mainnet.toml` before you fund the node. Mainnet spends real CKB.

## 1. Host

Use a machine with a public DNS name or IP. Open TCP 8228 to the internet. Keep RPC on localhost port 8227.

Point `ckb.rpc_url` in the fnn config at a CKB node for the same network. The stock testnet template uses `https://testnet.ckbapp.dev/`. Mainnet needs a mainnet endpoint.

## 2. Create the node

Install `fnn` built from this workspace's `fiber` checkout. From an empty data directory, start once with the official config for that network so `fnn` writes `config.yml` and the key, or copy `fiber/config/testnet/config.yml` (or `mainnet`) into the data directory as `config.yml`.

Stop `fnn`, then from this directory:

```bash
cargo run -- apply --policy policy/testnet.toml --fnn-config /var/lib/studio-lsp/config.yml
cargo run -- check --policy policy/testnet.toml
```

In `config.yml`, set a public address. Use your DNS name or public IP, and the port you opened:

```yaml
fiber:
  announced_addrs:
    - "/dns4/lsp.example.com/tcp/8228"
```

`apply` removes `jit_lsp_pubkey`, `jit_max_amount`, and `jit_max_open_fee` if they are present. This node serves channels. It does not also register as a wallet.

## 3. Fund the CKB wallet

Start `fnn` once and read the CKB address it logs for the node wallet. Send it enough CKB to cover:

- one channel per expected receiver, up to `max_funding_shannons`
- the 1,000 CKB it locks on every wallet-opened channel
- on-chain fees

A receive fails while the node is offline or the wallet cannot cover the next channel. Leave headroom above a single channel.

## 4. Run it

Example systemd unit. Change the paths to your `fnn` binary and data directory.

```ini
[Unit]
Description=Studio LSP
After=network-online.target

[Service]
ExecStart=/usr/local/bin/fnn -c /var/lib/studio-lsp/config.yml -d /var/lib/studio-lsp
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Confirm RPC answers on the server:

```bash
curl -s http://127.0.0.1:8227 \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"node_info","params":[]}'
```

`node_id` is the 66-character pubkey. The startup log prints the listen multiaddr that ends in `/p2p/Qm...`. Build the public multiaddr from that peer id and the DNS name you announced, for example `/dns4/lsp.example.com/tcp/8228/p2p/Qm...`.

## 5. Point Fiber Studio at this node

From this directory, with the pubkey and multiaddr from the running node:

```bash
cargo run -- publish \
  --policy policy/testnet.toml \
  --pubkey 02abc... \
  --multiaddr /dns4/lsp.example.com/tcp/8228/p2p/Qm... \
  --liquidity-json ../fiber-studio/app/shared/liquidity.json
```

Repeat with `policy/mainnet.toml` when the mainnet node is funded. `publish` changes only the network named in the policy.

Rebuild Fiber Studio and ship that build. `liquidity.json` is compiled into the app. On the next launch each wallet:

- dials Studio LSP and keeps retrying
- registers so incoming payments can open a channel
- moves the user's CKB into a private channel when they send
- lets Studio LSP fund a private channel when they receive

Wallets still learn the public graph from Fiber bootnodes. Payment channels go through Studio LSP.

## 6. Check a wallet

On testnet, with a Fiber Studio build that includes the published entry:

1. Start the wallet. The log should say payments can open a receive channel with Studio LSP. The network page should show Studio LSP connected without anyone pasting an address.
2. Create a CKB invoice before any channel exists. Pay it from another wallet that already has a route to this node. The sender pays the 10 CKB open fee. This node locks the payment, the 99 CKB cell reserve, and spare inbound up to the cushion.
3. From a wallet with on-chain CKB and no channel, send a payment. Fiber Studio opens a private channel funded by that user. This node accepts and locks its own 1,000 CKB on the far side, which is what that user can later receive.

## Operating

Keep the process up. A stopped node cannot open receive channels.

Watch the CKB balance. When it drops near one `max_funding_shannons`, fund it again before the next receive.

`max_inflight_opens` is how many channels may be waiting for confirmation at once (8 on testnet, 4 on mainnet). Extra receives wait or fail until one of those confirms.

To change fees or caps, edit the policy, run `apply` again, restart `fnn`, then `publish` and ship a new Fiber Studio build so wallets pick up the new caps.
