using System;
using System.IO;
using System.Windows;
using MossRaven.Services;

namespace MossRaven;

public partial class MainWindow : Window
{
    private readonly McpServiceClient _service;
    private Settings _settings;
    private readonly string _debugLogPath;

    public MainWindow()
    {
        InitializeComponent();
        _settings = SettingsService.Load();
        // Append-only debug log next to %TEMP%/mossraven-ui.log — written to on
        // every AppendLog() call so we have a forensic trail to read back after
        // the user reports a bug. Truncated to 1 MB on each launch.
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
        };
        StateChanged += MainWindow_StateChanged;
        Closing += (_, _) => SaveStateBeforeClose();
        Closed += (_, _) => _service.Dispose();
    }

    private void ApplyPersistedState()
    {
        // Restore the last concept the user typed, unless we already have
        // placeholder text from the XAML (we don't — XAML left ConceptInput empty).
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
        // In published builds: same install dir as MossRaven.exe.
        // In dev: copy target/release/mossraven-service.exe alongside MossRaven.dll.
        var here = AppContext.BaseDirectory;
        return Path.Combine(here, "mossraven-service.exe");
    }

    private async System.Threading.Tasks.Task ConnectServiceAsync()
    {
        try
        {
            await _service.StartAsync();
            ServiceStateText.Text = "service: connected (stub)";
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

        // Record in history (most-recent-first, deduped, capped). Save immediately
        // so a crash doesn't lose the user's latest seed.
        SettingsService.AppendHistory(_settings, concept);
        _settings.LastConcept = concept;
        SettingsService.Save(_settings);

        AppendLog($"[seed] {concept}");
        try
        {
            var result = await _service.SeedHypothesisAsync(concept);
            AppendLog($"[seed reply] {result}");
        }
        catch (Exception ex)
        {
            AppendLog($"[seed error] {ex.Message}");
        }
    }

    private async void RunButton_Click(object sender, RoutedEventArgs e)
    {
        AppendLog("[run] 10 generations");
        try
        {
            var result = await _service.RunSearchAsync(generations: 10, region: null);
            AppendLog($"[run reply] {result}");
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

    private void RenderArchive(string archiveJson)
    {
        try
        {
            using var doc = System.Text.Json.JsonDocument.Parse(archiveJson);
            var root = doc.RootElement;
            int filled = root.TryGetProperty("cells_filled", out var cf) ? cf.GetInt32() : 0;
            var sb = new System.Text.StringBuilder();
            sb.AppendLine($"{filled} cell(s) filled");
            sb.AppendLine(new string('-', 60));
            if (root.TryGetProperty("entries", out var entries) && entries.ValueKind == System.Text.Json.JsonValueKind.Array)
            {
                foreach (var entry in entries.EnumerateArray())
                {
                    var coords = entry.GetProperty("coords");
                    var stats = entry.GetProperty("stats");
                    string fmt(string key) => stats.TryGetProperty(key, out var v) && v.ValueKind == System.Text.Json.JsonValueKind.Number
                        ? v.GetDouble().ToString("N0")
                        : "—";
                    sb.AppendLine();
                    sb.AppendLine($"[{coords.GetProperty("damage_type").GetString()} / {coords.GetProperty("defense_layer").GetString()} / {coords.GetProperty("role").GetString()} / {coords.GetProperty("scaling_vector").GetString()}]");
                    sb.AppendLine($"  DPS:  {fmt("dps"),12}    EHP:  {fmt("effective_hp"),12}");
                    sb.AppendLine($"  Life: {fmt("life"),12}    ES:   {fmt("energy_shield"),12}");
                    if (entry.TryGetProperty("origin_hypothesis", out var origin) && origin.ValueKind == System.Text.Json.JsonValueKind.String)
                    {
                        sb.AppendLine($"  Origin: {origin.GetString()}");
                    }
                    if (entry.TryGetProperty("variant_id", out var vid) && vid.ValueKind == System.Text.Json.JsonValueKind.String)
                    {
                        sb.AppendLine($"  Variant: {vid.GetString()}");
                    }
                }
            }
            Dispatcher.Invoke(() =>
            {
                ArchiveView.Text = sb.ToString();
                ArchiveSummary.Text = $"{filled} cells";
            });
        }
        catch (Exception ex)
        {
            Dispatcher.Invoke(() =>
            {
                ArchiveView.Text = $"parse error: {ex.Message}\n\nRAW:\n{archiveJson}";
            });
        }
    }

    private void HistoryButton_Click(object sender, RoutedEventArgs e)
    {
        var win = new ConceptHistoryWindow(_settings.History)
        {
            Owner = this,
        };
        if (win.ShowDialog() == true && win.Selected is { Length: > 0 } picked)
        {
            ConceptInput.Text = picked;
            ConceptInput.Focus();
            ConceptInput.SelectAll();
        }
    }

    private void AppendLog(string line)
    {
        // Tee to forensic log file (so I can read it back when you say "broken").
        try
        {
            File.AppendAllText(_debugLogPath, $"{DateTime.Now:HH:mm:ss.fff}  {line}{Environment.NewLine}");
        }
        catch { /* best-effort */ }

        Dispatcher.Invoke(() =>
        {
            ServiceLog.AppendText(line + Environment.NewLine);
            ServiceLog.ScrollToEnd();
        });
    }

    // ----- Titlebar buttons -----

    private void BtnMin_Click(object sender, RoutedEventArgs e) => WindowState = WindowState.Minimized;

    private void BtnMax_Click(object sender, RoutedEventArgs e) =>
        WindowState = WindowState == WindowState.Maximized
            ? WindowState.Normal
            : WindowState.Maximized;

    private void BtnClose_Click(object sender, RoutedEventArgs e) => Close();

    // When maximized, compensate margin + flatten corners so the rounded
    // RootBorder doesn't butt against the monitor edge. Matches MossNote pattern.
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
