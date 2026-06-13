# pixel-probe.ps1 — Claude's permissionless eye for embed iteration.
# PrintWindow every MossRaven/PoB top-level window (visible or hidden) plus a
# full-screen BitBlt into -OutDir as PNGs; Claude then Reads the PNGs to see
# actual pixels. Works from the sandboxed shell (same window station), no
# permission dialogs, no user in the loop. Born 2026-06-12 when window-
# geometry probes kept passing while the user saw a dead white pane.
param([string]$OutDir = "C:\#AppProjects\MossRaven\scratch\shots")

New-Item -ItemType Directory -Force $OutDir | Out-Null

# CRITICAL: PrintWindow CANNOT reliably capture PoB's cross-process OpenGL
# child window (returns white/blank unpredictably — it showed the tree one run
# and white the next on the same working state). The ONLY reliable render check
# is the full-screen BitBlt (screen.png) with MossRaven in the FOREGROUND so
# the embedded GL content is actually composited to the screen. Foreground it
# here before capture; read screen.png (not the per-window PNGs) to judge render.
try {
    Add-Type -TypeDefinition @"
using System;using System.Runtime.InteropServices;
public static class FgProbe {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
}
"@ -ErrorAction SilentlyContinue
    $mr = Get-Process MossRaven -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($mr -and $mr.MainWindowHandle -ne 0) {
        [FgProbe]::ShowWindow($mr.MainWindowHandle, 9) | Out-Null   # SW_RESTORE
        [FgProbe]::SetForegroundWindow($mr.MainWindowHandle) | Out-Null
        Start-Sleep -Milliseconds 700
    }
} catch {}
Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Text;
public static class PixelProbeS {
    delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int c);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    [DllImport("user32.dll")] static extern int GetSystemMetrics(int i);
    [DllImport("user32.dll")] static extern bool IsHungAppWindow(IntPtr h);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L,T,R,B; }
    // PrintWindow sends a SYNCHRONOUS message — a window whose thread never
    // pumps (PoB's dead placeholder) hangs it forever. Guard: skip windows
    // Windows reports as hung, and hard-timeout every shot on a worker task.
    static bool TimedShot(IntPtr h, int w, int ht, string file) {
        var t = System.Threading.Tasks.Task.Run(delegate() {
            using (var bmp = new Bitmap(w, ht)) {
                using (var g = Graphics.FromImage(bmp)) {
                    IntPtr dc = g.GetHdc();
                    PrintWindow(h, dc, 3); // PW_RENDERFULLCONTENT
                    g.ReleaseHdc(dc);
                }
                bmp.Save(file, ImageFormat.Png);
            }
        });
        return t.Wait(3000);
    }
    public static List<string> ShootAll(uint[] pids, string dir) {
        var outp = new List<string>();
        int n = 0;
        var targets = new List<IntPtr>();
        EnumWindows(delegate(IntPtr h, IntPtr l) { targets.Add(h); return true; }, IntPtr.Zero);
        foreach (var h in targets) {
            uint p; GetWindowThreadProcessId(h, out p);
            bool match = false; foreach (var x in pids) { if (x == p) { match = true; } }
            if (!match) continue;
            RECT r; GetWindowRect(h, out r);
            int w = r.R - r.L, ht = r.B - r.T;
            if (w < 40 || ht < 40) continue;
            var sb = new StringBuilder(256); GetWindowText(h, sb, 256);
            n++;
            if (IsHungAppWindow(h)) {
                outp.Add(string.Format("HUNG (skipped) hwnd=0x{0:X} \"{1}\" {2}x{3} vis={4} — thread not pumping", h.ToInt64(), sb, w, ht, IsWindowVisible(h)));
                continue;
            }
            string file = dir + "\\win" + n + "_pid" + p + (IsWindowVisible(h) ? "_vis" : "_hid") + ".png";
            try {
                if (TimedShot(h, w, ht, file))
                    outp.Add(string.Format("{0} <= hwnd=0x{1:X} \"{2}\" {3}x{4} vis={5}", System.IO.Path.GetFileName(file), h.ToInt64(), sb, w, ht, IsWindowVisible(h)));
                else
                    outp.Add(string.Format("TIMEOUT (3s) hwnd=0x{0:X} \"{1}\" — thread not pumping", h.ToInt64(), sb));
            } catch (Exception ex) { outp.Add("FAIL " + sb + ": " + ex.Message); }
        }
        try {
            int sx = GetSystemMetrics(76), sy = GetSystemMetrics(77), sw = GetSystemMetrics(78), sh = GetSystemMetrics(79);
            using (var bmp = new Bitmap(sw, sh)) {
                using (var g = Graphics.FromImage(bmp)) g.CopyFromScreen(sx, sy, 0, 0, new Size(sw, sh));
                bmp.Save(dir + "\\screen.png", ImageFormat.Png);
            }
            outp.Add("screen.png " + sw + "x" + sh);
        } catch (Exception ex) { outp.Add("FAIL screen: " + ex.Message); }
        return outp;
    }
}
"@
$pids = @()
Get-Process | Where-Object { $_.ProcessName -match "^MossRaven$|^Path of Building" } | ForEach-Object { $pids += [uint32]$_.Id }
if ($pids.Count -eq 0) { "no MossRaven/PoB processes running"; exit 1 }
"pids: $($pids -join ',')"
[PixelProbeS]::ShootAll($pids, $OutDir)
