using System;
using System.Collections.Generic;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using MossRaven.Services;

namespace MossRaven;

/// <summary>
/// Provider + per-tier model management. Edits a working copy of Settings;
/// Save persists and signals the owner to reconnect the engine so the new
/// environment applies immediately.
/// </summary>
public partial class ModelSettingsWindow : Window
{
    private readonly Settings _settings;
    private readonly List<Row> _rows = new();

    /// <summary>True after Save — owner should reconnect the service.</summary>
    public bool Saved { get; private set; }

    private sealed class Row
    {
        public ProviderConfig Cfg = new();
        public TextBox Name = new();
        public TextBox Url = new();
        public TextBox Model = new();
        public TextBox Key = new();
        public CheckBox Tier2 = new();
        public Border Root = new();
    }

    public ModelSettingsWindow(Settings settings)
    {
        InitializeComponent();
        _settings = settings;
        _settings.EnsureDefaultProviders();
        foreach (var p in _settings.Providers)
        {
            AddRow(p);
        }
        RebuildTier1Combo();
        AnthropicKeyBox.Text = _settings.AnthropicApiKey;
        AnthropicModelBox.Text = _settings.AnthropicModel;
        Tier1Combo.SelectionChanged += (_, _) => UpdateAnthropicRowVisibility();
        UpdateAnthropicRowVisibility();
    }

    private static bool IsBuiltin(string name) =>
        name.Trim().ToLowerInvariant() is "cerebras" or "groq" or "gemini";

    private void AddRow(ProviderConfig cfg)
    {
        var row = new Row { Cfg = cfg };

        TextBox Mk(string text, double width)
        {
            var tb = new TextBox
            {
                Text = text,
                Width = width,
                Margin = new Thickness(0, 0, 6, 0),
                VerticalAlignment = VerticalAlignment.Center,
                Background = Brushes.Transparent,
                FontSize = 11.5,
            };
            tb.SetResourceReference(Control.ForegroundProperty, "TextBrush");
            tb.SetResourceReference(Control.BorderBrushProperty, "DimmerBrush");
            return tb;
        }

        row.Name = Mk(cfg.Name, 90);
        row.Url = Mk(cfg.BaseUrl, 230);
        row.Model = Mk(cfg.Model, 150);
        row.Key = Mk(cfg.ApiKey, 130);
        row.Tier2 = new CheckBox
        {
            IsChecked = cfg.EnabledTier2,
            VerticalAlignment = VerticalAlignment.Center,
            ToolTip = "Include in the Tier-2 mutation failover chain (built-ins only)",
            Margin = new Thickness(0, 0, 6, 0),
            IsEnabled = IsBuiltin(cfg.Name),
        };
        // Custom rows can't join the chain — keep the box visibly off.
        if (!IsBuiltin(cfg.Name)) row.Tier2.IsChecked = false;
        row.Name.TextChanged += (_, _) =>
        {
            var b = IsBuiltin(row.Name.Text);
            row.Tier2.IsEnabled = b;
            if (!b) row.Tier2.IsChecked = false;
            RebuildTier1Combo();
        };

        var remove = new Button
        {
            Content = "✕",
            Padding = new Thickness(7, 2, 7, 2),
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(1),
            VerticalAlignment = VerticalAlignment.Center,
            ToolTip = "Remove provider",
        };
        remove.SetResourceReference(Control.ForegroundProperty, "DimBrush");
        remove.SetResourceReference(Control.BorderBrushProperty, "DimmerBrush");
        remove.Click += (_, _) =>
        {
            _rows.Remove(row);
            ProviderPanel.Children.Remove(row.Root);
            RebuildTier1Combo();
        };

        string LabelOf(string s) => s;
        var headerNeeded = ProviderPanel.Children.Count == 0;
        if (headerNeeded)
        {
            var head = new DockPanel { Margin = new Thickness(2, 0, 0, 4), LastChildFill = false };
            foreach (var (label, w) in new[]
                     { ("name", 96.0), ("base url", 236.0), ("model", 156.0), ("api key", 136.0), ("T2 chain", 60.0) })
            {
                var tbk = new TextBlock
                {
                    Text = LabelOf(label),
                    Width = w,
                    FontSize = 10.5,
                };
                tbk.SetResourceReference(TextBlock.ForegroundProperty, "DimmerBrush");
                head.Children.Add(tbk);
            }
            ProviderPanel.Children.Add(head);
        }

        var panel = new DockPanel { LastChildFill = false };
        panel.Children.Add(row.Name);
        panel.Children.Add(row.Url);
        panel.Children.Add(row.Model);
        panel.Children.Add(row.Key);
        panel.Children.Add(row.Tier2);
        panel.Children.Add(remove);

        row.Root = new Border
        {
            Child = panel,
            Padding = new Thickness(2, 4, 2, 4),
        };
        ProviderPanel.Children.Add(row.Root);
        _rows.Add(row);
    }

    private void RebuildTier1Combo()
    {
        var current = (Tier1Combo.SelectedItem as string) ?? _settings.Tier1Provider;
        Tier1Combo.Items.Clear();
        Tier1Combo.Items.Add("(automatic — env priority)");
        Tier1Combo.Items.Add("anthropic");
        foreach (var r in _rows)
        {
            var n = r.Name.Text.Trim();
            if (n.Length > 0) Tier1Combo.Items.Add(n);
        }
        var want = string.IsNullOrEmpty(current) ? "(automatic — env priority)" : current;
        Tier1Combo.SelectedItem = Tier1Combo.Items.Cast<string>()
            .FirstOrDefault(x => string.Equals(x, want, StringComparison.OrdinalIgnoreCase))
            ?? "(automatic — env priority)";
    }

    private void UpdateAnthropicRowVisibility()
    {
        AnthropicRow.Visibility =
            string.Equals(Tier1Combo.SelectedItem as string, "anthropic", StringComparison.OrdinalIgnoreCase)
                ? Visibility.Visible
                : Visibility.Collapsed;
    }

    private void AddProvider_Click(object sender, RoutedEventArgs e)
    {
        AddRow(new ProviderConfig
        {
            Name = "custom",
            BaseUrl = "http://localhost:11434/v1",
            Model = "qwen2.5:14b-instruct",
            ApiKey = "",
            EnabledTier2 = false,
        });
        RebuildTier1Combo();
        StatusText.Text = "custom rows drive Tier 1/5 only (the Tier-2 chain knows the three built-ins)";
    }

    private void Save_Click(object sender, RoutedEventArgs e)
    {
        _settings.Providers = _rows
            .Where(r => !string.IsNullOrWhiteSpace(r.Name.Text))
            .Select(r => new ProviderConfig
            {
                Name = r.Name.Text.Trim(),
                BaseUrl = r.Url.Text.Trim(),
                Model = r.Model.Text.Trim(),
                ApiKey = r.Key.Text.Trim(),
                EnabledTier2 = r.Tier2.IsChecked == true && IsBuiltin(r.Name.Text),
            })
            .ToList();
        var sel = Tier1Combo.SelectedItem as string ?? "";
        _settings.Tier1Provider = sel.StartsWith("(automatic") ? "" : sel;
        _settings.AnthropicApiKey = AnthropicKeyBox.Text.Trim();
        _settings.AnthropicModel = AnthropicModelBox.Text.Trim();
        SettingsService.Save(_settings);
        Saved = true;
        Close();
    }

    private void Cancel_Click(object sender, RoutedEventArgs e) => Close();
}
