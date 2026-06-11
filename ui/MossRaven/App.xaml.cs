using System;
using System.Threading;
using System.Windows;

namespace MossRaven;

/// <summary>
/// Single-instance guard: a second launch signals the first (which restores
/// from the tray) and exits — so taskbar/shortcut/startup can never stack
/// instances or duplicate churn schedulers.
/// </summary>
public partial class App : Application
{
    private static Mutex? _instanceMutex;
    private static EventWaitHandle? _showSignal;

    protected override void OnStartup(StartupEventArgs e)
    {
        const string mutexName = "MossRaven_SingleInstance";
        const string signalName = "MossRaven_ShowMe";
        _instanceMutex = new Mutex(initiallyOwned: true, mutexName, out var isNew);
        _showSignal = new EventWaitHandle(false, EventResetMode.AutoReset, signalName);
        if (!isNew)
        {
            // Another instance owns the app — wake it and bow out.
            _showSignal.Set();
            Shutdown();
            return;
        }
        // First instance: listen for wake signals from later launches.
        var listener = new Thread(() =>
        {
            while (_showSignal.WaitOne())
            {
                Current?.Dispatcher.BeginInvoke(() =>
                {
                    if (Current?.MainWindow is MainWindow mw)
                    {
                        mw.ShowFromInstanceSignal();
                    }
                });
            }
        })
        { IsBackground = true };
        listener.Start();
        base.OnStartup(e);
    }
}
