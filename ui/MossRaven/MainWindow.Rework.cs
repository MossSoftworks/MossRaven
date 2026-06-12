using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Threading;

namespace MossRaven;

/// <summary>
/// 2026-06-11 UI rework: Build Workspace, Retool, preference autofill,
/// Ops box, settings gear. Partial — MainWindow.xaml.cs keeps the original
/// archive/history/tier plumbing.
/// </summary>
public partial class MainWindow
{
    // ----- shared rework state -----
    private string _wsCode = "";
    private System.Diagnostics.Process? _churnProc;
    private DispatcherTimer? _opsTimer;
    private readonly List<ComboBox> _prefUniques = new();
    private readonly List<ComboBox> _prefSkills = new();
    private readonly List<ComboBox> _prefNodes = new();

    private static string RepoRootDir()
    {
        // dist\MossRaven.exe → repo root is dist's parent. Falls back to the
        // exe dir itself (installed layouts keep scripts beside the exe).
        var exeDir = AppContext.BaseDirectory.TrimEnd('\\', '/');
        var parent = Path.GetDirectoryName(exeDir);
        if (parent != null && Directory.Exists(Path.Combine(parent, "scripts")))
            return parent;
        return exeDir;
    }

    private static string DataDir() => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
        "Moss", "MossRaven", "data");

    private DispatcherTimer? _schedTimer;
    private string _lastSchedMinute = "";

    private void InitRework()
    {
        BuildPrefCombos();
        _opsTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(10) };
        _opsTimer.Tick += (_, _) => RefreshOpsStatus();
        _opsTimer.Start();
        RefreshOpsStatus();

        // Ops scheduling: HH:mm fields, checked once a minute.
        ChurnStartTime.Text = _settings.ChurnStartAt;
        ChurnStopTime.Text = _settings.ChurnStopAt;
        RescoreAtTime.Text = _settings.RescoreAt;
        TrainAtTime.Text = _settings.TrainAt;
        foreach (var tb in new[] { ChurnStartTime, ChurnStopTime, RescoreAtTime, TrainAtTime })
            tb.LostFocus += (_, _) => SaveSchedule();
        _schedTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(20) };
        _schedTimer.Tick += (_, _) => TickSchedule();
        _schedTimer.Start();
    }

    private void SaveSchedule()
    {
        _settings.ChurnStartAt = ChurnStartTime.Text?.Trim() ?? "";
        _settings.ChurnStopAt = ChurnStopTime.Text?.Trim() ?? "";
        _settings.RescoreAt = RescoreAtTime.Text?.Trim() ?? "";
        _settings.TrainAt = TrainAtTime.Text?.Trim() ?? "";
        Services.SettingsService.Save(_settings);
    }

    private void TickSchedule()
    {
        var now = DateTime.Now.ToString("HH:mm");
        if (now == _lastSchedMinute) return; // fire each matching minute once
        _lastSchedMinute = now;
        bool churnAlive = _churnProc is { HasExited: false };
        if (now == _settings.ChurnStartAt && !churnAlive)
        {
            AppendLog($"[ops] scheduled churn start ({now})");
            OpsChurnButton_Click(this, new RoutedEventArgs());
        }
        if (now == _settings.ChurnStopAt && churnAlive)
        {
            AppendLog($"[ops] scheduled churn stop ({now})");
            OpsChurnButton_Click(this, new RoutedEventArgs());
        }
        if (now == _settings.RescoreAt)
        {
            AppendLog($"[ops] scheduled rescore ({now})");
            OpsRescoreButton_Click(this, new RoutedEventArgs());
        }
        if (now == _settings.TrainAt)
        {
            AppendLog($"[ops] scheduled training ({now})");
            OpsTrainButton_Click(this, new RoutedEventArgs());
        }
    }

    // ----- Settings gear (titlebar) -----
    private void SettingsButton_Click(object sender, RoutedEventArgs e)
        => ModelSettingsButton_Click(sender, e);

    // ----- Preferences (uniques / skills / nodes autofill) -----
    private void BuildPrefCombos()
    {
        var style = (Style)FindResource("PrefCombo");
        for (int i = 0; i < 3; i++)
        {
            var u = new ComboBox { Style = style, Margin = new Thickness(0, 0, 4, 4) };
            var s = new ComboBox { Style = style, Margin = new Thickness(0, 0, 4, 4) };
            var n = new ComboBox { Style = style, Margin = new Thickness(0, 0, 4, 4) };
            _prefUniques.Add(u);
            _prefSkills.Add(s);
            _prefNodes.Add(n);
            PrefUniquesPanel.Children.Add(u);
            PrefSkillsPanel.Children.Add(s);
            PrefNodesPanel.Children.Add(n);
        }
    }

    public async System.Threading.Tasks.Task LoadVocabAsync(bool retry = true)
    {
        try
        {
            var json = await _service.GetVocabAsync();
            using var doc = JsonDocument.Parse(json);
            List<string> Arr(string key) =>
                doc.RootElement.TryGetProperty(key, out var a) && a.ValueKind == JsonValueKind.Array
                    ? a.EnumerateArray().Select(x => x.GetString() ?? "").Where(x => x.Length > 0).ToList()
                    : new List<string>();
            var uniques = Arr("uniques");
            var skills = Arr("skills");
            var nodes = Arr("notables");
            foreach (var c in _prefUniques) c.ItemsSource = uniques;
            foreach (var c in _prefSkills) c.ItemsSource = skills;
            foreach (var c in _prefNodes) c.ItemsSource = nodes;
            AppendLog($"[vocab] autofill loaded: {uniques.Count} uniques, {skills.Count} skills, {nodes.Count} notables");
        }
        catch (Exception ex)
        {
            if (retry)
            {
                await System.Threading.Tasks.Task.Delay(3000);
                await LoadVocabAsync(retry: false);
                return;
            }
            AppendLog($"[vocab] autofill unavailable: {ex.Message}");
        }
    }

    /// <summary>Preference lines appended to the concept on Seed.</summary>
    private string PreferenceSuffix()
    {
        static IEnumerable<string> Picked(IEnumerable<ComboBox> combos) =>
            combos.Select(c => (c.Text ?? "").Trim()).Where(t => t.Length > 0).Distinct();
        var u = string.Join(", ", Picked(_prefUniques));
        var s = string.Join(", ", Picked(_prefSkills));
        var n = string.Join(", ", Picked(_prefNodes));
        if (u.Length + s.Length + n.Length == 0) return "";
        var parts = new List<string>();
        if (u.Length > 0) parts.Add($"uniques: {u}");
        if (s.Length > 0) parts.Add($"skills: {s}");
        if (n.Length > 0) parts.Add($"tree notables: {n}");
        return $"\n[PLAYER PREFERENCES — strongly prefer builds using {string.Join("; ", parts)}]";
    }

    // ----- Retool -----
    private async void RetoolButton_Click(object sender, RoutedEventArgs e)
    {
        var code = RetoolCodeInput.Text?.Trim() ?? "";
        if (code.Length < 20)
        {
            AppendLog("[retool] paste a PoB2 import code first");
            return;
        }
        var mode = RetoolBossing.IsChecked == true ? "bossing"
            : RetoolMapping.IsChecked == true ? "mapping"
            : RetoolLeveling.IsChecked == true ? "leveling"
            : "both";
        RetoolButton.IsEnabled = false;
        RetoolButton.Content = "Retooling…";
        AppendLog($"[retool] mode={mode} — seeding from pasted build, searching, writing guides (takes a few minutes)");
        try
        {
            var result = await _service.RetoolBuildAsync(code, mode);
            AppendLog($"[retool] done — see History for the new run. {Snippet(result, 200)}");
            await RefreshArchiveAsync();
        }
        catch (Exception ex)
        {
            AppendLog($"[retool error] {ex.Message}");
        }
        finally
        {
            RetoolButton.IsEnabled = true;
            RetoolButton.Content = "Retool";
        }
    }

    private static string Snippet(string s, int max) =>
        s.Length <= max ? s : s.Substring(0, max) + "…";

    // ----- Embedded PoB2 -----
    private Services.PobEmbedHost? _pobHost;

    private bool _pobBootstrapping;

    private async void EnsurePobEmbedded()
    {
        if (_pobHost is { IsAlive: true }) return;
        var pob = _settings.PobInstallPath ?? "";
        if (pob.Length == 0 || !File.Exists(pob))
        {
            // Auto-discover a previous bootstrap, else download the official
            // portable release once (~365 MB) — "PoB packaged in the app"
            // without redistributing GGG data.
            pob = Services.PobBootstrap.FindExistingExe() ?? "";
            if (pob.Length == 0)
            {
                if (_pobBootstrapping) return;
                _pobBootstrapping = true;
                PobHostHint.Text = "Downloading the official PoB2 portable release (~365 MB, one time)…\nProgress in Service status below.";
                try
                {
                    pob = await Services.PobBootstrap.EnsureRuntimeAsync(AppendLog) ?? "";
                }
                finally
                {
                    _pobBootstrapping = false;
                }
                if (pob.Length == 0)
                {
                    PobHostHint.Text = "PoB2 download failed — see Service status. You can also install PoB2 yourself and set its path in Settings (gear).";
                    return;
                }
            }
            _settings.PobInstallPath = pob;
            Services.SettingsService.Save(_settings);
            AppendLog($"[pob-embed] using {pob}");
        }
        Services.PobBootstrap.StabilizePob(pob, AppendLog);
        Services.PobBootstrap.EnsureLiveLink(pob, AppendLog);
        try
        {
            _pobHost = new Services.PobEmbedHost(pob, AppendLog);
            PobHostSlot.Content = _pobHost;
            PobHostHint.Visibility = Visibility.Collapsed;
        }
        catch (Exception ex)
        {
            AppendLog($"[pob-embed] failed: {ex.Message}");
            PobHostHint.Visibility = Visibility.Visible;
        }
    }

    /// <summary>Load the build into the LIVE embedded PoB window — view only,
    /// nothing written to PoB's Builds list. Saving is the user's explicit
    /// choice (Save in PoB, or Save in Build tools).</summary>
    private void PushBuildToPob(string xml, string title)
    {
        try
        {
            EnsurePobEmbedded();
            var exe = _settings.PobInstallPath ?? "";
            if (exe.Length > 0 && File.Exists(exe))
            {
                Services.PobBootstrap.PushLive(exe, xml, AppendLog);
                AppendLog($"[pob] '{title}' loaded into the live PoB window (not saved — Save keeps it)");
            }
        }
        catch (Exception ex)
        {
            AppendLog($"[pob] push failed: {ex.Message}");
        }
    }

    // ----- Build Workspace -----
    private void LoadIntoWorkspace(BuildEntry entry)
    {
        _wsCode = entry.PobImportCode ?? "";
        WsTitle.Text = string.IsNullOrEmpty(entry.HeaderLine) ? "Build" : entry.HeaderLine;
        WsStats.Text = entry.StatsLine ?? "";
        WsViability.Text = entry.GuideLine is { Length: > 0 } g ? g : entry.OriginLine ?? "";
        WsXml.Text = string.IsNullOrEmpty(entry.PobXml)
            ? "(no XML on this entry — paste or re-score from code)"
            : entry.PobXml;
        if (!string.IsNullOrEmpty(entry.PobXml))
            PushBuildToPob(entry.PobXml, entry.HeaderLine ?? "build");
        BuildListHint.Text = "Loaded → PoB2 pane (and Build tools below it)";
    }

    private async void WsRescoreButton_Click(object sender, RoutedEventArgs e)
    {
        var xml = WsXml.Text ?? "";
        if (xml.Length < 100 || !xml.Contains("<Build"))
        {
            AppendLog("[workspace] no build XML to score");
            return;
        }
        WsRescoreButton.IsEnabled = false;
        try
        {
            var json = await _service.ScoreXmlAsync(xml);
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;
            if (root.TryGetProperty("stats", out var st))
            {
                double D(string k) => st.TryGetProperty(k, out var v) && v.ValueKind == JsonValueKind.Number ? v.GetDouble() : 0;
                WsStats.Text =
                    $"DPS {D("dps"):N0}   ·   EHP {D("effective_hp"):N0}   ·   ES {D("energy_shield"):N0}   ·   " +
                    $"res {st.GetProperty("fire_res").GetInt32()}/{st.GetProperty("cold_res").GetInt32()}/{st.GetProperty("lightning_res").GetInt32()}   ·   " +
                    $"points {D("points_used"):N0}/{D("points_budget"):N0}";
            }
            if (root.TryGetProperty("viability", out var vi))
            {
                var pass = vi.TryGetProperty("pass", out var p) && p.GetBoolean();
                var band = vi.TryGetProperty("dps_band", out var b) ? b.GetString() : "";
                var cost = root.TryGetProperty("cost", out var c) && c.TryGetProperty("band", out var cb) ? cb.GetString() : "";
                WsViability.Text = $"viability: {(pass ? "PASS" : "FAIL")} — {band}   ·   cost: {cost}";
            }
            if (root.TryGetProperty("pob_import_code", out var code))
                _wsCode = code.GetString() ?? _wsCode;
            AppendLog("[workspace] re-scored through the PoB judge");
        }
        catch (Exception ex)
        {
            AppendLog($"[workspace] re-score failed: {ex.Message}");
        }
        finally
        {
            WsRescoreButton.IsEnabled = true;
        }
    }

    private void WsSaveButton_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var dir = Path.Combine(DataDir(), "workspace");
            Directory.CreateDirectory(dir);
            var stem = $"build-{DateTime.Now:yyyyMMdd-HHmmss}";
            File.WriteAllText(Path.Combine(dir, stem + ".xml"), WsXml.Text);
            if (_wsCode.Length > 0)
                File.WriteAllText(Path.Combine(dir, stem + ".pob-code.txt"), _wsCode);
            AppendLog($"[workspace] saved {stem}.xml to {dir}");
            BuildListHint.Text = $"Saved {stem}.xml";
        }
        catch (Exception ex)
        {
            AppendLog($"[workspace] save failed: {ex.Message}");
        }
    }

    private void WsCopyCodeButton_Click(object sender, RoutedEventArgs e)
    {
        if (_wsCode.Length == 0)
        {
            AppendLog("[workspace] no import code — Re-score first to derive one from the XML");
            return;
        }
        try
        {
            Clipboard.SetText(_wsCode);
            BuildListHint.Text = "Import code copied.";
        }
        catch (Exception ex)
        {
            AppendLog($"[workspace] clipboard failed: {ex.Message}");
        }
    }

    private void WsOpenPobButton_Click(object sender, RoutedEventArgs e)
    {
        var pob = _settings.PobInstallPath ?? "";
        if (pob.Length == 0 || !File.Exists(pob))
        {
            AppendLog("[workspace] set the PoB2 executable path in Settings (gear icon) first");
            return;
        }
        try
        {
            var buildsDir = Path.Combine(
                Path.GetDirectoryName(pob) ?? ".", "Builds");
            if (!Directory.Exists(buildsDir))
                buildsDir = Path.Combine(
                    Environment.GetFolderPath(Environment.SpecialFolder.MyDocuments),
                    "Path of Building", "Builds");
            Directory.CreateDirectory(buildsDir);
            var file = Path.Combine(buildsDir, $"MossRaven-{DateTime.Now:HHmmss}.xml");
            File.WriteAllText(file, WsXml.Text);
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
            {
                FileName = pob,
                UseShellExecute = true,
            });
            AppendLog($"[workspace] wrote {Path.GetFileName(file)} into PoB2's Builds folder and launched PoB2 — open it from Builds");
        }
        catch (Exception ex)
        {
            AppendLog($"[workspace] open in PoB2 failed: {ex.Message}");
        }
    }

    // ----- Ops box -----
    private long _opsRows, _opsBytes;

    private async void RefreshOpsStatus()
    {
        // Prefer the ENGINE's view (this UI process has been observed unable
        // to enumerate the data dir in some launch contexts).
        try
        {
            var json = await _service.OpsStatusAsync();
            using var doc = JsonDocument.Parse(json);
            if (doc.RootElement.TryGetProperty("corpus_rows", out var r))
                _opsRows = r.GetInt64();
            if (doc.RootElement.TryGetProperty("corpus_bytes", out var b))
                _opsBytes = b.GetInt64();
        }
        catch { /* keep last known; fall through to local estimate */ }
        try
        {
            var corpusDir = Path.Combine(DataDir(), "corpus");
            long bytes = _opsBytes;
            if (bytes == 0 && Directory.Exists(corpusDir))
                bytes = new DirectoryInfo(corpusDir).GetFiles("evals-*.jsonl").Sum(f => f.Length);
            var rows = _opsRows > 0 ? _opsRows : bytes / 2000;
            var churnAlive = _churnProc is { HasExited: false };
            OpsChurnStatus.Text = $"{(churnAlive ? "RUNNING" : "idle")} · ~{rows:N0} rows ({bytes / 1048576.0:N1} MB)";
            OpsChurnButton.Content = churnAlive ? "Stop" : "Start";

            // Value model: report file next to repo root.
            var models = Path.Combine(RepoRootDir(), "models");
            var report = Directory.Exists(models)
                ? new DirectoryInfo(models).GetFiles("report-*.json").OrderByDescending(f => f.LastWriteTime).FirstOrDefault()
                : null;
            if (report != null)
            {
                using var doc = JsonDocument.Parse(File.ReadAllText(report.FullName));
                var sp = doc.RootElement.TryGetProperty("spearman", out var s) ? s.GetDouble() : 0;
                OpsTrainStatus.Text = $"trained {report.LastWriteTime:MM-dd HH:mm} · spearman {sp:N2}";
            }
            else
            {
                OpsTrainStatus.Text = rows >= 5000
                    ? "corpus ready — not trained yet"
                    : $"needs ≥5k rows (have ~{rows:N0})";
            }
        }
        catch
        {
            // status refresh is cosmetic — never throw
        }
    }

    private void OpsChurnButton_Click(object sender, RoutedEventArgs e)
    {
        var root = RepoRootDir();
        var script = Path.Combine(root, "scripts", "corpus-churn.ps1");
        if (_churnProc is { HasExited: false })
        {
            try
            {
                File.WriteAllText(Path.Combine(root, "scratch", "STOP-CHURN"), "stop");
                AppendLog("[ops] churn stop requested (sentinel written — stops after the current cycle)");
            }
            catch (Exception ex)
            {
                AppendLog($"[ops] churn stop failed: {ex.Message}");
            }
            return;
        }
        if (!File.Exists(script))
        {
            AppendLog($"[ops] churn script not found at {script}");
            return;
        }
        try
        {
            var sentinel = Path.Combine(root, "scratch", "STOP-CHURN");
            Directory.CreateDirectory(Path.Combine(root, "scratch"));
            if (File.Exists(sentinel)) File.Delete(sentinel);
            var churnLog = Path.Combine(Path.GetTempPath(), "mr-churn-ui.log");
            var psi = new System.Diagnostics.ProcessStartInfo
            {
                FileName = "powershell.exe",
                Arguments = $"-NoProfile -ExecutionPolicy Bypass -File \"{script}\"",
                WorkingDirectory = root,
                UseShellExecute = false,      // hidden — no console window
                CreateNoWindow = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            };
            _churnProc = new System.Diagnostics.Process { StartInfo = psi, EnableRaisingEvents = true };
            var sink = new StreamWriter(churnLog, append: false) { AutoFlush = true };
            _churnProc.OutputDataReceived += (_, a) => { if (a.Data != null) sink.WriteLine(a.Data); };
            _churnProc.ErrorDataReceived += (_, a) => { if (a.Data != null) sink.WriteLine(a.Data); };
            _churnProc.Exited += (_, _) => Dispatcher.Invoke(() =>
            {
                try { sink.Dispose(); } catch { }
                string tail = "";
                try { tail = File.ReadLines(churnLog).LastOrDefault() ?? ""; } catch { }
                AppendLog($"[ops] churn exited (code {_churnProc?.ExitCode}) — {tail}  · full log: {churnLog}");
                RefreshOpsStatus();
            });
            _churnProc.Start();
            _churnProc.BeginOutputReadLine();
            _churnProc.BeginErrorReadLine();
            AppendLog($"[ops] corpus churn running in the background (no window) — progress in the Ops box; log: {churnLog}");
            RefreshOpsStatus();
        }
        catch (Exception ex)
        {
            AppendLog($"[ops] churn start failed: {ex.Message}");
        }
    }

    private async void OpsRescoreButton_Click(object sender, RoutedEventArgs e)
    {
        OpsRescoreButton.IsEnabled = false;
        OpsRescoreStatus.Text = "running…";
        AppendLog("[ops] rescore_archive started (re-runs PoB on every elite)");
        try
        {
            var json = await _service.RescoreArchiveAsync();
            OpsRescoreStatus.Text = $"done {DateTime.Now:HH:mm} · {Snippet(json.Replace("\n", " "), 60)}";
            AppendLog($"[ops] rescore done: {Snippet(json.Replace("\n", " "), 200)}");
            await RefreshArchiveAsync();
        }
        catch (Exception ex)
        {
            OpsRescoreStatus.Text = "FAILED (see log)";
            AppendLog($"[ops] rescore failed: {ex.Message}");
        }
        finally
        {
            OpsRescoreButton.IsEnabled = true;
        }
    }

    private void OpsTrainButton_Click(object sender, RoutedEventArgs e)
    {
        var root = RepoRootDir();
        var script = Path.Combine(root, "scripts", "train-value-model.py");
        if (!File.Exists(script))
        {
            AppendLog($"[ops] trainer not found at {script}");
            return;
        }
        try
        {
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
            {
                FileName = "py",
                Arguments = $"-3 \"{script}\" --out \"{Path.Combine(root, "models")}\"",
                WorkingDirectory = root,
                UseShellExecute = true,   // own console — pip/teaching output visible
            });
            AppendLog("[ops] value-model training launched in its own console (needs: pip install lightgbm scikit-learn scipy)");
        }
        catch (Exception ex)
        {
            AppendLog($"[ops] training launch failed: {ex.Message}");
        }
    }
}
