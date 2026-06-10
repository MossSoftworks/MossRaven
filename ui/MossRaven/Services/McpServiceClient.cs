using System;
using System.Diagnostics;
using System.IO;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using System.Collections.Concurrent;

namespace MossRaven.Services;

/// <summary>
/// Minimal MCP-over-stdio client. Launches <c>mossraven-service.exe</c> as a
/// subprocess and exchanges JSON-RPC 2.0 framed messages over stdin/stdout.
///
/// v1 status: scaffold. The real MCP framer (Content-Length headers per the
/// spec) and tool-call dispatch land alongside the matching Rust-side server
/// implementation. For now the client simulates the wire by writing a
/// newline-delimited JSON request and echoing back a placeholder.
/// </summary>
public sealed class McpServiceClient : IDisposable
{
    private readonly string _exePath;
    private readonly Action<string> _log;
    /// <summary>Env overrides applied to the spawned service (provider keys,
    /// tier assignment). Re-read on every StartAsync so a service reconnect
    /// picks up freshly-saved settings.</summary>
    private readonly Func<IDictionary<string, string>>? _envProvider;
    private Process? _proc;
    private long _nextId = 1;
    private readonly ConcurrentDictionary<long, TaskCompletionSource<JsonElement>> _pending = new();

    public McpServiceClient(
        string exePath,
        Action<string> log,
        Func<IDictionary<string, string>>? envProvider = null)
    {
        _exePath = exePath;
        _log = log;
        _envProvider = envProvider;
    }

    public async Task StartAsync()
    {
        if (!File.Exists(_exePath))
        {
            _log($"[mcp] service binary not found at {_exePath} — running UI in disconnected mode");
            return;
        }

        var psi = new ProcessStartInfo
        {
            FileName = _exePath,
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            // The service writes UTF-8 tracing to stderr (em-dashes in log
            // lines). Without this, .NET decodes with the OEM codepage and
            // the UI log shows mojibake ("â€”").
            StandardErrorEncoding = System.Text.Encoding.UTF8,
            StandardOutputEncoding = System.Text.Encoding.UTF8,
        };
        if (_envProvider != null)
        {
            foreach (var (k, v) in _envProvider())
            {
                // Empty string intentionally BLANKS an inherited var (the
                // service treats empty as unset) — that's how disabling a
                // provider in Settings beats a machine-level setx key.
                psi.Environment[k] = v;
            }
        }
        _proc = Process.Start(psi) ?? throw new InvalidOperationException("Process.Start returned null");

        _ = Task.Run(ReadStderrLoop);
        _ = Task.Run(ReadStdoutLoop);

        _log($"[mcp] launched {_exePath} pid={_proc.Id}");
        await Task.CompletedTask;
    }

    private async Task ReadStderrLoop()
    {
        if (_proc?.StandardError is null) return;
        string? line;
        while ((line = await _proc.StandardError.ReadLineAsync()) is not null)
        {
            _log($"[service] {line}");
        }
    }

    private async Task ReadStdoutLoop()
    {
        if (_proc?.StandardOutput is null) return;
        string? line;
        while ((line = await _proc.StandardOutput.ReadLineAsync()) is not null)
        {
            // v1 stub: treat each line as a JSON-RPC response and match by `id`.
            try
            {
                using var doc = JsonDocument.Parse(line);
                if (doc.RootElement.TryGetProperty("id", out var idEl) &&
                    idEl.TryGetInt64(out var id) &&
                    _pending.TryRemove(id, out var tcs))
                {
                    tcs.TrySetResult(doc.RootElement.Clone());
                }
            }
            catch (JsonException ex)
            {
                _log($"[mcp parse] {ex.Message}: {line}");
            }
        }
    }

    public Task<string> SeedHypothesisAsync(string concept, CancellationToken ct = default)
        => CallToolAsync("seed_hypothesis", JsonSerializer.SerializeToElement(new { concept }), ct);

    public Task<string> RunSearchAsync(int generations, string? region, CancellationToken ct = default)
        => CallToolAsync("run_search", JsonSerializer.SerializeToElement(new { generations, region }), ct);

    public Task<string> ReadArchiveAsync(CancellationToken ct = default)
        => CallToolAsync("read_archive", JsonSerializer.SerializeToElement(new { }), ct);

    public Task<string> GetFrontierAsync(CancellationToken ct = default)
        => CallToolAsync("get_frontier", JsonSerializer.SerializeToElement(new { }), ct);

    /// <summary>
    /// Tier 5 — Claude reads the current frontier and produces 5–10 narrated
    /// finalists. Returns the JSON payload (the same shape the FileSystemWatcher
    /// path would produce); the WPF caller parses `finalists[]` directly.
    /// </summary>
    public Task<string> SynthesizeFinalistsAsync(CancellationToken ct = default)
        => CallToolAsync("synthesize_finalists", JsonSerializer.SerializeToElement(new { }), ct);

    private async Task<string> CallToolAsync(string tool, JsonElement args, CancellationToken ct)
    {
        if (_proc?.StandardInput is null)
        {
            return $"(service not running — would have called {tool})";
        }
        var id = Interlocked.Increment(ref _nextId);
        var tcs = new TaskCompletionSource<JsonElement>(TaskCreationOptions.RunContinuationsAsynchronously);
        _pending[id] = tcs;

        var req = new
        {
            jsonrpc = "2.0",
            id,
            method = "tools/call",
            @params = new { name = tool, arguments = args },
        };
        var json = JsonSerializer.Serialize(req);
        await _proc.StandardInput.WriteLineAsync(json);
        await _proc.StandardInput.FlushAsync();

        using var reg = ct.Register(() => tcs.TrySetCanceled());
        var result = await tcs.Task;

        // Unwrap the JSON-RPC envelope and the MCP content array for display.
        // Returns the most useful payload: structuredContent if present (our
        // server adds it), else content[0].text, else the raw error or whole result.
        return ExtractDisplayPayload(result);
    }

    /// <summary>
    /// Extract a human-friendly string from a JSON-RPC response.
    /// Order of preference: result.structuredContent (pretty JSON) →
    /// result.content[0].text → error.message → whole envelope.
    /// </summary>
    private static string ExtractDisplayPayload(JsonElement envelope)
    {
        if (envelope.TryGetProperty("error", out var err))
        {
            var msg = err.TryGetProperty("message", out var m) ? m.GetString() : err.ToString();
            return $"ERROR: {msg}";
        }
        if (!envelope.TryGetProperty("result", out var result))
        {
            return envelope.ToString();
        }
        if (result.TryGetProperty("structuredContent", out var sc))
        {
            return JsonSerializer.Serialize(sc, new JsonSerializerOptions { WriteIndented = true });
        }
        if (result.TryGetProperty("content", out var content) &&
            content.ValueKind == JsonValueKind.Array &&
            content.GetArrayLength() > 0)
        {
            var first = content[0];
            if (first.TryGetProperty("text", out var txt))
            {
                return txt.GetString() ?? string.Empty;
            }
        }
        return result.ToString();
    }

    public void Dispose()
    {
        try
        {
            if (_proc is { HasExited: false })
            {
                _proc.Kill(entireProcessTree: true);
            }
        }
        catch { /* best-effort */ }
        _proc?.Dispose();
    }
}
