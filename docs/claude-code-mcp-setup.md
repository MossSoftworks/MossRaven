# Wiring MossRaven into Claude Code (Mode B)

MossRaven's orchestration service (`mossraven-service.exe`) exposes its outer control surface as an MCP server. **Adding it to Claude Code makes Claude Code itself Tier 1** — you drive the search interactively from your subscription, no Anthropic API key needed in the service. See [SPEC §4.1](../SPEC.md#41-tier-1-driver--dual-mode-tieronedriver-trait) for the architectural context.

> **v1 status note:** `mossraven-service`'s MCP server is currently a stub — it logs that it would serve stdio JSON-RPC and exits. The wiring instructions below are the **end-state setup** so you can register it now; the tool calls will return errors until the real framer lands in `crates/mcp-server/src/lib.rs::serve_stdio`. The shell, the service binary, and the registration are all in place.

---

## One-time setup (the recommended path — local stdio MCP)

**1. Verify Claude Code can see the service binary.**

```powershell
Test-Path C:\#AppProjects\MossRaven\dist\mossraven-service.exe
# True
```

**2. Register the MCP server via Claude Code CLI.**

```powershell
claude mcp add mossraven -- "C:\#AppProjects\MossRaven\dist\mossraven-service.exe"
```

This writes a `.mcp.json` entry in your current project directory (run it from the MossRaven dir or a project where you want MossRaven tools available). To register **globally** for every Claude Code project:

```powershell
claude mcp add --scope user mossraven -- "C:\#AppProjects\MossRaven\dist\mossraven-service.exe"
```

**3. Verify the wiring.**

```powershell
claude mcp list
# Should show: mossraven : C:\#AppProjects\MossRaven\dist\mossraven-service.exe
```

Inside a Claude Code session run `/mcp` — it'll list connected servers and the tools each exposes. MossRaven publishes:

| Tool | Purpose |
|---|---|
| `seed_hypothesis` | Start a new search from a free-text concept |
| `run_search` | Run N generations of the inner loop |
| `read_archive` | Snapshot the MAP-Elites grid |
| `inspect_cell` | Get the elite build in a specific cell |
| `get_frontier` | Pareto frontier across novelty × power × cost |

---

## Manual `.mcp.json` (if you prefer editing the config directly)

Project-level (`.mcp.json` in any project root):

```json
{
  "mcpServers": {
    "mossraven": {
      "command": "C:\\#AppProjects\\MossRaven\\dist\\mossraven-service.exe",
      "args": [],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

User-level (`%USERPROFILE%\.claude.json`, `mcpServers` key):

```json
{
  "mcpServers": {
    "mossraven": {
      "command": "C:\\#AppProjects\\MossRaven\\dist\\mossraven-service.exe",
      "args": []
    }
  }
}
```

---

## Critical: keep `ANTHROPIC_API_KEY` out of Claude Code's environment

If `ANTHROPIC_API_KEY` is set in the shell environment that launches Claude Code, **Claude Code silently switches off your subscription and bills against the API key**. The whole point of Mode B is to use your Pro/Max subscription quota, not the API.

```powershell
# Verify the key is NOT set in any scope Claude Code inherits from:
[Environment]::GetEnvironmentVariable("ANTHROPIC_API_KEY", "User")     # should be empty
[Environment]::GetEnvironmentVariable("ANTHROPIC_API_KEY", "Machine")  # should be empty
$env:ANTHROPIC_API_KEY                                                  # should be empty
```

If you need the key set for *other* projects, set it per-shell via `$env:ANTHROPIC_API_KEY = "..."` rather than persisting it. Mode A (the autonomous API driver inside `mossraven-service`) reads its own `MOSSRAVEN_ANTHROPIC_API_KEY` so the two don't collide.

---

## Driving a search from Claude Code (once the MCP server is live)

Once registered and the MCP framer is implemented, the flow is:

```
You: seed a hypothesis about cold DoT scaled through an obscure ailment

Claude Code: calls mossraven.seed_hypothesis({concept: "cold DoT ..."})
             → service returns the parsed hypothesis

You: run 10 generations on it

Claude Code: calls mossraven.run_search({generations: 10})
             → service grinds the inner loop (mutate → surrogate-prune → PoB-sim → archive-place)
             → returns a summary

You: what cells filled?

Claude Code: calls mossraven.read_archive()
             → service returns the grid

You: inspect the cell at chaos / energy-shield / boss / unique-driven

Claude Code: calls mossraven.inspect_cell({...})
             → service returns the build's PoB XML + lineage
```

The inner loop runs Claude-free — your subscription quota is only consumed for the seed_hypothesis / read_archive / inspect_cell calls Claude Code makes between generations.

---

## Cowork (subscription-backed remote driver)

Cowork connects to MCP servers as **custom connectors over HTTP**, originating from Anthropic's cloud. This requires the MCP server to be publicly reachable. Local stdio (above) is **strongly preferred** because no public exposure is needed. If you want Cowork specifically, the path is:

1. Add an HTTP transport to `mcp-server/src/lib.rs` (Axum is already in deps).
2. Forward an external port (pfSense / Cloudflare Tunnel) to the service.
3. Add a custom connector in Cowork settings pointing at the public URL.

See SPEC §4.1's table for the security trade-offs.
