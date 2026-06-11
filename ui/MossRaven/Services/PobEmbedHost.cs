using System;
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
            _proc = Process.Start(new ProcessStartInfo
            {
                FileName = _exePath,
                WorkingDirectory = System.IO.Path.GetDirectoryName(_exePath) ?? ".",
                UseShellExecute = true,
                // Hidden from birth: the splash and main window are created
                // invisible, captured invisibly, and only ever SHOWN inside
                // our pane — zero pop-ups, zero flicker by construction.
                WindowStyle = ProcessWindowStyle.Hidden,
            });
            if (_proc == null)
            {
                _log("[pob-embed] process start returned null");
                return;
            }
            // PoB builds its window asynchronously — poll for a usable
            // top-level window owned by the process (MainWindowHandle is
            // unreliable for SimpleGraphic; enumerate by PID instead).
            for (int i = 0; i < 150 && _child == IntPtr.Zero; i++)
            {
                await Task.Delay(200);
                if (_proc.HasExited && _child == IntPtr.Zero && i < 30)
                {
                    // Squirrel-style stubs exit after spawning the real app —
                    // fall through to the title search instead of bailing.
                    _log("[pob-embed] launcher exited early — searching by window title");
                }
                // Search INCLUDING invisible windows (we launch hidden);
                // by pid first, then by title (covers child-process spawns).
                var cand = FindWindowForPid((uint)_proc.Id, includeHidden: true, appSizedOnly: true);
                if (cand == IntPtr.Zero && i >= 10)
                    cand = FindWindowByTitle("Path of Building", includeHidden: true, appSizedOnly: true);
                // Any small window (splash) stays hidden; never shown at all.
                var splash = FindWindowForPid((uint)_proc.Id, includeHidden: false, appSizedOnly: false);
                if (splash != IntPtr.Zero && !IsAppSized(splash))
                    ShowWindow(splash, SW_HIDE);
                if (cand != IntPtr.Zero)
                    _child = cand;
            }
            if (_child == IntPtr.Zero)
            {
                _log("[pob-embed] no PoB2 window found to embed (still runs standalone)");
                return;
            }
            Capture(host);
            _log("[pob-embed] PoB2 embedded");
            // Watchdog: SimpleGraphic re-applies its own styles on some
            // events (display-mode changes) and can pop back to top-level —
            // re-capture for the lifetime of the host.
            _ = Task.Run(async () =>
            {
                while (_proc is { HasExited: false })
                {
                    await Task.Delay(1500);
                    if (_child != IntPtr.Zero && !IsWindow(_child))
                    {
                        // Splash/main handoff or PoB recreated its window —
                        // find the new app-sized one and re-capture.
                        _child = IntPtr.Zero;
                        for (int j = 0; j < 75 && _child == IntPtr.Zero; j++)
                        {
                            await Task.Delay(200);
                            var c2 = FindWindowByTitle("Path of Building", includeHidden: true, appSizedOnly: true);
                            if (c2 != IntPtr.Zero) _child = c2;
                        }
                        if (_child != IntPtr.Zero)
                        {
                            try { Capture(host); _log("[pob-embed] main window captured after splash"); }
                            catch { }
                        }
                        continue;
                    }
                    if (_child == IntPtr.Zero) continue;
                    if (GetParent(_child) != host)
                    {
                        try { Capture(host); _log("[pob-embed] re-captured PoB2 window"); }
                        catch { }
                    }
                }
            });
        }
        catch (Exception ex)
        {
            _log($"[pob-embed] {ex.Message}");
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
        MoveWindow(_child, 0, 0, w, h, true);
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
    /// recalc, and size to the pane.</summary>
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
            SWP_NOSIZE | SWP_NOMOVE | SWP_NOZORDER | SWP_FRAMECHANGED);
        ShowWindow(_child, SW_SHOWNA); // first time it becomes visible: inside the pane
        Dispatcher.Invoke(ResizeChild);
    }

    private static bool IsAppSized(IntPtr hwnd)
    {
        if (!GetWindowRect(hwnd, out var r)) return false;
        return (r.Right - r.Left) >= 700 && (r.Bottom - r.Top) >= 480;
    }

    private static IntPtr FindWindowByTitle(string needle, bool includeHidden, bool appSizedOnly)
    {
        IntPtr found = IntPtr.Zero;
        EnumWindows((hwnd, _) =>
        {
            if (!includeHidden && !IsWindowVisible(hwnd)) return true;
            if (GetParent(hwnd) != IntPtr.Zero) return true;
            var sb = new System.Text.StringBuilder(256);
            GetWindowText(hwnd, sb, sb.Capacity);
            if (!sb.ToString().Contains(needle, StringComparison.OrdinalIgnoreCase)) return true;
            if (appSizedOnly && !IsAppSized(hwnd)) return true;
            found = hwnd;
            return false;
        }, IntPtr.Zero);
        return found;
    }

    private static IntPtr FindWindowForPid(uint pid, bool includeHidden, bool appSizedOnly)
    {
        IntPtr found = IntPtr.Zero;
        EnumWindows((hwnd, _) =>
        {
            GetWindowThreadProcessId(hwnd, out var wpid);
            if (wpid != pid) return true;
            if (!includeHidden && !IsWindowVisible(hwnd)) return true;
            if (GetParent(hwnd) != IntPtr.Zero) return true;
            if (appSizedOnly && !IsAppSized(hwnd)) return true;
            found = hwnd;
            return false; // stop
        }, IntPtr.Zero);
        return found;
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
    private const uint SWP_FRAMECHANGED = 0x0020;

    private delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lparam);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lparam);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] private static extern IntPtr GetParent(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int w, int h, uint flags);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetWindowText(IntPtr hwnd, System.Text.StringBuilder text, int count);
    [DllImport("user32.dll")] private static extern bool IsWindow(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern bool ShowWindow(IntPtr hwnd, int cmd);
    [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    private const int SW_HIDE = 0;
    private const int SW_SHOWNA = 8;
    [StructLayout(LayoutKind.Sequential)]
    private struct RECT { public int Left, Top, Right, Bottom; }

    private static int W32(long v) => unchecked((int)v);

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
