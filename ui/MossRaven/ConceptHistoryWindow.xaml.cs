using System.Collections.Generic;
using System.Windows;
using System.Windows.Input;

namespace MossRaven;

public partial class ConceptHistoryWindow : Window
{
    public string? Selected { get; private set; }

    /// <summary>Raised when the user picks a concept (double-click, Enter, or Use selected button).</summary>
    public event System.EventHandler<string>? ConceptPicked;

    public ConceptHistoryWindow(IReadOnlyList<string> history)
    {
        InitializeComponent();
        foreach (var c in history) HistoryList.Items.Add(c);
        if (HistoryList.Items.Count > 0) HistoryList.SelectedIndex = 0;
        Loaded += (_, _) => HistoryList.Focus();
        // Click-outside-to-close: when the user clicks the main window (or
        // anything else), this dialog loses activation and closes itself.
        Deactivated += (_, _) =>
        {
            // Guard the case where DialogResult was just set (we're closing
            // ourselves via Pick) so Close doesn't double-fire.
            if (IsLoaded) Close();
        };
    }

    private void Pick()
    {
        if (HistoryList.SelectedItem is string s)
        {
            Selected = s;
            ConceptPicked?.Invoke(this, s);
            // DialogResult only matters in ShowDialog mode; harmless in Show().
            try { DialogResult = true; } catch { }
            Close();
        }
    }

    private void HistoryList_MouseDoubleClick(object sender, System.Windows.Input.MouseButtonEventArgs e) => Pick();

    private void HistoryList_KeyDown(object sender, KeyEventArgs e)
    {
        switch (e.Key)
        {
            case Key.Enter:
                Pick();
                e.Handled = true;
                break;
            case Key.Escape:
                Close();
                e.Handled = true;
                break;
        }
    }

    private void BtnUseSelected_Click(object sender, RoutedEventArgs e) => Pick();
    private void BtnClose_Click(object sender, RoutedEventArgs e) => Close();
}
