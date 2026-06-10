# MossRaven

POE2 build-discovery engine. LLM-driven quality-diversity evolutionary search with Path of Building 2 as the fitness function. Finds builds the meta hasn't surfaced — "should theoretically work," "works if we tweak xyz."

Not a Q&A oracle. **The MAP-Elites archive is the product.**

See [SPEC.md](SPEC.md) for the full design. See [docs/pob-deepdive.md](docs/pob-deepdive.md) for the pob-engine extraction decision.

---

## Status

**Working pipeline** (updated 2026-06-10). Every tier runs end-to-end; finalists persist to `%APPDATA%\Moss\MossRaven\data\finalists\`. Authoritative requirements live in `SPEC.md` (see §1.1 for the definition of done). Run `scripts/windows-validate.ps1` for the full build/test/smoke gate.

| Component | State |
|---|---|
| `crates/pob` | real engine — fork-and-trim of poe2-agent (~3.9k loc); `init_smoke` + `parity` fixture tests |
| `crates/archive` | MAP-Elites grid, atomic disk persistence + resume, import-code encode/decode |
| `crates/surrogate` | OpenAI-compat client + **failover chain (Cerebras→Groq→Gemini)** with 429 backoff/cooldown, datamined-vocab grounding, `MockSurrogate` |
| `crates/dreamer` | Tier-1/5 drivers: Anthropic **or any OpenAI-compat endpoint** (Gemini/Groq/Ollama — free solo Mode A); Mode B external marker |
| `crates/mcp-server` | full stdio JSON-RPC framer; 7 tools including `save_finalists` (Mode B write-back) |
| `crates/core` + `tier3.rs` | cascade evaluator; mutation applier (gem level/quality/swap + weapon-set swap); `LocalBackend` pool + `RemoteBackend` |
| `bin/mossraven-service` | daemon (MCP stdio) / `--headless` / `--tool` one-shots; finalist persistence; session + archive state; PoB2 version stamping |
| `bin/mossraven-node` | real `/score` via PobParser pool; bearer auth; `/health` reports workers + pob2 version |
| `ui/MossRaven` (WPF) | live archive pane (click = copy import code), Tier-5 finalist cards with guides, **finalist History browser** (every saved run → full guide detail window), concept history, archive file-watcher |

---

## Running it — solo vs. with Claude

Two LLM seats exist: **Tier 2** (mutation proposals, thousands of cheap calls) and
**Tier 1/5** (hypothesis + finalist guides, a handful of quality calls). Both run on
free tiers; keys are env vars, never committed.

### Solo (free, no Anthropic spend)

1. Grab free keys (no card required for any of these):
   [Groq](https://console.groq.com/keys) · [Google AI Studio](https://aistudio.google.com/apikey) · [Cerebras](https://cloud.cerebras.ai). Any subset works; more = more failover headroom.
2. Set them for your shell / before launching the WPF:
   ```powershell
   $env:GROQ_API_KEY   = "gsk_..."   # Tier-2 chain + Tier-1/5 fallback
   $env:GEMINI_API_KEY = "AIza..."   # Tier-2 chain + preferred free Tier-1/5
   $env:CEREBRAS_API_KEY = "csk-..." # Tier-2 chain (first in line)
   .\dist\MossRaven.exe
   ```
3. In the app: type a concept → **Seed** → **Run** (watch cells fill) → **Synthesize**
   (Tier-5 guides, runs on Gemini/Groq when no Anthropic key is set) → **History** to
   browse every saved run; click a build for its full leveling/bossing/swap guide.

Optional upgrades: `MOSSRAVEN_ANTHROPIC_API_KEY` switches Tier-1/5 to Claude
(noticeably better guides; a Synthesize click on Haiku costs ~$0.02 — pennies, not a
subscription), or `MOSSRAVEN_T1_BASE_URL`/`_MODEL` points Tier-1/5 at local Ollama
(`http://localhost:11434/v1` + `qwen2.5:32b-instruct`) for fully-offline guides.

### With Claude Code (Mode B — runs on the subscription, $0 marginal)

1. Open the WPF (it's the dashboard) and open Claude Code in this repo.
2. Tell Claude the concept. Claude drives `seed_hypothesis → run_search →
   get_frontier` over MCP/CLI; the WPF's file-watcher live-refreshes the archive pane
   as cells fill.
3. Claude curates the frontier itself (no API key burned) and calls `save_finalists`
   — the run appears under **History** in the WPF.

Do **not** set `ANTHROPIC_API_KEY` in Claude Code's own shell (it silently flips
billing from subscription to API).

---

## Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Rust | stable (1.85+) | `rustup` is fine; `rust-toolchain.toml` pins the channel |
| .NET SDK | 10.0+ | for the WPF shell |
| Git | recent | for `vendor/` clones |
| C toolchain | required for `mlua` `vendored` feature | Windows: MSVC build tools (`cl.exe`); Linux: gcc/clang + cmake |

Vendored upstream (cloned, never committed):
- `vendor/PathOfBuilding-PoE2` — already cloned by setup; PoB2 itself.

