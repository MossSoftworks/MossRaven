using System;
using System.Collections.Generic;
using System.IO;
using System.IO.Compression;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using MossRaven.Services;

namespace MossRaven;

public partial class MainWindow : Window
{
    private McpServiceClient _service; // replaced on settings-reconnect
    private Settings _settings;
    private readonly string _debugLogPath;

    private FileSystemWatcher? _archiveWatcher;
    private DateTime _lastArchiveRefresh = DateTime.MinValue;

    // Per-tier counters (matches the 5-tier UI model).
    private int _t1Count;   // hypotheses (Seed clicks)
    private int _t2Count;   // mutations proposed (sum of variants_proposed)
    private int _t3Count;   // PoB sims (sum of variants_scored)
    private int _t4Count;   // pruned (sum of variants_pruned — currently pre-sim)
    // Finalist count is the live archive cell count, read directly from the service.

    // Rolling per-tier iteration logs — kept short, last N lines only.
    private readonly System.Collections.Generic.List<string> _t1Lines = new();
    private readonly System.Collections.Generic.List<string> _t2Lines = new();
    private readonly System.Collections.Generic.List<string> _t3Lines = new();
    private readonly System.Collections.Generic.List<string> _t4Lines = new();
    private readonly System.Collections.Generic.List<string> _t6Lines = new();
    private readonly System.Collections.Generic.List<string> _t7Lines = new();
    private const int MaxLogLines = 30;

    public MainWindow()
    {
        InitializeComponent();
        _settings = SettingsService.Load();
        _debugLogPath = Path.Combine(Path.GetTempPath(), "mossraven-ui.log");
        try
        {
            if (File.Exists(_debugLogPath) && new FileInfo(_debugLogPath).Length > 1_000_000)
                File.Delete(_debugLogPath);
            File.AppendAllText(_debugLogPath,
                $"---- launch {DateTime.Now:O} pid={System.Diagnostics.Process.GetCurrentProcess().Id} ----{Environment.NewLine}");
        }
        catch { /* best-effort */ }

        // Open right-sized: tall enough that the Search Concept column
        // (concept + retool + preference rows) fits without scrolling,
        // clamped to the monitor's work area.
        Height = Math.Min(1080, SystemParameters.WorkArea.Height - 40);
        Width = Math.Min(1560, SystemParameters.WorkArea.Width - 60);

        _service = new McpServiceClient(
            LocateServiceExe(),
            AppendLog,
            () => _settings.ToServiceEnvironment());
        Loaded += async (_, _) =>
        {
            ApplyPersistedState();
            InitRework();
            InitTray();
            // Identity stamp: kills the which-exe-did-you-launch class of
            // bug forever (stale Debug builds masquerading as current).
            var exePath = Environment.ProcessPath ?? "?";
            var built = File.GetLastWriteTime(exePath);
            AppendLog($"[launch] exe={exePath} built={built:yyyy-MM-dd HH:mm}");
            Title = $"MossRaven — build {built:MM-dd HH:mm}";
            await ConnectServiceAsync();
            await RefreshArchiveAsync();
            _ = LoadVocabAsync();
            // History self-diagnostic on EVERY launch (log-only) — no click
            // needed for the [history] lines to appear.
            try { LoadFinalistHistoryViaService(); } catch (Exception hx) { AppendLog($"[history] startup scan failed: {hx.Message}"); }
            // PoB2 autolaunch every open (first open downloads the official
            // portable once). Toggle: Settings → AutoEmbedPob.
            if (_settings.AutoEmbedPob) EnsurePobEmbedded();
            // Watch archive.json so external --tool calls (Claude in the shell)
            // and WPF's own clicks both auto-refresh the right pane.
            SetupArchiveWatcher();
        };
        StateChanged += MainWindow_StateChanged;
        Closing += (_, _) => SaveStateBeforeClose();
        Closed += (_, _) => _service.Dispose();
    }

    private void ApplyPersistedState()
    {
        if (!string.IsNullOrEmpty(_settings.LastConcept))
        {
            ConceptInput.Text = _settings.LastConcept;
        }
    }

    private void SaveStateBeforeClose()
    {
        _settings.LastConcept = ConceptInput.Text ?? string.Empty;
        SettingsService.Save(_settings);
    }

    private static string LocateServiceExe()
    {
        var here = AppContext.BaseDirectory;
        return Path.Combine(here, "mossraven-service.exe");
    }

    private async System.Threading.Tasks.Task ConnectServiceAsync()
    {
        try
        {
            await _service.StartAsync();
            ServiceStateText.Text = "service: connected";
        }
        catch (Exception ex)
        {
            AppendLog($"[startup error] {ex.Message}");
            ServiceStateText.Text = "service: failed to start";
        }
    }

    private async void SeedButton_Click(object sender, RoutedEventArgs e)
    {
        var concept = ConceptInput.Text?.Trim() ?? string.Empty;
        if (concept.Length == 0) return;
        concept += PreferenceSuffix();

        SettingsService.AppendHistory(_settings, concept);
        _settings.LastConcept = concept;
        SettingsService.Save(_settings);

        AppendLog($"[seed] {concept}");
        try
        {
            var result = await _service.SeedHypothesisAsync(concept);
            AppendLog($"[seed reply] {result}");
            _t1Count++;
            AppendCapped(_t1Lines, $"#{_t1Count}: {concept}", MaxLogLines);
            UpdateTierCounters();
        }
        catch (Exception ex)
        {
            AppendLog($"[seed error] {ex.Message}");
        }
    }

