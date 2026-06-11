using System;
using System.IO;
using System.Linq;
using System.Windows;

namespace MossRaven;

/// <summary>
/// Tray + startup behavior (for unattended churn): close hides to the tray,
/// the tray icon's right-click menu is the only exit, optional launch-with-
/// Windows (HKCU Run) and start-minimized.
/// </summary>
public partial class MainWindow
{
    private System.Windows.Forms.NotifyIcon? _tray;
    private bool _reallyExit;

    private void InitTray()
    {
        _tray = new System.Windows.Forms.NotifyIcon
        {
            Text = "MossRaven",
            Visible = true,
        };
        try
        {
            var exe = Environment.ProcessPath ?? "";
            _tray.Icon = System.Drawing.Icon.ExtractAssociatedIcon(exe)
                         ?? System.Drawing.SystemIcons.Application;
        }
        catch
        {
            _tray.Icon = System.Drawing.SystemIcons.Application;
        }
        var menu = new System.Windows.Forms.ContextMenuStrip();
        menu.Items.Add("Open MossRaven", null, (_, _) => RestoreFromTray());
        menu.Items.Add("Start corpus churn", null, (_, _) => Dispatcher.Invoke(() =>
        {
            if (_churnProc is not { HasExited: false }) OpsChurnButton_Click(this, new RoutedEventArgs());
        }));
        menu.Items.Add("Stop corpus churn", null, (_, _) => Dispatcher.Invoke(() =>
        {
            if (_churnProc is { HasExited: false }) OpsChurnButton_Click(this, new RoutedEventArgs());
        }));
        menu.Items.Add(new System.Windows.Forms.ToolStripSeparator());
        menu.Items.Add("Exit", null, (_, _) =>
        {
            _reallyExit = true;
            Dispatcher.Invoke(Close);
        });
        _tray.ContextMenuStrip = menu;
        _tray.DoubleClick += (_, _) => RestoreFromTray();

        Closing += (_, e) =>
        {
            if (_settings.CloseToTray && !_reallyExit)
            {
                e.Cancel = true;
                Hide();
                _tray!.BalloonTipTitle = "MossRaven";
                _tray.BalloonTipText = "Still running (churn keeps going). Right-click the tray icon to exit.";
                _tray.ShowBalloonTip(2500);
            }
            else
            {
                _tray!.Visible = false;
                _tray.Dispose();
            }
        };

        ApplyStartupRegistration();
        if (_settings.StartMinimized
            || Environment.GetCommandLineArgs().Contains("--minimized"))
        {
            Hide();
        }
    }

    private void RestoreFromTray()
    {
        Dispatcher.Invoke(() =>
        {
            Show();
            WindowState = WindowState.Normal;
            Activate();
        });
    }

    /// <summary>HKCU\...\Run registration matching the LaunchAtStartup setting.</summary>
    private void ApplyStartupRegistration()
    {
        try
        {
            using var key = Microsoft.Win32.Registry.CurrentUser.OpenSubKey(
                @"Software\Microsoft\Windows\CurrentVersion\Run", writable: true);
            if (key == null) return;
            if (_settings.LaunchAtStartup)
            {
                var exe = Environment.ProcessPath ?? "";
                if (exe.Length > 0)
                    key.SetValue("MossRaven", $"\"{exe}\" --minimized");
            }
            else if (key.GetValue("MossRaven") != null)
            {
                key.DeleteValue("MossRaven", throwOnMissingValue: false);
            }
        }
        catch (Exception ex)
        {
            AppendLog($"[startup] Run-key update failed: {ex.Message}");
        }
    }
}
