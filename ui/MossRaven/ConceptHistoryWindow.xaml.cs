using System.Collections.Generic;
using System.Windows;
using System.Windows.Input;

namespace MossRaven;

public partial class ConceptHistoryWindow : Window
{
    public string? Selected { get; private set; }

    public ConceptHistoryWindow(IReadOnlyList<string> history)
    {
        InitializeComponent();
        foreach (var c in history) HistoryList.Items.Add(c);
        if (HistoryList.Items.Count > 0) HistoryList.SelectedIndex = 0;
        Loaded += (_, _) => HistoryList.Focus();
    }

    private void Pick()
    {
        if (HistoryList.SelectedItem is string s)
        {
            Selected = s;
            DialogResult = true;
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