    private async void RunButton_Click(object sender, RoutedEventArgs e)
    {
        const int gensThisRun = 10;
        AppendLog($"[run] {gensThisRun} generations");
        try
        {
            var result = await _service.RunSearchAsync(generations: gensThisRun, region: null);
            AppendLog($"[run reply] {result}");
            // Parse the structured totals from the reply and increment each tier.
            try
            {
                using var doc = System.Text.Json.JsonDocument.Parse(result);
                if (doc.RootElement.TryGetProperty("totals", out var totals))
                {
                    int proposed = totals.TryGetProperty("variants_proposed", out var p) ? p.GetInt32() : gensThisRun * 8;
                    int scored = totals.TryGetProperty("variants_scored", out var s) ? s.GetInt32() : 0;
                    int pruned = totals.TryGetProperty("variants_pruned", out var pr) ? pr.GetInt32() : 0;
                    int placed = totals.TryGetProperty("cells_filled_or_improved", out var pl) ? pl.GetInt32() : 0;
                    _t2Count += proposed;
                    _t3Count += scored;
                    _t4Count += pruned;
                    AppendCapped(_t2Lines, $"+{proposed} mutations over {gensThisRun} gens", MaxLogLines);
                    AppendCapped(_t3Lines, $"+{scored} sims, {placed} placed into cells", MaxLogLines);
                    AppendCapped(_t4Lines, $"+{pruned} pruned by interest+plausibility thresholds", MaxLogLines);
                }
            }
            catch { /* parsing best-effort */ }
            UpdateTierCounters();
        }
        catch (Exception ex)
        {
            AppendLog($"[run error] {ex.Message}");
        }
        await RefreshArchiveAsync();
    }

    private async void RefreshArchiveButton_Click(object sender, RoutedEventArgs e)
    {
        // Refresh always returns to the live archive view.
        FinalistHistoryScroller.Visibility = Visibility.Collapsed;
        BuildList.Visibility = Visibility.Visible;
        FinalistHistoryButton.Content = "History";
        await RefreshArchiveAsync();
    }

    // ----- Finalist history (saved finalists runs) -----

