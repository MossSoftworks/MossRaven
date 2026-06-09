using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.Json;

namespace MossRaven.Services;

/// <summary>
/// Persisted user state for the MossRaven shell.
///
/// Stored at <c>%APPDATA%\MossRaven\settings.json</c>. Written on Save();
/// read once at startup via Load(). Survives crashes (write is atomic via
/// .tmp + File.Move).
/// </summary>
public sealed class Settings
{
    /// <summary>Last concept text the user typed. Restored into ConceptInput on next launch.</summary>
    public string LastConcept { get; set; } = "";

    /// <summary>Most-recent-first list of concepts the user has seeded. Capped at <see cref="HistoryCap"/>.</summary>
    public List<string> History { get; set; } = new();

    public const int HistoryCap = 50;
}

public static class SettingsService
{
    private static readonly string Dir = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
        "MossRaven");
    private static readonly string FilePath = Path.Combine(Dir, "settings.json");

    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
    };

    public static Settings Load()
    {
        try
        {
            // One-time migration: if there's no new-location settings yet, but an
            // old MossOrb-era settings file exists, copy it over so the user
            // doesn't lose their concept history across the rename.
            if (!File.Exists(FilePath))
            {
                var legacy = Path.Combine(
                    Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                    "MossOrb",
                    "settings.json");
                if (File.Exists(legacy))
                {
                    try
                    {
                        Directory.CreateDirectory(Dir);
                        File.Copy(legacy, FilePath);
                    }
                    catch { /* migration is best-effort */ }
                }
            }
            if (!File.Exists(FilePath)) return new Settings();
            var json = File.ReadAllText(FilePath);
            return JsonSerializer.Deserialize<Settings>(json, JsonOpts) ?? new Settings();
        }
        catch
        {
            // Corrupt or unreadable — start fresh. Don't crash the shell over user state.
            return new Settings();
        }
    }

    public static void Save(Settings s)
    {
        try
        {
            Directory.CreateDirectory(Dir);
            var tmp = FilePath + ".tmp";
            var json = JsonSerializer.Serialize(s, JsonOpts);
            File.WriteAllText(tmp, json);
            // Atomic-ish replace
            if (File.Exists(FilePath)) File.Delete(FilePath);
            File.Move(tmp, FilePath);
        }
        catch
        {
            // Persistence is best-effort. Losing the last-concept on a crash is OK.
        }
    }

    /// <summary>Append a concept to history, deduping by content and capping to <see cref="Settings.HistoryCap"/>.</summary>
    public static void AppendHistory(Settings s, string concept)
    {
        if (string.IsNullOrWhiteSpace(concept)) return;
        concept = concept.Trim();
        // Remove any earlier copy so the new entry floats to the top.
        s.History = s.History.Where(x => !string.Equals(x, concept, StringComparison.Ordinal)).ToList();
        s.History.Insert(0, concept);
        if (s.History.Count > Settings.HistoryCap)
        {
            s.History = s.History.Take(Settings.HistoryCap).ToList();
        }
    }
}
