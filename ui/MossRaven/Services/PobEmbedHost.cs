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
/// PoB2 renders via SimpleGraphic (its own swap chain), which tolerates
/// being a child window; we strip the popup frame, glue it to the host's
/// client rect, and resize it with the pane.
///
/// v10 lessons, all paid for:
///  - Capture ONLY the real main window, identified by TITLE ("Path of
///    Building" on this pid), ANY size. Size gates (>=700px) lost a race
///    twice: PoB restores its saved window size (often pane-sized, ~630px)
///    right after creation, so an "app-sized" filter can permanently
///    reject the main window.
///  - NEVER capture the boot console ("SimpleGraphic Console"). SetParent
///    across processes fuses input queues; during boot PoB's thread isn't
///    pumping messages, so capturing the console freezes the whole WPF UI.
///    The console is hidden async instead (severable: drop the ShowWindowAsync
///    calls if PoB ever misbehaves) and un-hidden if no main window appears,
///    so error text is never lost.
///  - Only ever match VISIBLE windows. PoB owns invisible helpers ("GLFW
///    message window", IME) that otherwise get embedded as a dead white pane
///    while the real window escapes (the v6-v8 bug).
///  - All recurring geometry ops use SWP_ASYNCWINDOWPOS so a stalled PoB
///    thread can never hang our UI thread.
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
    // Windows WE hid (boot console + strays) — restored if the main window
    // vanishes so PoB output is never silently lost.
    private readonly HashSet<IntPtr> _hiddenByUs = new();
    // PoB-titled windows that existed BEFORE our launch (e.g. the user's
    // own PoB session) — the title fallback must never steal those.
    private readonly HashSet<IntPtr> _preexisting = new();

    public PobEmbedHost(string exePath, Action<string> log)
    {
        _exePath = exePath;
        _log = log;
    }

    public bool IsAlive => _child != IntPtr.Zero;

    protected override HandleRef BuildWindowCore(HandleRef hwndParent)
    {
        // A plain static container window; PoB gets re-parented into it.
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
            // Wait for the REAL main window (title-matched, any size) and
            // capture nothing before it exists — by then PoB's boot is done
            // and its thread is pumping, so SetParent can't deadlock us.
            for (int i = 0; i < 240 && _child == IntPtr.Zero; i++)
            {
                await Task.Delay(i < 40 ? 50 : 200);
                if (_proc.HasExited && _child == IntPtr.Zero && i == 40)
                {
                    // Squirrel-style stubs exit after spawning the real app —
                    // fall through to the title search instead of bailing.
                    _log("[pob-embed] launcher exited early — searching by window title");
                }
                _child = FindWindowForPid((uint)_proc.Id, "Path of Building", visibleOnly: true);
                if (_child == IntPtr.Zero)
                {
                    HideStrayWindows();
                    // Fallback: the real window may belong to a CHILD process
                    // (updater stubs). Match any NEW top-level titled like PoB.
                    if (_proc.HasExited || i >= 60)
                        _child = FindNewWindowByTitle("Path of Building");
                }
                if (_child != IntPtr.Zero)
                    _log($"[pob-embed] capturing {Describe(_child)}");
                else if (i == 40 || i == 160)
                    _log($"[pob-embed] still waiting for main window; pid windows: {DumpPidWindows()}");
            }
            if (_child == IntPtr.Zero)
            {
                _log("[pob-embed] no PoB2 main window appeared (PoB may show an error); pid windows: " + DumpPidWindows());
                UnhideHiddenStrays(); // never swallow PoB's error output
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

    /// <summary>Keeps the pane owning a live PoB window: re-grabs if PoB
    /// recreates its main window, keeps the boot console off the desktop,
    /// and re-asserts child styles SimpleGraphic occasionally resets.</summary>
    private async Task WatchdogAsync(IntPtr host)
    {
        int tick = 0;
        while (IsWindow(host))
        {
            await Task.Delay(300);
            tick++;
            if (_child != IntPtr.Zero && !IsWindow(_child))
            {
                _log("[pob-embed] embedded window closed — re-grabbing main window");
                _child = IntPtr.Zero;
                for (int j = 0; j < 375 && _child == IntPtr.Zero && IsWindow(host); j++)
                {
                    await Task.Delay(80);
                    var c2 = _proc is { HasExited: false }
                        ? FindWindowForPid((uint)_proc.Id, "Path of Building", visibleOnly: true)
                        : IntPtr.Zero;
                    if (c2 == IntPtr.Zero && j >= 25)
                    {
                        // One of the strays WE hid may be (or have become)
                        // the real window — title-gated, so invisible
                        // helpers (GLFW/IME) can never match.
                        if (_proc is { HasExited: false })
                            c2 = FindWindowForPid((uint)_proc.Id, "Path of Building", visibleOnly: false);
                        if (c2 != IntPtr.Zero) ShowWindowAsync(c2, SW_SHOW);
                    }
                    if (c2 == IntPtr.Zero && (_proc is not { HasExited: false } || j >= 25))
                        c2 = FindNewWindowByTitle("Path of Building");
                    if (c2 != IntPtr.Zero) _child = c2;
                }
                if (_child != IntPtr.Zero)
                {
                    _hiddenByUs.Remove(_child);
                    try { Capture(host); _log($"[pob-embed] re-captured {Describe(_child)}"); }
                    catch { }
                }
                else
                {
                    _log("[pob-embed] re-grab found nothing; pid windows: " + DumpPidWindows());
                    UnhideHiddenStrays();
                }
                continue;
            }
            HideStrayWindows();
            if (_child == IntPtr.Zero) continue;
            if (tick % 5 == 0 && GetParent(_child) != host)
            {
                try { Capture(host); _log("[pob-embed] re-captured PoB2 window"); }
                catch { }
            }
        }
    }

    /// <summary>Keep every visible top-level PoB window that isn't our
    /// captured child OFF the desktop (boot console — which PoB later
    /// RETITLES to "Path of Building (PoE2)" and re-shows — plus any other
    /// strays). Never captured: parenting the boot console fuses input
    /// queues while PoB's thread is boot-busy (the freeze). Safety rule: a
    /// window carrying the main title is only hidden while we HOLD a live
    /// captured main, so the window we're about to capture can't be hidden.
    /// PoB's modal dialogs are owned windows (GetParent != 0) — exempt.</summary>
    private void HideStrayWindows()
    {
        if (_proc is not { HasExited: false }) return;
        uint pid;
        try { pid = (uint)_proc.Id; } catch { return; }
        var holdingMain = _child != IntPtr.Zero && IsWindow(_child);
        EnumWindows((hwnd, _) =>
        {
            GetWindowThreadProcessId(hwnd, out var wpid);
            if (wpid != pid || hwnd == _child) return true;
            if (!IsWindowVisible(hwnd) || GetParent(hwnd) != IntPtr.Zero) return true;
            var sb = new System.Text.StringBuilder(256);
            GetWindowText(hwnd, sb, sb.Capacity);
            var isMainTitled = sb.ToString().Contains("Path of Building", StringComparison.OrdinalIgnoreCase);
            if (isMainTitled && !holdingMain) return true; // might be our capture target
            ShowWindowAsync(hwnd, SW_HIDE);
            if (_hiddenByUs.Add(hwnd))
                _log($"[pob-embed] stray PoB window hidden: {Describe(hwnd)}");
            return true;
        }, IntPtr.Zero);
    }

    private void UnhideHiddenStrays()
    {
        foreach (var hwnd in _hiddenByUs)
        {
            if (IsWindow(hwnd)) ShowWindowAsync(hwnd, SW_SHOW);
        }
        if (_hiddenByUs.Count > 0)
            _log("[pob-embed] hidden PoB windows restored (no main window — check their output)");
        _hiddenByUs.Clear();
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
        // ASYNC so the WPF UI thread never waits on PoB's message pump.
        SetWindowPos(_child, IntPtr.Zero, 0, 0, w, h,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);
    }

    protected override void DestroyWindowCore(HandleRef hwnd)
    {
        try
        {
            if (_proc is { HasExited: false })
            {
                _proc.Kill(entireProcessTree: true);
            }
        }
        catch { /* shutdown best-effort */ }
        DestroyWindow(hwnd.Handle);
    }

    /// <summary>Strip frame styles, parent into the host, force a frame
    /// recalc, and size to the pane. Called only when PoB is past boot and
    /// pumping messages (we capture nothing before its main window exists).</summary>
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
        Dispatcher.BeginInvoke(ResizeChild);
    }

    /// <summary>Single window matcher. visibleOnly:true for ALL capture
    /// paths (invisible helpers = the v6-v8 dead-pane bug); false only for
    /// diagnostics/unhide. titleContains filters by window title.</summary>
    private static IntPtr FindWindowForPid(uint pid, string? titleContains, bool visibleOnly)
    {
        IntPtr found = IntPtr.Zero;
        EnumWindows((hwnd, _) =>
        {
            GetWindowThreadProcessId(hwnd, out var wpid);
            if (wpid != pid) return true;
            if (visibleOnly && !IsWindowVisible(hwnd)) return true;
            if (GetParent(hwnd) != IntPtr.Zero) return true;
            if (titleContains != null)
            {
                var sb = new System.Text.StringBuilder(256);
                GetWindowText(hwnd, sb, sb.Capacity);
                if (!sb.ToString().Contains(titleContains, StringComparison.OrdinalIgnoreCase)) return true;
            }
            found = hwnd;
            return false; // stop
        }, IntPtr.Zero);
        return found;
    }

    /// <summary>Title fallback that skips windows alive before our launch,
    /// so it can never steal the user's own PoB session.</summary>
    private IntPtr FindNewWindowByTitle(string needle)
    {
        IntPtr found = IntPtr.Zero;
        EnumWindows((hwnd, _) =>
        {
            if (_preexisting.Contains(hwnd)) return true;
            if (!IsWindowVisible(hwnd) || GetParent(hwnd) != IntPtr.Zero) return true;
            var sb = new System.Text.StringBuilder(256);
            GetWindowText(hwnd, sb, sb.Capacity);
            if (!sb.ToString().Contains(needle, StringComparison.OrdinalIgnoreCase)) return true;
            found = hwnd;
            return false;
        }, IntPtr.Zero);
        return found;
    }

    private static IEnumerable<IntPtr> TitledTopLevels(string needle)
    {
        var list = new List<IntPtr>();
        EnumWindows((hwnd, _) =>
        {
            if (GetParent(hwnd) != IntPtr.Zero) return true;
            var sb = new System.Text.StringBuilder(256);
            GetWindowText(hwnd, sb, sb.Capacity);
            if (sb.ToString().Contains(needle, StringComparison.OrdinalIgnoreCase)) list.Add(hwnd);
            return true;
        }, IntPtr.Zero);
        return list;
    }

    /// <summary>Diagnostics: every top-level window of the PoB pid (any
    /// visibility) with title/visible/size — readable in the UI log.</summary>
    private string DumpPidWindows()
    {
        if (_proc == null) return "(no proc)";
        uint pid;
        try { pid = (uint)_proc.Id; } catch { return "(pid unavailable)"; }
        var list = new List<string>();
        EnumWindows((hwnd, _) =>
        {
            GetWindowThreadProcessId(hwnd, out var wpid);
            if (wpid == pid && GetParent(hwnd) == IntPtr.Zero) list.Add(Describe(hwnd));
            return true;
        }, IntPtr.Zero);
        return list.Count == 0 ? "(none)" : string.Join(" | ", list);
    }

    private static string Describe(IntPtr hwnd)
    {
        var sb = new System.Text.StringBuilder(256);
        GetWindowText(hwnd, sb, sb.Capacity);
        GetWindowRect(hwnd, out var r);
        return $"hwnd=0x{hwnd.ToInt64():X} \"{sb}\" visible={IsWindowVisible(hwnd)} size={r.Right - r.Left}x{r.Bottom - r.Top}";
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