---

## Building

**Windows dev hosts: one-time LuaJIT bootstrap fix.** If your Windows host blocks CWD execute-resolution (Group Policy / WDAC / `NoDefaultCurrentDirectoryInExePath`), LuaJIT's MSVC build script fails on `'minilua' is not recognized`. Run the patch script once after first checkout (and again after any `cargo update` that bumps `luajit-src`):

```powershell
.\scripts\patch-luajit-msvc.ps1
# (if you had a partial build cached) cargo clean -p mlua-sys
```

It's idempotent — re-running on an already-patched checkout is a no-op. See [docs/pob-deepdive.md "Post-extraction status"](docs/pob-deepdive.md) for why this is needed and the durable fix plan.

**Standard build:**

```bash
# Rust workspace (all crates + both binaries)
cargo build --workspace

# Release
cargo build --workspace --release

# WPF shell (Debug)
dotnet build ui/MossRaven/MossRaven.csproj

# WPF shell (Release single-file publish — produces ONE .exe)
dotnet publish ui/MossRaven/MossRaven.csproj -c Release -r win-x64 --self-contained -o dist/
```

**Assemble a distribution:**

```powershell
cargo build --workspace --release
dotnet publish ui/MossRaven/MossRaven.csproj -c Release -r win-x64 --self-contained -o dist/
Copy-Item target/release/mossraven-service.exe dist/
Copy-Item target/release/mossraven-node.exe dist/
# dist/ now has MossRaven.exe + the two Rust sidecars
```

After `cargo build --release`, copy `target/release/mossraven-service.exe` next to `MossRaven.exe` so the shell can launch it as a subprocess:

```powershell
Copy-Item target/release/mossraven-service.exe ui/MossRaven/bin/Debug/net10.0-windows/
```

(Production packaging will automate this — for v1 dev, do it manually.)

---

## Running each piece in isolation

### `mossraven-service` (the orchestration core)

```powershell
# Default: reads vendor/PathOfBuilding-PoE2; runs one stub generation; exits.
cargo run -p mossraven-service

# With explicit PoB path:
$env:MOSSRAVEN_POB_PATH = "C:\path\to\PathOfBuilding-PoE2"; cargo run -p mossraven-service

# Verbose logging:
$env:RUST_LOG = "mossraven_service=debug,mossraven_core=debug"; cargo run -p mossraven-service
```

### `mossraven-node` (the power-user farm worker)

```powershell
# Defaults: binds 0.0.0.0:5380, bearer "dev-bearer-change-me", PoB at vendor/PathOfBuilding-PoE2.
cargo run -p mossraven-node

# Configured:
$env:MOSSRAVEN_NODE_BEARER = "your-secret"
$env:MOSSRAVEN_NODE_BIND   = "0.0.0.0:5380"
$env:MOSSRAVEN_POB_PATH    = "/opt/PathOfBuilding-PoE2"
cargo run -p mossraven-node --release
```

Smoke test once it's running:

```powershell
curl http://localhost:5380/health
# {"version":"0.1.0","pob2_version":"unknown","cores":24,"in_flight":0}

curl -X POST http://localhost:5380/score `
     -H "Authorization: Bearer dev-bearer-change-me" `
     -H "Content-Type: application/json" `
     -d '{"batch_id":"smoke","variants":[]}'
