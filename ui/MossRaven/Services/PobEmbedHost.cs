using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Interop;

namespace MossRaven.Services;

/// <summary>
/// Embeds the REAL desktop Path of Building 2 inside a WPF pane by
/// re-parenting its top-level window (classic Win32 SetParent hosting).
///
/// PoB2 (SimpleGraphic) creates TWO top-level windows: a small ~566x539
/// boot CONSOLE (text log) and the app-sized ~1096x759 GUI. On a healthy
/// boot the console hides itself and the GUI is shown. We therefore:
///   - capture the app-sized (>=700px) GUI window — and ONLY once it's
///     visible, which means boot is done and its thread is pumping, so the
///     cross-process SetParent can't deadlock our UI thread (the freeze we
///     hit when we used to capture the console mid-boot);
///   - hide the small console so it never flashes on the desktop;
///   - match by our process id first, falling back to NEW PoB-titled
///     windows (never one that predates our launch — that's the user's own
///     PoB session).
/// Pixel-probe (scripts/pixel-probe.ps1) is the source of truth for what's
/// actually in the pane; window geometry alone lied for several rounds.
///
/// Known limits of SetParent hosting (documented, not bugs): keyboard
/// focus follows clicks into the PoB area; modal PoB dialogs open as
/// real top-level windows; closing MossRaven kills the embedded PoB.
/// </summary>
public sealed class PobEmbedHost : HwndHost
{
    private readonly string _exePath;
    private Process? _proc;
    private IntPtr _child = IntPtr.Zero;
    private readonly Action<string> _log;
    private readonly HashSet<IntPtr> _hiddenByUs = new();
    // PoB windows alive BEFORE our launch (the user's own session) — never
    // steal these via the title fallback.
    private readonly HashSet<IntPtr> _preexisting = new();

    public PobEmbedHost(string exePath, Action<string> log)
    {
        _exePath = exePath;
        _log = log;
    }

    public bool IsAlive => _child != IntPtr.Zero;

