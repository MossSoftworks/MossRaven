using System;
using System.Collections.Generic;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Documents;
using System.Windows.Media;

namespace MossRaven;

/// <summary>
/// Read-only viewer for one persisted finalist: title/stats header, the
/// SPEC §1.1 guide sections (Why / Leveling / Endgame / Loadout swap /
/// Playtest notes), and a copy-import-code action. Opened from the Tier-5
/// history list; plain window (no auto-close-on-deactivate — it's a reading
/// surface the user keeps open next to PoB).
/// </summary>
public partial class FinalistDetailWindow : Window
{
    private readonly string _pobImportCode;

    public FinalistDetailWindow(
        string title,
        string oneLiner,
        IReadOnlyList<string> tags,
        string statsLine,
        IReadOnlyList<(string Heading, string Body)> sections,
        string pobImportCode)
    {
        InitializeComponent();
        _pobImportCode = pobImportCode ?? "";

        Title = title;
        TitleText.Text = title;
        OneLinerText.Text = oneLiner;
        TagsText.Text = tags is { Count: > 0 } ? string.Join("  ·  ", tags) : "";
        StatsText.Text = statsLine;

        foreach (var (heading, body) in sections)
        {
            if (string.IsNullOrWhiteSpace(body)) continue;
            var h = new TextBlock
            {
                Text = heading,
                FontSize = 14,
                FontWeight = FontWeights.SemiBold,
                Margin = new Thickness(0, 10, 0, 4),
            };
            h.SetResourceReference(TextBlock.ForegroundProperty, "MossBrush");
            GuidePanel.Children.Add(h);

            var b = new TextBlock
            {
                Text = body,
                FontSize = 12.5,
                TextWrapping = TextWrapping.Wrap,
                LineHeight = 19,
            };
            b.SetResourceReference(TextBlock.ForegroundProperty, "TextBrush");
            GuidePanel.Children.Add(b);
        }

        CopyCodeButton.IsEnabled = _pobImportCode.Length > 0;
        if (_pobImportCode.Length == 0)
        {
            CopyHint.Text = "no import code stored for this finalist";
        }
    }

    private void CopyCodeButton_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            Clipboard.SetText(_pobImportCode);
            CopyHint.Text = $"✓ copied {_pobImportCode.Length:N0} chars — PoB2 → Import → Import from code";
        }
        catch (Exception ex)
        {
            CopyHint.Text = $"copy failed: {ex.Message}";
        }
    }

    private void CloseButton_Click(object sender, RoutedEventArgs e) => Close();
}