    private static string FinalistsRootDir() => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
        "Moss", "MossRaven", "data", "finalists");

    /// <summary>
    /// Toggle between the live archive view and the saved-runs history.
    /// History re-scans the finalists dir on every open (runs are small;
    /// a watcher would be overkill).
    /// </summary>
    private void FinalistHistoryButton_Click(object sender, RoutedEventArgs e)
    {
        if (FinalistHistoryScroller.Visibility == Visibility.Visible)
        {
            FinalistHistoryScroller.Visibility = Visibility.Collapsed;
            BuildList.Visibility = Visibility.Visible;
            FinalistHistoryButton.Content = "History";
            BuildListHint.Text = "Click a build to copy its PoB import code to clipboard.";
            return;
        }
        LoadFinalistHistoryViaService();
        BuildList.Visibility = Visibility.Collapsed;
        FinalistHistoryScroller.Visibility = Visibility.Visible;
        FinalistHistoryButton.Content = "Archive";
        BuildListHint.Text = "Expand a run, click a build for its full guide (PoB code copies from the detail window).";
    }

    private bool _historyRetryDone;

    /// <summary>Service-backed history: the engine process reads the data
    /// dir reliably in every launch context; this UI process demonstrably
    /// does not (Explorer-launched WPF saw exists=False on a real dir).
    /// Falls back to direct disk when the service is down.</summary>
    private async void LoadFinalistHistoryViaService()
    {
        // The daemon needs a few seconds on cold machines (PoB VM + dbs);
        // retry instead of falling straight back to the unreliable disk scan.
        string json = "";
        for (int attempt = 0; attempt < 4; attempt++)
        {
            try
            {
                json = await _service.ListFinalistRunsAsync();
                if (json.TrimStart().StartsWith("{")) break;
            }
            catch { /* not up yet */ }
            await System.Threading.Tasks.Task.Delay(2500);
        }
        try
        {
            if (json.Length == 0) throw new Exception("service did not answer after retries");
            using var doc = JsonDocument.Parse(json);
            if (doc.RootElement.TryGetProperty("runs", out var runs)
                && runs.ValueKind == JsonValueKind.Array)
            {
                FinalistHistoryPanel.Children.Clear();
                int rendered = 0;
                bool first = true;
                foreach (var run in runs.EnumerateArray())
                {
                    var ts = run.TryGetProperty("ts", out var t) ? t.GetString() ?? "?" : "?";
                    if (!run.TryGetProperty("finalists", out var arr)
                        || arr.ValueKind != JsonValueKind.Array
                        || arr.GetArrayLength() == 0) continue;
                    try
                    {
                        var exp = BuildRunExpanderFromArray(ts, arr, expandByDefault: first);
                        FinalistHistoryPanel.Children.Add(exp);
                        first = false;
                        rendered++;
                    }
                    catch (Exception rex)
                    {
                        AppendLog($"[history] run {ts} render failed: {rex.Message}");
                    }
                }
                AppendLog($"[history] service-backed: rendered {rendered} runs");
                if (rendered > 0) return;
            }
        }
        catch (Exception ex)
        {
            AppendLog($"[history] service path failed ({ex.Message}) — falling back to disk scan");
        }
        LoadFinalistHistory();
    }

    /// <summary>Expander from an in-memory finalists array (service-backed path).</summary>
    private Expander BuildRunExpanderFromArray(string tsName, JsonElement arr, bool expandByDefault)
    {
        var when = long.TryParse(tsName, out var unix)
            ? DateTimeOffset.FromUnixTimeSeconds(unix).ToLocalTime().ToString("yyyy-MM-dd HH:mm")
            : tsName;
        var firstTitle = arr[0].TryGetProperty("title", out var t) ? t.GetString() : "";
        var header = new TextBlock
        {
            Text = $"{when}   ·   {arr.GetArrayLength()} builds   ·   {firstTitle}…",
            FontWeight = FontWeights.SemiBold,
            FontSize = 13,
        };
        header.SetResourceReference(TextBlock.ForegroundProperty, "TextBrush");
        var body = new StackPanel { Margin = new Thickness(6, 4, 0, 6) };
        foreach (var fin in arr.EnumerateArray())
            body.Children.Add(BuildFinalistRow(fin));
        return new Expander
        {
            Header = header,
            Content = body,
            IsExpanded = expandByDefault,
            Margin = new Thickness(4, 3, 4, 3),
            BorderThickness = new Thickness(1),
        };
    }

    private void LoadFinalistHistory()
    {
        FinalistHistoryPanel.Children.Clear();
        var root = FinalistsRootDir();
        List<string> runDirs;
        try
        {
            var exists = Directory.Exists(root);
            runDirs = exists
                ? new List<string>(Directory.GetDirectories(root))
                : new List<string>();
            // Diagnostic for the persistent-empty report: print exactly what
            // this process computes and sees, plus the env-var view.
            var envRoot = Path.Combine(
                Environment.GetEnvironmentVariable("APPDATA") ?? "?",
                "Moss", "MossRaven", "data", "finalists");
            AppendLog($"[history] root={root} exists={exists} dirs={runDirs.Count} envRoot={envRoot} envExists={Directory.Exists(envRoot)}");
            if (runDirs.Count == 0 && Directory.Exists(envRoot))
            {
                runDirs = new List<string>(Directory.GetDirectories(envRoot));
                AppendLog($"[history] env-var path served {runDirs.Count} runs (SpecialFolder path was empty)");
            }
        }
        catch (Exception ex)
        {
            AppendLog($"[history] scan failed: {ex}");
            runDirs = new List<string>();
        }
        // Dir names are unix timestamps — numeric sort, newest first.
        runDirs.Sort((a, b) =>
        {
            long ta = long.TryParse(Path.GetFileName(a), out var x) ? x : 0;
            long tb = long.TryParse(Path.GetFileName(b), out var y) ? y : 0;
            return tb.CompareTo(ta);
        });

        if (runDirs.Count == 0)
        {
            // Transient-fs resilience (same incident class as the blank
            // archive): if the data dir exists but scans come back empty,
            // re-scan once after 3s before believing it.
            if (!_historyRetryDone && Directory.Exists(Path.GetDirectoryName(root) ?? root))
            {
                _historyRetryDone = true;
                AppendLog("[history] scan found 0 runs — retrying once in 3s");
                _ = System.Threading.Tasks.Task.Run(async () =>
                {
                    await System.Threading.Tasks.Task.Delay(3000);
                    await Dispatcher.InvokeAsync(LoadFinalistHistory);
                });
            }
            FinalistHistoryPanel.Children.Add(new TextBlock
            {
                Text = "No saved runs yet — Synthesize (or a Mode-B save_finalists) writes one per run.",
                FontSize = 12,
                Margin = new Thickness(10, 12, 10, 0),
                TextWrapping = TextWrapping.Wrap,
                Foreground = (Brush)FindResource("DimBrush"),
            });
            return;
        }

        bool first = true;
        int rendered = 0;
        foreach (var dir in runDirs)
        {
            try
            {
                var expander = BuildRunExpander(dir, expandByDefault: first);
                if (expander != null)
                {
                    FinalistHistoryPanel.Children.Add(expander);
                    first = false;
                    rendered++;
                }
            }
            catch (Exception ex)
            {
                AppendLog($"[history] run '{Path.GetFileName(dir)}' failed to render: {ex.Message}");
            }
        }
        AppendLog($"[history] rendered {rendered}/{runDirs.Count} runs");
        if (rendered == 0 && runDirs.Count > 0)
        {
            FinalistHistoryPanel.Children.Add(new TextBlock
            {
                Text = $"Found {runDirs.Count} run folders but none rendered — see Service status for per-run errors.",
                FontSize = 12.5,
                Margin = new Thickness(10, 12, 10, 0),
                TextWrapping = TextWrapping.Wrap,
                Foreground = (Brush)FindResource("DimBrush"),
            });
        }
    }

    /// <summary>One saved run → an Expander headed by time + summary, containing one clickable row per finalist.</summary>
    private Expander? BuildRunExpander(string runDir, bool expandByDefault)
    {
        JsonDocument doc;
        try
        {
            doc = JsonDocument.Parse(File.ReadAllText(Path.Combine(runDir, "finalists.json")));
        }
        catch
        {
            return null; // partial/corrupt run dir — skip silently
        }

        using (doc)
        {
            var rootEl = doc.RootElement;
            var arr = rootEl.ValueKind == JsonValueKind.Array
                ? rootEl
                : rootEl.TryGetProperty("finalists", out var f) ? f : default;
            if (arr.ValueKind != JsonValueKind.Array || arr.GetArrayLength() == 0) return null;

            var when = long.TryParse(Path.GetFileName(runDir), out var unix)
                ? DateTimeOffset.FromUnixTimeSeconds(unix).ToLocalTime().ToString("yyyy-MM-dd HH:mm")
                : Path.GetFileName(runDir);

            var firstTitle = arr[0].TryGetProperty("title", out var t) ? t.GetString() : "";
            var header = new TextBlock
            {
                Text = $"{when}   ·   {arr.GetArrayLength()} builds   ·   {firstTitle}…",
                FontWeight = FontWeights.SemiBold,
                FontSize = 12.5,
            };
            header.SetResourceReference(TextBlock.ForegroundProperty, "TextBrush");

            var body = new StackPanel { Margin = new Thickness(6, 4, 0, 6) };
            foreach (var fin in arr.EnumerateArray())
            {
                body.Children.Add(BuildFinalistRow(fin));
            }

            var exp = new Expander
            {
                Header = header,
                Content = body,
                IsExpanded = expandByDefault,
                Margin = new Thickness(4, 3, 4, 3),
                BorderThickness = new Thickness(1),
                Padding = new Thickness(6, 4, 6, 4),
            };
            exp.SetResourceReference(Control.BorderBrushProperty, "DimmerBrush");
            exp.SetResourceReference(Control.BackgroundProperty, "BgBrush");
            exp.SetResourceReference(Control.ForegroundProperty, "TextBrush");
            return exp;
        }
    }

    /// <summary>Clickable summary row for one finalist; click opens the full-guide detail window.</summary>
    private Border BuildFinalistRow(JsonElement fin)
    {
        string Str(string key) => fin.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.String
            ? v.GetString() ?? "" : "";

        var title = Str("title");
        var oneLiner = Str("one_liner");
        var statsBits = new List<string>();
        if (fin.TryGetProperty("key_stats", out var ks) && ks.ValueKind == JsonValueKind.Array)
        {
            foreach (var s in ks.EnumerateArray())
            {
                var label = s.TryGetProperty("label", out var l) ? l.GetString() : "";
                var value = s.TryGetProperty("value", out var v) ? v.GetString() : "";
                statsBits.Add($"{value} {label}");
            }
        }

        var panel = new StackPanel();
        var titleTb = new TextBlock { Text = title, FontWeight = FontWeights.SemiBold, FontSize = 12.5 };
        titleTb.SetResourceReference(TextBlock.ForegroundProperty, "TextBrush");
        panel.Children.Add(titleTb);
        var statsTb = new TextBlock
        {
            Text = string.Join("    ", statsBits),
            FontFamily = new FontFamily("Cascadia Mono, Consolas"),
            FontSize = 11,
            Margin = new Thickness(0, 2, 0, 0),
        };
        statsTb.SetResourceReference(TextBlock.ForegroundProperty, "DimBrush");
        panel.Children.Add(statsTb);
        var olTb = new TextBlock
        {
            Text = oneLiner,
            FontSize = 11,
            TextWrapping = TextWrapping.Wrap,
            Margin = new Thickness(0, 2, 0, 0),
        };
        olTb.SetResourceReference(TextBlock.ForegroundProperty, "DimmerBrush");
        panel.Children.Add(olTb);

        var row = new Border
        {
            Child = panel,
            CornerRadius = new CornerRadius(5),
            BorderThickness = new Thickness(1),
            Padding = new Thickness(10, 7, 10, 7),
            Margin = new Thickness(2, 3, 8, 3),
            Cursor = System.Windows.Input.Cursors.Hand,
            Background = Brushes.Transparent,
        };
        row.SetResourceReference(Border.BorderBrushProperty, "DimmerBrush");
        row.MouseEnter += (_, _) => row.SetResourceReference(Border.BackgroundProperty, "HoverBrush");
        row.MouseLeave += (_, _) => row.Background = Brushes.Transparent;

        // Capture everything the detail window needs NOW — the JsonDocument
        // is disposed when BuildRunExpander returns.
        var tags = new List<string>();
        if (fin.TryGetProperty("tags", out var tg) && tg.ValueKind == JsonValueKind.Array)
        {
            foreach (var x in tg.EnumerateArray())
                if (x.ValueKind == JsonValueKind.String) tags.Add(x.GetString() ?? "");
        }
        var sections = new List<(string, string)> { ("Why it works", Str("why_it_works")) };
        if (fin.TryGetProperty("guide", out var g) && g.ValueKind == JsonValueKind.Object)
        {
            string G(string k) => g.TryGetProperty(k, out var v) && v.ValueKind == JsonValueKind.String
                ? v.GetString() ?? "" : "";
            sections.Add(("Leveling", G("leveling")));
            sections.Add(("Endgame (bossing & gearing)", G("endgame")));
            sections.Add(("Clear / boss loadout swap", G("loadout_swap")));
            sections.Add(("Playtest notes", G("playtest_notes")));
        }
        var code = Str("pob_import_code");
        var statsLine = string.Join("    ", statsBits);
        var pobXml = Str("pob_xml");

        row.MouseLeftButtonUp += (_, _) =>
        {
            // History click behaves like a Builds-list click: straight into
            // the live embedded PoB (view-only), plus the guide window.
            if (pobXml.Length > 100)
                PushBuildToPob(pobXml, title);
            var win = new FinalistDetailWindow(title, oneLiner, tags, statsLine, sections, code)
            {
                Owner = this,
                ShowInTaskbar = false,
            };
            win.Show();
        };
        return row;
    }

    /// <summary>
    /// Tiers 6+7 — ask the service to synthesize finalists from the current
    /// frontier. The button stays disabled while the Anthropic call runs (it
    /// can take 10–30s for the curate pass at max_tokens=4096). On success
    /// the right pane re-renders as the curated list; on failure we log to
    /// ServiceLog and leave the existing archive view in place.
    /// </summary>
    private async void SynthesizeFinalistsButton_Click(object sender, RoutedEventArgs e)
    {
        SynthesizeFinalistsButton.IsEnabled = false;
        var origLabel = SynthesizeFinalistsButton.Content;
        SynthesizeFinalistsButton.Content = "Synthesizing…";
        BuildListHint.Text = "Asking Claude to curate the frontier…";
        try
        {
            var json = await _service.SynthesizeFinalistsAsync();
            RenderFinalists(json);
            // Tier 6/7 pane counters from the v2 pipeline result.
            try
            {
                using var doc = System.Text.Json.JsonDocument.Parse(json);
                var root = doc.RootElement;
                if (root.TryGetProperty("pool_size", out var ps))
                {
                    T6HeaderCount.Text = $"pool = {ps.GetInt32()}";
                    AppendCapped(_t6Lines, $"pool of {ps.GetInt32()} selected from the frontier", MaxLogLines);
                }
                if (root.TryGetProperty("finalists", out var fl) && fl.ValueKind == System.Text.Json.JsonValueKind.Array)
                {
                    T7HeaderCount.Text = $"n = {fl.GetArrayLength()}";
                    foreach (var f in fl.EnumerateArray())
                        AppendCapped(_t7Lines, f.TryGetProperty("title", out var t) ? (t.GetString() ?? "build") : "build", MaxLogLines);
                }
                T6Log.Text = _t6Lines.Count == 0 ? "(no pools yet — run Synthesize)" : string.Join(System.Environment.NewLine, _t6Lines);
                T7Log.Text = _t7Lines.Count == 0 ? "(no curations yet)" : string.Join(System.Environment.NewLine, _t7Lines);
            }
            catch { /* counters are cosmetic */ }
        }
        catch (Exception ex)
        {
            AppendLog($"[synthesize error] {ex.Message}");
            BuildListHint.Text = $"synthesize failed: {ex.Message}";
        }
        finally
        {
            SynthesizeFinalistsButton.Content = origLabel;
            SynthesizeFinalistsButton.IsEnabled = true;
        }
    }

    private void RenderFinalists(string payloadJson)
    {
        // Friendly path for the service returning plain-text errors (no
        // ANTHROPIC key, no Cerebras key in Mode B, network failure, etc.).
        // McpServiceClient's ExtractDisplayPayload returns "ERROR: <msg>"
        // for JSON-RPC errors — those start with 'E', not '{', so
        // JsonDocument.Parse throws and the user sees a useless parse error.
        // Surface the actual message instead.
        if (payloadJson.StartsWith("ERROR:", StringComparison.Ordinal)
            || payloadJson.StartsWith("(service not running", StringComparison.Ordinal))
        {
            var msg = payloadJson.Length > 240 ? payloadJson[..240] + "…" : payloadJson;
            AppendLog($"[synthesize] {msg}");
            Dispatcher.Invoke(() =>
            {
                BuildListHint.Text = payloadJson.Contains("not implemented", StringComparison.OrdinalIgnoreCase)
                    || payloadJson.Contains("DriverIsExternal", StringComparison.OrdinalIgnoreCase)
                    ? "Tiers 6+7 need MOSSRAVEN_ANTHROPIC_API_KEY in the service env (Mode A), or run from Claude Code (Mode B). Click Refresh to restore the archive view."
                    : $"synthesize failed: {msg}";
            });
            return;
        }
        try
        {
            using var doc = JsonDocument.Parse(payloadJson);
            var root = doc.RootElement;

            // Mode B response: { external: true, frontier: [...] }. Tell the
            // user the service is in Mode B and surface the raw frontier as
            // a fallback render.
            if (root.TryGetProperty("external", out var ext) && ext.ValueKind == JsonValueKind.True)
            {
                AppendLog("[synthesize] service is in Mode B — external Claude must curate");
                Dispatcher.Invoke(() =>
                {
                    BuildListHint.Text = "Mode B: set MOSSRAVEN_ANTHROPIC_API_KEY for in-app synthesis, or run from Claude Code.";
                });
                return;
            }

            if (!root.TryGetProperty("finalists", out var arr) || arr.ValueKind != JsonValueKind.Array)
            {
                AppendLog($"[synthesize] unexpected payload: {payloadJson.Substring(0, Math.Min(200, payloadJson.Length))}");
                Dispatcher.Invoke(() => BuildListHint.Text = "Claude returned no finalists field — see log");
                return;
            }

            var entries = new List<BuildEntry>();
            foreach (var f in arr.EnumerateArray())
            {
                string Str(string key) => f.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.String
                    ? v.GetString() ?? "" : "";

                var title = Str("title");
                var oneLiner = Str("one_liner");
                var why = Str("why_it_works");
                var cell = Str("cell");
                var importCode = Str("pob_import_code");

                // SPEC §1.1 guide — leveling / endgame / clear-boss swap prose.
                var guideLine = "";
                if (f.TryGetProperty("guide", out var g) && g.ValueKind == JsonValueKind.Object)
                {
                    string G(string key) => g.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.String
                        ? v.GetString() ?? "" : "";
                    var parts = new List<string>();
                    var lev = G("leveling");
                    if (lev.Length > 0) parts.Add("⮕ Leveling: " + lev);
                    var endg = G("endgame");
                    if (endg.Length > 0) parts.Add("⮕ Endgame: " + endg);
                    var swap = G("loadout_swap");
                    if (swap.Length > 0) parts.Add("⮕ Clear/boss swap: " + swap);
                    var notes = G("playtest_notes");
                    if (notes.Length > 0) parts.Add("⮕ Playtest: " + notes);
                    guideLine = string.Join("\n", parts);
                }

                var tags = new List<string>();
                if (f.TryGetProperty("tags", out var tarr) && tarr.ValueKind == JsonValueKind.Array)
                {
                    foreach (var t in tarr.EnumerateArray())
                    {
                        if (t.ValueKind == JsonValueKind.String) tags.Add(t.GetString() ?? "");
                    }
                }

                var keyStats = new List<string>();
                if (f.TryGetProperty("key_stats", out var ksarr) && ksarr.ValueKind == JsonValueKind.Array)
                {
                    foreach (var ks in ksarr.EnumerateArray())
                    {
                        var label = ks.TryGetProperty("label", out var l) ? l.GetString() : "";
                        var value = ks.TryGetProperty("value", out var v) ? v.GetString() : "";
                        keyStats.Add($"{value} {label}");
                    }
                }

                var headerBits = new List<string> { title };
                if (tags.Count > 0) headerBits.Add("[" + string.Join(" · ", tags) + "]");

                entries.Add(new BuildEntry
                {
                    HeaderLine = string.Join("  ", headerBits),
                    StatsLine = string.Join("    ", keyStats),
                    OriginLine = string.IsNullOrEmpty(why)
                        ? oneLiner
                        : $"{oneLiner} — {why}  ·  cell: {cell}",
                    GuideLine = guideLine,
                    // Finalists carry the import code directly from the
                    // service — DO NOT re-encode. EncodePobImportCode would
                    // double-compress a string that's already compressed.
                    PobImportCode = importCode,
                    PobXml = "", // not surfaced — UI only needs the import code for clipboard
                });
            }

            // Mode A persists finalists to disk and reports where.
            var savedTo = root.TryGetProperty("saved_to", out var st) && st.ValueKind == JsonValueKind.String
                ? st.GetString() ?? "" : "";

            Dispatcher.Invoke(() =>
            {
                T3CountText.Text = entries.Count.ToString();
                BuildList.ItemsSource = null;
                BuildList.ItemTemplate = BuildListTemplate();
                BuildList.ItemsSource = entries;
                BuildListHint.Text = entries.Count == 0
                    ? "Claude returned 0 finalists — try seeding more cells before synthesizing."
                    : string.IsNullOrEmpty(savedTo)
                        ? "Click a finalist to copy its PoB import code. Refresh restores the full archive view."
                        : $"Click a finalist to copy its PoB import code. Guides saved to {savedTo}";
            });
        }
        catch (Exception ex)
        {
            AppendLog($"[synthesize parse error] {ex.Message}");
            Dispatcher.Invoke(() => BuildListHint.Text = $"parse error: {ex.Message}");
        }
    }

    /// <summary>
    /// Watch %APPDATA%\Moss\MossRaven\data\archive.json. When ANY process
    /// (this WPF's child service OR a separate `--tool` invocation Claude
    /// runs from the shell) writes the archive, refresh the right pane.
    /// Both sides share state through the file.
    /// </summary>
    private void SetupArchiveWatcher()
    {
        try
        {
            var dir = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                "Moss", "MossRaven", "data");
            Directory.CreateDirectory(dir);
            _archiveWatcher = new FileSystemWatcher(dir, "archive.json")
            {
                NotifyFilter = NotifyFilters.LastWrite | NotifyFilters.Size | NotifyFilters.CreationTime,
            };
            _archiveWatcher.Changed += OnArchiveFileChanged;
            _archiveWatcher.Created += OnArchiveFileChanged;
            _archiveWatcher.EnableRaisingEvents = true;
            AppendLog($"[watcher] archive.json watched at {dir}");
        }
        catch (Exception ex)
        {
            AppendLog($"[watcher error] {ex.Message}");
        }
    }

    private async void OnArchiveFileChanged(object sender, FileSystemEventArgs e)
    {
        // FileSystemWatcher fires multiple times on atomic-rename writes.
        // Debounce + dedup: ignore if less than 500ms since last refresh.
        var now = DateTime.UtcNow;
        if ((now - _lastArchiveRefresh).TotalMilliseconds < 500) return;
        _lastArchiveRefresh = now;
        await System.Threading.Tasks.Task.Delay(300); // let the writer finish
        await RefreshArchiveAsync();
        Dispatcher.Invoke(() => AppendLog("[watcher] archive changed externally — refreshed right pane"));
    }

    private async System.Threading.Tasks.Task RefreshArchiveAsync()
    {
        try
        {
            var json = await _service.ReadArchiveAsync();
            RenderArchive(json);
            ScheduleEmptyArchiveRetry(json);
        }
        catch (Exception ex)
        {
            AppendLog($"[archive refresh error] {ex.Message}");
        }
    }

    private int _emptyRetryCount;

    /// <summary>
    /// Self-heal for the blank-pane incident (2026-06-11): a freshly spawned
    /// service can transiently fail to see archive.json (OS-level lock at
    /// session start) and report 0 cells while the file sits on disk. The
    /// service self-heals on its next read once the file is visible — so if
    /// we rendered EMPTY but the file exists, re-poll a few times with
    /// backoff instead of leaving the user staring at nothing.
    /// </summary>
    private void ScheduleEmptyArchiveRetry(string archiveJson)
    {
        bool empty;
        try
        {
            using var doc = JsonDocument.Parse(archiveJson);
            empty = !(doc.RootElement.TryGetProperty("cells_filled", out var cf) && cf.GetInt32() > 0);
        }
        catch
        {
            empty = true;
        }
        var dataDir = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
            "Moss", "MossRaven", "data");
        var archiveOnDisk = File.Exists(Path.Combine(dataDir, "archive.json"));
        if (!empty || !archiveOnDisk)
        {
            _emptyRetryCount = 0;
            return;
        }
        if (_emptyRetryCount >= 3)
        {
            AppendLog("[archive] still empty after retries — archive.json exists on disk but the service can't read it; check the service log");
            return;
        }
        var delay = TimeSpan.FromSeconds(5 * Math.Pow(3, _emptyRetryCount)); // 5s, 15s, 45s
        _emptyRetryCount++;
        AppendLog($"[archive] service reports 0 cells but archive.json exists — auto-retrying in {delay.TotalSeconds:N0}s ({_emptyRetryCount}/3)");
        _ = System.Threading.Tasks.Task.Run(async () =>
        {
            await System.Threading.Tasks.Task.Delay(delay);
            await Dispatcher.InvokeAsync(async () => await RefreshArchiveAsync());
        });
    }

    private void UpdateTierCounters()
    {
        Dispatcher.Invoke(() =>
        {
            T1HeaderCount.Text = $"n = {_t1Count}";
            T2HeaderCount.Text = $"n = {_t2Count}";
            T3HeaderCount.Text = $"n = {_t3Count}";
            T4HeaderCount.Text = $"n = {_t4Count}";
            T1Log.Text = _t1Lines.Count == 0 ? "(no hypotheses yet)" : string.Join('\n', _t1Lines);
            T2Log.Text = _t2Lines.Count == 0 ? "(no generations yet)" : string.Join('\n', _t2Lines);
            T3Log.Text = _t3Lines.Count == 0 ? "(no sims yet)" : string.Join('\n', _t3Lines);
            T4Log.Text = _t4Lines.Count == 0
                ? "(no prunings yet — legality gates + interest pruning report here)"
                : string.Join('\n', _t4Lines);
        });
    }

    private static void AppendCapped(System.Collections.Generic.List<string> list, string line, int cap)
    {
        list.Add(line);
        if (list.Count > cap) list.RemoveRange(0, list.Count - cap);
    }

    /// <summary>
    /// A single archive entry surfaced to the UI. ListBoxItem renders
    /// HeaderLine + StatsLine + OriginLine; clicking copies <see cref="PobImportCode"/>.
    /// </summary>
    public sealed class BuildEntry
    {
        public string HeaderLine { get; set; } = "";
        public string StatsLine { get; set; } = "";
        public string OriginLine { get; set; } = "";
        /// <summary>SPEC §1.1 guide text (leveling / endgame / loadout swap). Empty for plain archive rows.</summary>
        public string GuideLine { get; set; } = "";
        public string PobImportCode { get; set; } = "";
        public string PobXml { get; set; } = "";
    }

    private void RenderArchive(string archiveJson)
    {
        try
        {
            using var doc = JsonDocument.Parse(archiveJson);
            var root = doc.RootElement;
            int filled = root.TryGetProperty("cells_filled", out var cf) ? cf.GetInt32() : 0;

            var entries = new List<BuildEntry>();
            if (root.TryGetProperty("entries", out var arr) && arr.ValueKind == JsonValueKind.Array)
            {
                foreach (var entry in arr.EnumerateArray())
                {
                    var coords = entry.GetProperty("coords");
                    var stats = entry.GetProperty("stats");

                    string Num(string key, string fmt = "N0") =>
                        stats.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.Number
                            ? v.GetDouble().ToString(fmt)
                            : "—";

                    var damage = coords.GetProperty("damage_type").GetString() ?? "?";
                    var defense = coords.GetProperty("defense_layer").GetString() ?? "?";
                    var role = coords.GetProperty("role").GetString() ?? "?";
                    var scaling = coords.GetProperty("scaling_vector").GetString() ?? "?";

                    var origin = entry.TryGetProperty("origin_hypothesis", out var o) && o.ValueKind == JsonValueKind.String
                        ? o.GetString() ?? ""
                        : "";
                    var variant = entry.TryGetProperty("variant_id", out var vid) && vid.ValueKind == JsonValueKind.String
                        ? vid.GetString() ?? ""
                        : "";
                    var pobXml = entry.TryGetProperty("pob_xml", out var px) && px.ValueKind == JsonValueKind.String
                        ? px.GetString() ?? ""
                        : "";

                    entries.Add(new BuildEntry
                    {
                        HeaderLine = $"[{damage} · {defense} · {role} · {scaling}]",
                        StatsLine = $"{Num("dps")} DPS    {Num("life")} life    {Num("energy_shield")} ES    {Num("effective_hp")} EHP",
                        OriginLine = string.IsNullOrEmpty(origin) ? variant : $"{origin}  ·  {variant}",
                        PobXml = pobXml,
                        PobImportCode = string.IsNullOrEmpty(pobXml) ? "" : EncodePobImportCode(pobXml),
                    });
                }
            }

            Dispatcher.Invoke(() =>
            {
                T3CountText.Text = filled.ToString();
                BuildList.ItemsSource = null;
                BuildList.ItemTemplate = BuildListTemplate();
                BuildList.ItemsSource = entries;
                if (entries.Count == 0)
                {
                    BuildListHint.Text = "No builds yet — click Seed then Run.";
                }
                else
                {
                    BuildListHint.Text = "Click a build to copy its PoB import code to clipboard.";
                }
            });
        }
        catch (Exception ex)
        {
            Dispatcher.Invoke(() =>
            {
                BuildListHint.Text = $"parse error: {ex.Message}";
            });
        }
    }

    /// <summary>
    /// Build a DataTemplate that renders each BuildEntry as three lines of
    /// text (header / stats / origin). Defined in C# instead of XAML so we
    /// can wire the MouseLeftButtonUp event without binding gymnastics.
    /// </summary>
    private DataTemplate BuildListTemplate()
    {
        var template = new DataTemplate(typeof(BuildEntry));
        var sp = new FrameworkElementFactory(typeof(StackPanel));
        sp.SetValue(StackPanel.OrientationProperty, Orientation.Vertical);

        var header = new FrameworkElementFactory(typeof(TextBlock));
        header.SetBinding(TextBlock.TextProperty, new System.Windows.Data.Binding(nameof(BuildEntry.HeaderLine)));
        header.SetResourceReference(TextBlock.ForegroundProperty, "TextBrush");
        header.SetValue(TextBlock.FontWeightProperty, FontWeights.SemiBold);
        header.SetValue(TextBlock.FontSizeProperty, 13.0);
        sp.AppendChild(header);

        var stats = new FrameworkElementFactory(typeof(TextBlock));
        stats.SetBinding(TextBlock.TextProperty, new System.Windows.Data.Binding(nameof(BuildEntry.StatsLine)));
        stats.SetResourceReference(TextBlock.ForegroundProperty, "DimBrush");
        stats.SetValue(TextBlock.FontFamilyProperty, new FontFamily("Cascadia Mono, Consolas"));
        stats.SetValue(TextBlock.FontSizeProperty, 11.5);
        stats.SetValue(TextBlock.MarginProperty, new Thickness(0, 3, 0, 0));
        sp.AppendChild(stats);

        var origin = new FrameworkElementFactory(typeof(TextBlock));
        origin.SetBinding(TextBlock.TextProperty, new System.Windows.Data.Binding(nameof(BuildEntry.OriginLine)));
        origin.SetResourceReference(TextBlock.ForegroundProperty, "DimmerBrush");
        origin.SetValue(TextBlock.FontSizeProperty, 10.5);
        origin.SetValue(TextBlock.MarginProperty, new Thickness(0, 3, 0, 0));
        origin.SetValue(TextBlock.TextWrappingProperty, TextWrapping.Wrap);
        sp.AppendChild(origin);

        // SPEC §1.1 guide block — only visible on finalist entries (empty
        // string collapses to zero height on archive rows).
        var guide = new FrameworkElementFactory(typeof(TextBlock));
        guide.SetBinding(TextBlock.TextProperty, new System.Windows.Data.Binding(nameof(BuildEntry.GuideLine)));
        guide.SetResourceReference(TextBlock.ForegroundProperty, "DimBrush");
        guide.SetValue(TextBlock.FontSizeProperty, 11.0);
        guide.SetValue(TextBlock.MarginProperty, new Thickness(0, 4, 0, 0));
        guide.SetValue(TextBlock.TextWrappingProperty, TextWrapping.Wrap);
        sp.AppendChild(guide);

        template.VisualTree = sp;
        return template;
    }


    /// <summary>
    /// Wired up programmatically because the auto-named handler from XAML
    /// isn't a thing when we set ItemsSource. Hook in code-behind on the
    /// ListBox's SelectionChanged.
    /// </summary>
    private bool _buildListWired;
    private void EnsureBuildListWiredOnce()
    {
        if (_buildListWired) return;
        BuildList.SelectionChanged += (_, _) =>
        {
            if (BuildList.SelectedItem is BuildEntry entry)
            {
                LoadIntoWorkspace(entry);
            }
        };
        _buildListWired = true;
    }

    private void CopyBuildToClipboard(BuildEntry entry)
    {
        if (string.IsNullOrEmpty(entry.PobImportCode))
        {
            AppendLog("[copy] no PoB XML on this entry");
            BuildListHint.Text = "no PoB XML on this entry";
            return;
        }
        try
        {
            Clipboard.SetText(entry.PobImportCode);
            var snippet = entry.PobImportCode.Length > 80
                ? entry.PobImportCode[..80] + "…"
                : entry.PobImportCode;
            AppendLog($"[copy] PoB code copied ({entry.PobImportCode.Length} chars): {snippet}");
            BuildListHint.Text = $"✓ copied {entry.PobImportCode.Length}-char PoB code — paste into desktop PoB2 → File → Import/Export Build → Import";
        }
        catch (Exception ex)
        {
            AppendLog($"[copy error] {ex.Message}");
            BuildListHint.Text = $"copy failed: {ex.Message}";
        }
    }

    /// <summary>
    /// Encode a PoB XML string to a PoB import code:
    /// utf8(xml) -> zlib deflate -> URL-safe base64.
    /// Decoder is symmetric (see scripts/pull-pob-fixtures.py for the
    /// Python equivalent we use to pull seeds from pobb.in).
    /// </summary>
    public static string EncodePobImportCode(string xml)
    {
        var raw = Encoding.UTF8.GetBytes(xml);
        using var ms = new MemoryStream();
        using (var zs = new ZLibStream(ms, CompressionLevel.Optimal, leaveOpen: true))
        {
            zs.Write(raw, 0, raw.Length);
        }
        var b64 = Convert.ToBase64String(ms.ToArray());
        return b64.Replace('+', '-').Replace('/', '_');
    }

    private ConceptHistoryWindow? _historyWin;

    /// <summary>
    /// Open model settings; on save, restart the engine child so the new
    /// provider keys / tier assignment apply (env is read at spawn).
    /// </summary>
    private async void ModelSettingsButton_Click(object sender, RoutedEventArgs e)
    {
        var win = new ModelSettingsWindow(_settings) { Owner = this, ShowInTaskbar = false };
        win.ShowDialog();
        if (!win.Saved) return;
        AppendLog("[settings] saved — reconnecting engine with new provider environment");
        try
        {
            _service.Dispose();
        }
        catch { /* old child may already be gone */ }
        _service = new McpServiceClient(
            LocateServiceExe(),
            AppendLog,
            () => _settings.ToServiceEnvironment());
        await ConnectServiceAsync();
        await RefreshArchiveAsync();
    }

    private void HistoryButton_Click(object sender, RoutedEventArgs e)
    {
        // Toggle behavior: clicking the button while history is open closes it.
        if (_historyWin != null)
        {
            _historyWin.Close();
            _historyWin = null;
            return;
        }
        _historyWin = new ConceptHistoryWindow(_settings.History)
        {
            Owner = this,
            ShowInTaskbar = false,
        };
        _historyWin.ConceptPicked += (_, picked) =>
        {
            ConceptInput.Text = picked;
            ConceptInput.Focus();
            ConceptInput.SelectAll();
        };
        _historyWin.Closed += (_, _) => _historyWin = null;
        // Non-modal so clicks on the main window trigger the dialog's Deactivated
        // event (which auto-closes it). ShowDialog would block the main window
        // and you couldn't click back to dismiss.
        _historyWin.Show();
    }

    private void AppendLog(string line)
    {
        try
        {
            File.AppendAllText(_debugLogPath, $"{DateTime.Now:HH:mm:ss.fff}  {line}{Environment.NewLine}");
        }
        catch { /* best-effort */ }

        Dispatcher.Invoke(() =>
        {
            ServiceLog.AppendText(line + Environment.NewLine);
            ServiceLog.ScrollToEnd();
            EnsureBuildListWiredOnce();
        });
    }

    // ----- Titlebar buttons -----

    private void BtnMin_Click(object sender, RoutedEventArgs e) => WindowState = WindowState.Minimized;

    private void BtnMax_Click(object sender, RoutedEventArgs e) =>
        WindowState = WindowState == WindowState.Maximized
            ? WindowState.Normal
            : WindowState.Maximized;

    private void BtnClose_Click(object sender, RoutedEventArgs e) => Close();

    private void MainWindow_StateChanged(object? sender, EventArgs e)
    {
        if (WindowState == WindowState.Maximized)
        {
            RootBorder.Margin = new Thickness(8);
            RootBorder.CornerRadius = new CornerRadius(0);
        }
        else
        {
            RootBorder.Margin = new Thickness(0);
            RootBorder.CornerRadius = new CornerRadius(8);
        }
    }
}
