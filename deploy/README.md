# Deploying a `mossraven-node`

`mossraven-node` is a single static Rust binary. **One binary, two operating systems.** No code differs between Linux and Windows — the same Cargo manifest cross-compiles for both.

## Cross-compile

From a dev machine with the right Rust targets installed:

```bash
# Linux x86_64, fully static (musl, no glibc dependency at runtime)
rustup target add x86_64-unknown-linux-musl
cargo build -p mossraven-node --target x86_64-unknown-linux-musl --release
# → target/x86_64-unknown-linux-musl/release/mossraven-node

# Windows x86_64
rustup target add x86_64-pc-windows-msvc
cargo build -p mossraven-node --target x86_64-pc-windows-msvc --release
# → target/x86_64-pc-windows-msvc/release/mossraven-node.exe
```

Both binaries are self-contained except for the vendored `PathOfBuilding-PoE2` directory which each node operator clones themselves on first install (per GGG's fan-content policy — see SPEC §9).

## Per-platform install

| Platform | Template | Style |
|---|---|---|
| Linux (any systemd distro) | [`linux/mossraven-node.service`](linux/mossraven-node.service) + [`linux/install.sh`](linux/install.sh) | systemd unit, restarts on failure, runs as a dedicated `mossraven` user |
| Windows 10 / 11 | [`windows/install-mossraven-node.ps1`](windows/install-mossraven-node.ps1) + [`windows/uninstall-mossraven-node.ps1`](windows/uninstall-mossraven-node.ps1) | Task Scheduler entry, runs on user logon, restarts on failure |

Both expect the binary + vendored PoB2 to live at a known path on disk and read the same env vars: `MOSSRAVEN_NODE_BEARER`, `MOSSRAVEN_NODE_BIND`, `MOSSRAVEN_POB_PATH`.

## Why Task Scheduler instead of a real Windows service?

A "real" Windows service requires the binary to implement service control codes (`SCM` handlers — start/stop/pause/continue) which means linking the `windows-service` crate and writing platform-specific code in `mossraven-node`. We deliberately keep `mossraven-node` 100% portable Rust so the same source tree builds for both OSes.

Task Scheduler with `Run whether user is logged on or not` is functionally equivalent for a long-running CPU worker: starts on boot, restarts on failure, no GUI session required, all configurable. If a power user wants real service semantics later, a wrapper like NSSM (`nssm install MossRavenNode "C:\path\to\mossraven-node.exe"`) handles it without changing our binary.

## Shared-secret bearer token

Both install paths require you to set `MOSSRAVEN_NODE_BEARER` to a value that matches what the orchestrator's `Tier3Config::Remote { bearer }` expects. Use a random secret (e.g. `openssl rand -hex 32`). Internet-exposed nodes need real isolation beyond bearer-only auth (mTLS, IP allowlist, or a VPN-only listen interface) — out of v1 scope, documented in SPEC §6.

## Verifying a fresh install

```bash
# from any machine that can reach the node
curl http://<node-ip>:5380/health
# → {"version":"0.1.0","pob2_version":"unknown","cores":N,"in_flight":0}

curl -X POST http://<node-ip>:5380/score \
     -H "Authorization: Bearer $MOSSRAVEN_NODE_BEARER" \
     -H "Content-Type: application/json" \
     -d '{"batch_id":"smoke","variants":[]}'
# → {"batch_id":"smoke","results":[]}
```
