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

    private void InitRework()
    {
        BuildPrefCombos();
        _opsTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(10) };
        _opsTimer.Tick += (_, _) => RefreshOpsStatus();
        _opsTimer.Start();
        RefreshOpsStatus();
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

    private void WsEmbedButton_Click(object sender, RoutedEventArgs e) => EnsurePobEmbedded();

    private void EnsurePobEmbedded()
    {
        if (_pobHost is { IsAlive: true }) return;
        var pob = _settings.PobInstallPath ?? "";
        if (pob.Length == 0 || !File.Exists(pob))
        {
            AppendLog("[pob-embed] set the PoB2 executable path in Settings (gear) first");
            PobHostHint.Text = "Set the PoB2 executable path in Settings (gear icon), then Launch PoB2 here.";
            return;
        }
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

    /// <summary>Write the build into PoB2's Builds folder (always) and make
    /// sure the embedded PoB2 is up so the user can open it immediately.</summary>
    private void PushBuildToPob(string xml, string title)
    {
        try
        {
            var pob = _settings.PobInstallPath ?? "";
            string buildsDir;
            if (pob.Length > 0 && File.Exists(pob))
            {
                buildsDir = Path.Combine(Path.GetDirectoryName(pob) ?? ".", "Builds");
                if (!Directory.Exists(buildsDir))
                    buildsDir = Path.Combine(
                        Environment.GetFolderPath(Environment.SpecialFolder.MyDocuments),
                        "Path of Building", "Builds");
            }
            else
            {
                buildsDir = Path.Combine(
                    Environment.GetFolderPath(Environment.SpecialFolder.MyDocuments),
                    "Path of Building", "Builds");
            }
            Directory.CreateDirectory(buildsDir);
            var safe = new string(title.Where(c => char.IsLetterOrDigit(c) || c == ' ' || c == '-').ToArray()).Trim();
            if (safe.Length == 0) safe = "MossRaven build";
            if (safe.Length > 40) safe = safe.Substring(0, 40);
            var file = Path.Combine(buildsDir, $"MossRaven - {safe}.xml");
            File.WriteAllText(file, xml);
            AppendLog($"[pob] wrote '{Path.GetFileName(file)}' into PoB2 Builds — open it from PoB2's build list (refresh the list if it's already open)");
            EnsurePobEmbedded();
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
    private void RefreshOpsStatus()
    {
        try
        {
            // Corpus: newest evals file size → row estimate (~2 KB/row).
            var corpusDir = Path.Combine(DataDir(), "corpus");
            long bytes = 0;
            if (Directory.Exists(corpusDir))
                bytes = new DirectoryInfo(corpusDir).GetFiles("evals-*.jsonl").Sum(f => f.Length);
            var rows = bytes / 2000;
            var churnAlive = _churnProc is { HasExited: false };
            OpsChurnStatus.Text = $"Corpus churn — {(churnAlive ? "RUNNING" : "idle")} · ~{rows:N0} rows ({bytes / 1048576.0:N1} MB)";
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
                OpsTrainStatus.Text = $"Value model — trained {report.LastWriteTime:MM-dd HH:mm} · spearman {sp:N2}";
            }
            else
            {
                OpsTrainStatus.Text = rows >= 5000
                    ? "Value model — corpus ready, not trained"
                    : $"Value model — needs ≥5k rows (have ~{rows:N0})";
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
            _churnProc = System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
            {
                FileName = "powershell.exe",
                Arguments = $"-NoProfile -ExecutionPolicy Bypass -File \"{script}\"",
                WorkingDirectory = root,
                UseShellExecute = true,   // visible console so progress is watchable
            });
            AppendLog("[ops] corpus churn started (own console window; Stop writes the sentinel)");
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
        OpsRescoreStatus.Text = "Rescore archive — running…";
        AppendLog("[ops] rescore_archive started (re-runs PoB on every elite)");
        try
        {
            var json = await _service.RescoreArchiveAsync();
            OpsRescoreStatus.Text = $"Rescore archive — done {DateTime.Now:HH:mm} · {Snippet(json.Replace("\n", " "), 60)}";
            AppendLog($"[ops] rescore done: {Snippet(json.Replace("\n", " "), 200)}");
            await RefreshArchiveAsync();
        }
        catch (Exception ex)
        {
            OpsRescoreStatus.Text = "Rescore archive — FAILED (see log)";
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
