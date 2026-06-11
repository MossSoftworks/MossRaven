using System;
using System.IO;
using System.IO.Compression;
using System.Linq;
using System.Net.Http;
using System.Text.Json;
using System.Threading.Tasks;

namespace MossRaven.Services;

/// <summary>
/// "Package PoB inside the app" — policy-clean version. PoB2's CODE is MIT
/// but its GGG-derived data/assets fall under the fan-content policy (no
/// redistribution), so we never bundle it. Instead the app downloads the
/// OFFICIAL portable release from GitHub on demand (~365 MB, once) into
/// %LOCALAPPDATA%\MossRaven\PoB2 and points the embed pane at it. Same
/// user experience as shipping it, none of the redistribution.
/// </summary>
public static class PobBootstrap
{
    private const string ReleaseApi =
        "https://api.github.com/repos/PathOfBuildingCommunity/PathOfBuilding-PoE2/releases/latest";

    public static string RuntimeDir => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "MossRaven", "PoB2");

    /// <summary>Marker + snippet appended to the LOCAL PoB copy's
    /// Modules/Main.lua: every ~0.5s it checks mossraven-live.sig next to the
    /// PoB exe; when the counter changes it loads mossraven-live.xml into the
    /// running instance (SetMode BUILD). This is what makes clicks load into
    /// the LIVE window and lets the tree light up during search. We patch the
    /// user's local copy only — never redistributed.</summary>
    private const string LiveLinkMarker = "-- MossRaven live-link v2";
    private const string LiveLinkLua = @"
-- MossRaven live-link v2 (appended by MossRaven; safe to delete)
do
    local mrOrigOnFrame = main.OnFrame
    local mrTick, mrLastSig = 0, nil
    function main:OnFrame(...)
        mrOrigOnFrame(self, ...)
        mrTick = mrTick + 1
        if mrTick >= 5 then
            mrTick = 0
            local sf = io.open(""mossraven-live.sig"", ""rb"")
            if sf then
                local sig = sf:read(""*l"")
                sf:close()
                if sig and sig ~= mrLastSig then
                    mrLastSig = sig
                    local xf = io.open(""mossraven-live.xml"", ""rb"")
                    if xf then
                        local xml = xf:read(""*a"")
                        xf:close()
                        if xml and #xml > 100 then
                            self:SetMode(""BUILD"", false, ""MossRaven Live"", xml)
                        end
                    end
                end
            end
        end
    end
