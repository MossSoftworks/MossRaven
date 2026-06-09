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
    private readonly McpServiceClient _service;
    private Settings _settings;
    private readonly string _debugLogPath;

    private FileSystemWatcher? _archiveWatcher;
    private DateTime _lastArchiveRefresh = DateTime.MinValue;

    // Per-tier counters (matches the 5-tier UI model).
    private int _t1Count;   // hypotheses (Seed clicks)
    private int _t2Count;   // mutations proposed (sum of variants_proposed)
    private int _t3Count;   // PoB sims (sum of variants_scored)
    private int _t4Count;   // pruned (sum of variants_pruned — currently pre-sim)
    // Tier-5 count is the live archive cell count, read directly from the service.

    // Rolling per-tier iteration logs — kept short, last N lines only.
    private readonly System.Collections.Generic.List<string> _t1Lines = new();
    private readonly System.Collections.Generic.List<string> _t2Lines = new();
    private readonly System.Collections.Generic.List<string> _t3Lines = new();
    private readonly System.Collections.Generic.List<string> _t4Lines = new();
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

        _service = new McpServiceClient(LocateServiceExe(), AppendLog);
        Loaded += async (_, _) =>
        {
            ApplyPersistedState();
            await ConnectServiceAsync();
            await RefreshArchiveAsync();
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
        await RefreshArchiveAsync();
    }

    /// <summary>
    /// Tier 5 — ask the service to synthesize finalists from the current
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
                    // Finalists carry the import code directly from the
                    // service — DO NOT re-encode. EncodePobImportCode would
                    // double-compress a string that's already compressed.
                    PobImportCode = importCode,
                    PobXml = "", // not surfaced — UI only needs the import code for clipboard
                });
            }

            Dispatcher.Invoke(() =>
            {
                T3CountText.Text = entries.Count.ToString();
                BuildList.ItemsSource = null;
                BuildList.ItemTemplate = BuildListTemplate();
                BuildList.ItemsSource = entries;
                BuildListHint.Text = entries.Count == 0
                    ? "Claude returned 0 finalists — try seeding more cells before synthesizing."
                    : "Click a finalist to copy its PoB import code. Refresh restores the full archive view.";
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
        }
        catch (Exception ex)
        {
            AppendLog($"[archive refresh error] {ex.Message}");
        }
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
                ? "(no prunings yet — currently pre-sim pruning only; smart post-sim pruning is next session)"
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
        sp.AppendChild(origin);

        template.VisualTree = sp;
        return template;
    }

    private void BuildList_PreviewMouseLeftButtonUp(object sender, MouseButtonEventArgs e)
    {
        // Selection-and-paste behavior: click an item, its PoB import code goes to clipboard.
        if (sender is ListBox lb && lb.SelectedItem is BuildEntry entry)
        {
            CopyBuildToClipboard(entry);
        }
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
        BuildList.MouseLeftButtonUp += BuildList_PreviewMouseLeftButtonUp;
        BuildList.SelectionChanged += (_, _) =>
        {
            if (BuildList.SelectedItem is BuildEntry entry)
            {
                CopyBuildToClipboard(entry);
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