    protected override HandleRef BuildWindowCore(HandleRef hwndParent)
    {
        var host = CreateWindowEx(
            0, "STATIC", "",
            WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN,
            0, 0, 100, 100,
            hwndParent.Handle, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
        _ = AttachPobAsync(host);
        return new HandleRef(this, host);
    }

    private async Task AttachPobAsync(IntPtr host)
    {
        try
        {
            foreach (var w in TitledTopLevels("Path of Building")) _preexisting.Add(w);

            // Windows spawn off-screen (vid_last x,y=-32000) — zero desktop
            // flashes by construction; we move the GUI into the pane.
            PobBootstrap.PrepareGraphicsConfig(_exePath, _log);

            _proc = Process.Start(new ProcessStartInfo
            {
                FileName = _exePath,
                WorkingDirectory = System.IO.Path.GetDirectoryName(_exePath) ?? ".",
                UseShellExecute = true,
            });
            if (_proc == null)
            {
                _log("[pob-embed] process start returned null");
                return;
            }
            // Hunt for the APP-SIZED GUI window. Capture nothing before it
            // exists: until boot finishes the only window is the console,
            // and parenting that mid-boot freezes the UI thread.
            for (int i = 0; i < 360 && _child == IntPtr.Zero; i++)
            {
                await Task.Delay(i < 180 ? 16 : 150);
                HideConsoleWindows();          // keep the boot console off-screen
                var cand = FindAppSizedWindow();
                if (cand != IntPtr.Zero)
                {
                    _child = cand;
                    _hiddenByUs.Remove(cand);
                    _log($"[pob-embed] capturing {Describe(cand)}");
                }
                else if (i == 60 || i == 200)
                    _log($"[pob-embed] waiting for app-sized GUI; pid windows: {DumpPidWindows()}");
            }
            if (_child == IntPtr.Zero)
            {
                _log("[pob-embed] no app-sized PoB GUI appeared; pid windows: " + DumpPidWindows());
                UnhideHidden();
                return;
            }
            Capture(host);
            _log("[pob-embed] PoB2 embedded");
            _ = Task.Run(() => WatchdogAsync(host));
        }
        catch (Exception ex)
        {
            _log($"[pob-embed] {ex.Message}");
        }
    }

    private async Task WatchdogAsync(IntPtr host)
    {
        int tick = 0;
        while (IsWindow(host))
        {
            await Task.Delay(300);
            tick++;
            HideConsoleWindows();
            if (_child != IntPtr.Zero && !IsWindow(_child))
            {
                _log("[pob-embed] embedded GUI window closed — re-grabbing");
                _child = IntPtr.Zero;
                for (int j = 0; j < 200 && _child == IntPtr.Zero && IsWindow(host); j++)
                {
                    await Task.Delay(100);
                    HideConsoleWindows();
                    var c2 = FindAppSizedWindow();
                    if (c2 != IntPtr.Zero) { _child = c2; _hiddenByUs.Remove(c2); }
                }
                if (_child != IntPtr.Zero)
                {
                    try { Capture(host); _log($"[pob-embed] re-captured {Describe(_child)}"); }
                    catch { }
                }
                else
                {
                    _log("[pob-embed] re-grab found nothing; pid windows: " + DumpPidWindows());
                    UnhideHidden();
                }
                continue;
            }
            if (_child == IntPtr.Zero) continue;
            if (tick % 5 == 0 && GetParent(_child) != host)
            {
                try { Capture(host); _log("[pob-embed] re-captured PoB2 window"); }
                catch { }
            }
        }
    }

    protected override void OnRenderSizeChanged(SizeChangedInfo sizeInfo)
    {
        base.OnRenderSizeChanged(sizeInfo);
        ResizeChild();
    }

    private void ResizeChild()
    {
        if (_child == IntPtr.Zero) return;
        var src = PresentationSource.FromVisual(this);
        var scale = src?.CompositionTarget?.TransformToDevice.M11 ?? 1.0;
        var w = Math.Max(50, (int)(ActualWidth * scale));
        var h = Math.Max(50, (int)(ActualHeight * scale));
        // ASYNC so the WPF UI thread never blocks on PoB's message pump.
        SetWindowPos(_child, IntPtr.Zero, 0, 0, w, h,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);
    }

    protected override void DestroyWindowCore(HandleRef hwnd)
    {
        try
        {
            if (_proc is { HasExited: false })
                _proc.Kill(entireProcessTree: true);
        }
        catch { /* shutdown best-effort */ }
        DestroyWindow(hwnd.Handle);
    }

    /// <summary>Strip frame styles, parent into the host, force a frame
    /// recalc, and size to the pane. Only called on the app-sized GUI, which
    /// by definition is past boot and pumping messages.</summary>
    private void Capture(IntPtr host)
    {
        var style = GetWindowLongPtr(_child, GWL_STYLE).ToInt64();
        style &= ~(WS_POPUP | WS_CAPTION | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_SYSMENU);
        style |= WS_CHILD | WS_VISIBLE;
        SetWindowLongPtr(_child, GWL_STYLE, new IntPtr(style));
        var ex = GetWindowLongPtr(_child, GWL_EXSTYLE).ToInt64();
        ex &= ~WS_EX_APPWINDOW;
        SetWindowLongPtr(_child, GWL_EXSTYLE, new IntPtr(ex));
        SetParent(_child, host);
        SetWindowPos(_child, IntPtr.Zero, 0, 0, 0, 0,
            SWP_NOSIZE | SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        ShowWindowAsync(_child, SW_SHOWNA);
        Dispatcher.BeginInvoke(ResizeChild);
    }

    private static bool IsAppSized(IntPtr hwnd)
    {
        if (!GetWindowRect(hwnd, out var r)) return false;
        return (r.Right - r.Left) >= 700 && (r.Bottom - r.Top) >= 480;
    }

    /// <summary>The app-sized GUI: visible, top-level, app-sized. Prefer a
    /// pid match; fall back to a NEW PoB-titled window (never the user's own
    /// pre-existing session).</summary>
    private IntPtr FindAppSizedWindow()
    {
        IntPtr found = IntPtr.Zero;
        uint pid = SafePid();
        EnumWindows((hwnd, _) =>
        {
            if (hwnd == _child) return true;
            if (!IsWindowVisible(hwnd) || GetParent(hwnd) != IntPtr.Zero) return true;
            if (!IsAppSized(hwnd)) return true;
            GetWindowThreadProcessId(hwnd, out var wpid);
            bool ours = pid != 0 && wpid == pid;
            if (!ours)
            {
                if (_preexisting.Contains(hwnd)) return true;
                if (!TitleOf(hwnd).Contains("Path of Building", StringComparison.OrdinalIgnoreCase)) return true;
            }
            found = hwnd;
            return false;
        }, IntPtr.Zero);
        return found;
    }

    /// <summary>Hide every visible, NON-app-sized top-level window of our pid
    /// (the boot console) so it never flashes. Our captured GUI is app-sized,
    /// so it's never a target here. PoB modal dialogs are owned windows
    /// (GetParent != 0) and are skipped.</summary>
    private void HideConsoleWindows()
    {
        uint pid = SafePid();
        if (pid == 0) return;
        EnumWindows((hwnd, _) =>
        {
            if (hwnd == _child) return true;
            GetWindowThreadProcessId(hwnd, out var wpid);
            if (wpid != pid) return true;
            if (!IsWindowVisible(hwnd) || GetParent(hwnd) != IntPtr.Zero) return true;
            if (IsAppSized(hwnd)) return true; // never hide the GUI
            ShowWindowAsync(hwnd, SW_HIDE);
            if (_hiddenByUs.Add(hwnd))
                _log($"[pob-embed] boot console hidden: {Describe(hwnd)}");
            return true;
        }, IntPtr.Zero);
    }

    private void UnhideHidden()
    {
        foreach (var hwnd in _hiddenByUs)
            if (IsWindow(hwnd)) ShowWindowAsync(hwnd, SW_SHOW);
        if (_hiddenByUs.Count > 0)
            _log("[pob-embed] hidden windows restored (no GUI captured — check their output)");
        _hiddenByUs.Clear();
    }

    private uint SafePid()
    {
        try { return _proc is { HasExited: false } ? (uint)_proc.Id : 0; }
        catch { return 0; }
    }

    private static IEnumerable<IntPtr> TitledTopLevels(string needle)
    {
        var list = new List<IntPtr>();
        EnumWindows((hwnd, _) =>
        {
            if (GetParent(hwnd) != IntPtr.Zero) return true;
            if (TitleOf(hwnd).Contains(needle, StringComparison.OrdinalIgnoreCase)) list.Add(hwnd);
            return true;
        }, IntPtr.Zero);
        return list;
    }

    private string DumpPidWindows()
    {
        uint pid = SafePid();
        if (pid == 0) return "(no proc)";
        var list = new List<string>();
        EnumWindows((hwnd, _) =>
        {
            GetWindowThreadProcessId(hwnd, out var wpid);
            if (wpid == pid && GetParent(hwnd) == IntPtr.Zero) list.Add(Describe(hwnd));
            return true;
        }, IntPtr.Zero);
        return list.Count == 0 ? "(none)" : string.Join(" | ", list);
    }

    private static string TitleOf(IntPtr hwnd)
    {
        var sb = new System.Text.StringBuilder(256);
        GetWindowText(hwnd, sb, sb.Capacity);
        return sb.ToString();
    }

    private static string Describe(IntPtr hwnd)
    {
        GetWindowRect(hwnd, out var r);
        return $"hwnd=0x{hwnd.ToInt64():X} \"{TitleOf(hwnd)}\" visible={IsWindowVisible(hwnd)} size={r.Right - r.Left}x{r.Bottom - r.Top}";
    }

    // ----- Win32 -----
    private const int GWL_STYLE = -16;
    private const long WS_CHILD = 0x40000000;
    private const long WS_VISIBLE = 0x10000000;
    private const long WS_CLIPCHILDREN = 0x02000000;
    private const long WS_POPUP = unchecked((long)0x80000000);
    private const long WS_CAPTION = 0x00C00000;
    private const long WS_THICKFRAME = 0x00040000;
    private const long WS_MINIMIZEBOX = 0x00020000;
    private const long WS_MAXIMIZEBOX = 0x00010000;
    private const long WS_SYSMENU = 0x00080000;
    private const int GWL_EXSTYLE = -20;
    private const long WS_EX_APPWINDOW = 0x00040000;
    private const uint SWP_NOSIZE = 0x0001;
    private const uint SWP_NOMOVE = 0x0002;
    private const uint SWP_NOZORDER = 0x0004;
    private const uint SWP_NOACTIVATE = 0x0010;
    private const uint SWP_FRAMECHANGED = 0x0020;
    private const uint SWP_ASYNCWINDOWPOS = 0x4000;
    private const int SW_HIDE = 0;
    private const int SW_SHOW = 5;
    private const int SW_SHOWNA = 8;

    private delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lparam);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lparam);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] private static extern IntPtr GetParent(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int w, int h, uint flags);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetWindowText(IntPtr hwnd, System.Text.StringBuilder text, int count);
    [DllImport("user32.dll")] private static extern bool IsWindow(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] private static extern bool ShowWindowAsync(IntPtr hwnd, int cmd);
    [StructLayout(LayoutKind.Sequential)]
    private struct RECT { public int Left, Top, Right, Bottom; }

    [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern IntPtr CreateWindowEx(
        int exStyle, string className, string windowName, long style,
        int x, int y, int width, int height,
        IntPtr parent, IntPtr menu, IntPtr instance, IntPtr param);

    [DllImport("user32.dll")] private static extern bool DestroyWindow(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern IntPtr SetParent(IntPtr child, IntPtr parent);
    [DllImport("user32.dll")] private static extern bool MoveWindow(IntPtr hwnd, int x, int y, int w, int h, bool repaint);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern IntPtr GetWindowLongPtr(IntPtr hwnd, int index);
    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")]
    private static extern IntPtr SetWindowLongPtr(IntPtr hwnd, int index, IntPtr value);
}