end
";

    /// <summary>Append the live-link watcher to the runtime's Main.lua once.</summary>
    public static void EnsureLiveLink(string exePath, Action<string> log)
    {
        try
        {
            var dir = Path.GetDirectoryName(exePath) ?? RuntimeDir;
            var mainLua = Path.Combine(dir, "Modules", "Main.lua");
            if (!File.Exists(mainLua))
            {
                log("[pob-live] Modules/Main.lua not found — live-link unavailable for this PoB layout");
                return;
            }
            var text = File.ReadAllText(mainLua);
            if (text.Contains(LiveLinkMarker)) return; // current version present
            // Strip any older injected block (always appended at EOF).
            var oldIdx = text.IndexOf("-- MossRaven live-link", StringComparison.Ordinal);
            if (oldIdx >= 0)
                text = text.Substring(0, oldIdx).TrimEnd() + "\n";
            File.WriteAllText(mainLua, text + "\n" + LiveLinkLua);
            log("[pob-live] live-link v2 injected (~80ms poll) into local PoB copy");
        }
        catch (Exception ex)
        {
            log($"[pob-live] injection failed: {ex.Message}");
        }
    }

    private static long _liveCounter = Environment.TickCount64;

    /// <summary>Push a build into the RUNNING embedded PoB via the handoff.</summary>
    public static void PushLive(string exePath, string xml, Action<string> log)
    {
        try
        {
            var dir = Path.GetDirectoryName(exePath) ?? RuntimeDir;
            File.WriteAllText(Path.Combine(dir, "mossraven-live.xml"), xml);
            File.WriteAllText(Path.Combine(dir, "mossraven-live.sig"),
                (++_liveCounter).ToString());
        }
        catch (Exception ex)
        {
            log($"[pob-live] push failed: {ex.Message}");
        }
    }

    /// <summary>Find an already-bootstrapped PoB2 exe, or null.</summary>
    public static string? FindExistingExe()
    {
        if (!Directory.Exists(RuntimeDir)) return null;
        return Directory
            .EnumerateFiles(RuntimeDir, "Path of Building*.exe", SearchOption.AllDirectories)
            .FirstOrDefault();
    }

    /// <summary>
    /// Ensure the PoB2 runtime exists locally; download + extract the
    /// official portable release if not. Returns the exe path. Progress
    /// lines go to <paramref name="log"/> (this is a one-time ~365 MB pull —
    /// say so loudly).
    /// </summary>
    public static async Task<string?> EnsureRuntimeAsync(Action<string> log)
    {
        var existing = FindExistingExe();
        if (existing != null) return existing;

        Directory.CreateDirectory(RuntimeDir);
        using var http = new HttpClient();
        http.DefaultRequestHeaders.UserAgent.ParseAdd("MossRaven/0.2 (+github.com/MossSoftworks/MossRaven)");
        http.Timeout = TimeSpan.FromMinutes(30);

        log("[pob-setup] looking up the latest official PoB2 release…");
        string? zipUrl = null, tag = null;
        long size = 0;
        try
        {
            var meta = await http.GetStringAsync(ReleaseApi);
            using var doc = JsonDocument.Parse(meta);
            tag = doc.RootElement.TryGetProperty("tag_name", out var t) ? t.GetString() : "?";
            foreach (var a in doc.RootElement.GetProperty("assets").EnumerateArray())
            {
                var name = a.GetProperty("name").GetString() ?? "";
                if (name.Contains("Portable", StringComparison.OrdinalIgnoreCase) && name.EndsWith(".zip"))
                {
                    zipUrl = a.GetProperty("browser_download_url").GetString();
                    size = a.TryGetProperty("size", out var sz) ? sz.GetInt64() : 0;
                    break;
                }
            }
        }
        catch (Exception ex)
        {
            log($"[pob-setup] release lookup failed: {ex.Message}");
            return null;
        }
        if (zipUrl == null)
        {
            log("[pob-setup] no portable zip on the latest release — install PoB2 manually and set its path in Settings");
            return null;
        }

        var zipPath = Path.Combine(RuntimeDir, "pob2-portable.zip");
        log($"[pob-setup] downloading PoB2 {tag} portable ({size / 1048576.0:N0} MB, one time) — this takes a few minutes…");
        try
        {
            using (var resp = await http.GetAsync(zipUrl, HttpCompletionOption.ResponseHeadersRead))
            {
                resp.EnsureSuccessStatusCode();
                await using var src = await resp.Content.ReadAsStreamAsync();
                await using var dst = File.Create(zipPath);
                var buf = new byte[1 << 20];
                long done = 0, lastMark = 0;
                int n;
                while ((n = await src.ReadAsync(buf)) > 0)
                {
                    await dst.WriteAsync(buf.AsMemory(0, n));
                    done += n;
                    if (done - lastMark > 50L * 1048576)
                    {
                        lastMark = done;
                        log($"[pob-setup] …{done / 1048576} MB");
                    }
                }
            }
            log("[pob-setup] extracting…");
            ZipFile.ExtractToDirectory(zipPath, RuntimeDir, overwriteFiles: true);
            File.Delete(zipPath);
        }
        catch (Exception ex)
        {
            log($"[pob-setup] download/extract failed: {ex.Message}");
            return null;
        }

        var exe = FindExistingExe();
        log(exe != null
            ? $"[pob-setup] PoB2 ready: {exe}"
            : "[pob-setup] extracted but no 'Path of Building*.exe' found — set the path manually in Settings");
        return exe;
    }
}