```

### WPF shell

Built and launched via `dotnet run` in dev:

```powershell
dotnet run --project ui/MossRaven/MossRaven.csproj
```

The shell tries to launch `mossraven-service.exe` from its own directory. If absent it runs in "disconnected" mode and logs that to the status pane.

---

## Drive modes — Tier 1

### Mode A (API, headless, automated)

```powershell
$env:ANTHROPIC_API_KEY = "sk-ant-..."
$env:MOSSRAVEN_DREAMER_MODE = "api"
cargo run -p mossraven-service --release
```

Metered at API rates. Schedule via Task Scheduler / systemd / cron for unattended long runs.

### Mode B (subscription, interactive — Claude Code or Cowork)

Mode B drives the service from outside via MCP. **Do not set `ANTHROPIC_API_KEY`** in the Claude Code / Cowork shell environment — Claude Code silently falls back to API billing if it is set.

**Claude Code (local stdio MCP — preferred):**

```powershell
claude mcp add mossraven -- "C:\#AppProjects\MossRaven\dist\mossraven-service.exe"
# or, for all projects globally:
claude mcp add --scope user mossraven -- "C:\#AppProjects\MossRaven\dist\mossraven-service.exe"
```

Verify with `claude mcp list` and inside a session with `/mcp`. Then ask Claude Code to "seed a hypothesis around cold DoT" — it'll discover the `seed_hypothesis` / `run_search` / `read_archive` / `inspect_cell` / `get_frontier` tools and drive the search.

Full setup with gotchas (key-leak prevention, `.mcp.json` manual format, Cowork remote path): **[docs/claude-code-mcp-setup.md](docs/claude-code-mcp-setup.md)**.

**Cowork (custom connector, HTTP):** the MCP HTTP transport originates from Anthropic's cloud, so the service must be publicly reachable. This needs a pfSense port-forward, Cloudflare Tunnel, or similar. See SPEC §4.1 for the security trade-off. v1 ships local stdio as the recommended Mode B; Cowork support comes after testing.

---

## Surrogate provider — Tier 2

Swap providers by config; no code change. v1 stub config in `mossraven-service` reads:

```toml
[surrogate]
base_url = "https://api.cerebras.ai/v1"
model    = "gpt-oss-120b"
api_key_env = "CEREBRAS_API_KEY"
temperature = 0.4
```

Drop-in alternatives:

- Local Ollama: `base_url = "http://localhost:11434/v1"`, `api_key_env` omitted
- Groq: `base_url = "https://api.groq.com/openai/v1"`, `model = "llama-3.3-70b-versatile"`
- OpenRouter: `base_url = "https://openrouter.ai/api/v1"`, model of your choice

---

## Deploying a farm node (`mossraven-node`)

A node is a single static Rust binary, **same source for Linux and Windows**. Power users add idle gaming PCs or homelab VMs to the pool by installing the binary and pointing the orchestrator's `Tier3Config::Remote` at them.

See [deploy/README.md](deploy/README.md) for templates:
- **Linux** — [`deploy/linux/mossraven-node.service`](deploy/linux/mossraven-node.service) systemd unit + [`install.sh`](deploy/linux/install.sh)
- **Windows** — [`deploy/windows/install-mossraven-node.ps1`](deploy/windows/install-mossraven-node.ps1) (Task Scheduler entry)

Cross-compile:

```bash
# Static Linux binary (no glibc dep)
rustup target add x86_64-unknown-linux-musl
cargo build -p mossraven-node --target x86_64-unknown-linux-musl --release

# Windows
rustup target add x86_64-pc-windows-msvc
cargo build -p mossraven-node --target x86_64-pc-windows-msvc --release
```

---

## Tier 3 backend — local vs remote

```toml
# Single-machine (default for gaming-rig install)
[tier3]
mode = "local"

# Power-user mode: fan out across a pool of mossraven-node URLs
[tier3]
mode = "remote"
node_urls = [
  "http://node1.lan:5380",
  "http://node2.lan:5380",
  "http://10.0.0.42:5380",
]
bearer = "your-shared-secret"
```

Switching is a config-file change; no rebuild required. v1 wires the `RemoteBackend` HTTP path through to `mossraven-node`'s `/score`, which currently returns `error` for every variant. Real scoring lands once the in-process Tier-3 is validated against desktop PoB2.

---

## Benchmarking each tier in isolation

(Pending. Bench harness is a follow-up after the pob crate is real. The shape will be:)

```powershell
# Tier 3 local — variants per second per core
cargo bench -p mossraven-core --bench tier3_local

# Tier 3 remote — wall-clock latency vs batch size against a running mossraven-node
cargo bench -p mossraven-core --bench tier3_remote -- --node-url=http://node1.lan:5380

# Tier 2 surrogate — round-trip latency, tokens/sec, batch throughput
cargo bench -p mossraven-surrogate --bench cerebras
```

---

## Licensing & redistribution

- This repository: MIT.
- `crates/pob` will be a fork-and-trim of [poe2-agent](https://github.com/SFerenczy/poe2-agent) (MIT, © Sándor Ferenczy). See `crates/pob/NOTICE`.
- `vendor/PathOfBuilding-PoE2` is upstream MIT, but the GGG game data inside it is not redistributable.
- **Do not bundle `vendor/` or any extracted skill/passive/item data into published binaries.** GGG fan-content policy is non-commercial and prohibits asset redistribution. Distributed builds clone `vendor/PathOfBuilding-PoE2` from upstream on first run.

---

## Repo layout

```
MossRaven/
├── SPEC.md                          canonical spec
├── README.md                        this file
├── Cargo.toml                       workspace root
├── rust-toolchain.toml              pins stable Rust
├── docs/
│   └── pob-deepdive.md              extraction-strategy report for pob crate
├── crates/
│   ├── pob/                         PoB2 headless engine (stub; salvage pending)
│   ├── archive/                     MAP-Elites archive
│   ├── surrogate/                   Tier 2 (OpenAI-compatible provider trait)
│   ├── dreamer/                     Tier 1 driver (Mode A + Mode B)
│   ├── core/                        orchestration + Tier 3 dispatch
│   ├── mcp-server/                  MCP control-surface server
│   └── node-protocol/               wire types for service ↔ node
├── bin/
│   ├── mossraven-service/             the main orchestration daemon
│   └── mossraven-node/                power-user farm worker
├── ui/
│   └── MossRaven/                     WPF shell (.NET 10)
├── vendor/
│   └── PathOfBuilding-PoE2/         git-cloned, gitignored
└── scratch/
    └── poe2-agent/                  reference clone, for the pob salvage step
```
