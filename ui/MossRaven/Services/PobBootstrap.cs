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
    // v3 is wrapped in BEGIN/END sentinels and inserted BEFORE Main.lua's
    // trailing `return main`. v1/v2 appended AFTER the return — a Lua syntax
    // error ('<eof>' expected near 'do'): nothing may follow `return` in a
    // chunk. That silently broke Main.lua loading every launch (revealed by
    // pixel-probe 2026-06-12 once the GetVirtualScreenSize popup stopped
    // masking it). EnsureLiveLink migrates the broken v1/v2 tail away.
    private const string LiveBegin = "-- MossRaven live-link v3 BEGIN (auto-managed; do not edit within sentinels)";
    private const string LiveEnd = "-- MossRaven live-link v3 END";
    private const string LiveLinkLua = @"-- MossRaven live-link v3 BEGIN (auto-managed; do not edit within sentinels)
do
    local mrOrigOnFrame = main.OnFrame
    local mrTick, mrLastSig = 0, nil
    function main:OnFrame(...)
        if mrOrigOnFrame then mrOrigOnFrame(self, ...) end
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
-- MossRaven live-link v3 END";

    /// <summary>Inject the live-link watcher into Main.lua, BEFORE its
    /// trailing `return main`. Idempotent and self-healing: strips any prior
    /// v3 block and migrates the broken v1/v2 trailing block.</summary>
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
            var original = File.ReadAllText(mainLua);
            var text = original;

            // 1. Remove any existing v3 sentinel block (anywhere).
            int b = text.IndexOf(LiveBegin, StringComparison.Ordinal);
            int e = text.IndexOf(LiveEnd, StringComparison.Ordinal);
            if (b >= 0 && e > b)
            {
                int endPos = e + LiveEnd.Length;
                text = (text.Substring(0, b).TrimEnd() + "\n") + text.Substring(endPos).TrimStart('\r', '\n');
            }

            // 2. Migrate the broken v1/v2 tail: those were appended after
            // `return main`, so anything from their marker to EOF is dead.
            int legacy = text.IndexOf("-- MossRaven live-link v", StringComparison.Ordinal);
            if (legacy >= 0)
                text = text.Substring(0, legacy).TrimEnd() + "\n";

            var alreadyClean = text; // text with no MossRaven block at all

            // 3. Insert before the LAST `return main` (must stay last in chunk).
            int ret = alreadyClean.LastIndexOf("\nreturn main", StringComparison.Ordinal);
            string patched;
            if (ret >= 0)
                patched = alreadyClean.Substring(0, ret + 1) + LiveLinkLua + "\n\n" + alreadyClean.Substring(ret + 1);
            else
                patched = alreadyClean.TrimEnd() + "\n\n" + LiveLinkLua + "\n"; // module has no trailing return

            if (patched == original) return; // nothing to do

            File.WriteAllText(mainLua, patched, new System.Text.UTF8Encoding(false));
            log(ret >= 0
                ? "[pob-live] live-link v3 injected before 'return main' (~80ms poll); migrated any broken v1/v2 tail"
                : "[pob-live] live-link v3 appended (no trailing return found)");
        }
        catch (Exception ex)
        {
            log($"[pob-live] injection failed: {ex.Message}");
        }
    }

    private const string StabilityMarker = "-- MossRaven stability shim";

    /// <summary>
    /// Pin the local PoB to its bundled, version-matched release and stop it
    /// from auto-updating its Lua scripts past the bundled SimpleGraphic
    /// runtime. PoB's portable bundle is internally consistent, but on first
    /// run it pulls bleeding-edge scripts from the `master` branch — which
    /// call newer runtime exports (e.g. GetVirtualScreenSize) the bundled
    /// exe lacks, hard-crashing the boot popup at Launch.lua. (Proven by
    /// pixel-probe 2026-06-12: standalone PoB, no MossRaven, same crash.)
    ///
    /// Idempotent; runs every launch. Three guards:
    ///  1. delete `first.run` — kills the fresh-install immediate update;
    ///  2. neuter `self:CheckForUpdate(true)` — kills the 12h background one;
    ///  3. shim a missing GetVirtualScreenSize -> GetScreenSize, as belt-and-
    ///     suspenders for an install already on mismatched master scripts
    ///     (deferred resolution so it uses the real screen size at draw time).
    /// </summary>
    public static void StabilizePob(string exePath, Action<string> log)
    {
        try
        {
            var dir = Path.GetDirectoryName(exePath) ?? RuntimeDir;
            var firstRun = Path.Combine(dir, "first.run");
            if (File.Exists(firstRun))
            {
                try { File.Delete(firstRun); log("[pob-stable] removed first.run (skips master auto-update)"); }
                catch { }
            }
            var launchLua = Path.Combine(dir, "Launch.lua");
            if (!File.Exists(launchLua))
            {
                log("[pob-stable] Launch.lua not found — skipping stability patch");
                return;
            }
            var text = File.ReadAllText(launchLua);
            if (text.Contains(StabilityMarker)) return; // already patched

            // Insurance shim, inserted right after the window title is set so
            // it exists before any DrawPopup / restart-overlay call. Deferred
            // body: GetScreenSize is only valid after RenderInit, so resolve
            // it on call, not now.
            const string shim =
                "\n" + StabilityMarker + " (auto-update off + missing-export guard)\n" +
                "if not GetVirtualScreenSize then\n" +
                "\tGetVirtualScreenSize = function()\n" +
                "\t\tif GetScreenSize then return GetScreenSize() end\n" +
                "\t\treturn 2560, 1440\n" +
                "\tend\n" +
                "end\n";
            var anchor = "SetWindowTitle(APP_NAME)";
            var idx = text.IndexOf(anchor, StringComparison.Ordinal);
            if (idx >= 0)
            {
                var insertAt = text.IndexOf('\n', idx);
                if (insertAt < 0) insertAt = idx + anchor.Length;
                text = text.Substring(0, insertAt + 1) + shim + text.Substring(insertAt + 1);
            }
            else
            {
                // Unknown layout — prepend after any leading #@ directive line.
                var nl = text.IndexOf('\n');
                text = (nl >= 0 ? text.Substring(0, nl + 1) : "") + shim + (nl >= 0 ? text.Substring(nl + 1) : text);
            }

            // Disable both update paths (all occurrences).
            text = text.Replace("self:CheckForUpdate(true)",
                                 "if false then self:CheckForUpdate(true) end --MossRaven");

            // Write WITHOUT a BOM: Launch.lua line 1 is `#@ SimpleGraphic`,
            // a directive PoB's loader reads from byte 0; a BOM breaks it.
            File.WriteAllText(launchLua, text, new System.Text.UTF8Encoding(false));
            log("[pob-stable] pinned PoB to bundled version (updates off, GetVirtualScreenSize shimmed)");
        }
        catch (Exception ex)
        {
            log($"[pob-stable] patch failed: {ex.Message}");
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
