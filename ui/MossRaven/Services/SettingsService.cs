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

    /// <summary>
    /// LLM provider configurations. The three built-ins (cerebras/groq/gemini)
    /// map onto the service's Tier-2 failover-chain env contract; custom
    /// entries are selectable for Tier-1/5 only (the chain env contract only
    /// knows the built-ins). Keys live HERE (user-local settings.json), and
    /// are injected into the spawned service's environment — never written
    /// to the repo.
    /// </summary>
    public List<ProviderConfig> Providers { get; set; } = new();

    /// <summary>
    /// Which provider drives Tier-1 (hypothesis) + Tier-5 (guides).
    /// "" = automatic (env-driven: Anthropic key → Gemini → Groq → Mode B);
    /// "anthropic" = the Anthropic fields below; otherwise the Name of an
    /// entry in <see cref="Providers"/>.
    /// </summary>
    public string Tier1Provider { get; set; } = "";

    public string AnthropicApiKey { get; set; } = "";
    public string AnthropicModel { get; set; } = "claude-sonnet-4-5";

    /// <summary>Path to the desktop PoB2 executable (workspace's "Open in PoB2").</summary>
    public string PobInstallPath { get; set; } = "";
    /// <summary>poe.ninja live unique prices (MOSSRAVEN_NINJA=1).</summary>
    public bool NinjaEnabled { get; set; } = false;
    /// <summary>Override route template for the ninja item overview.</summary>
    public string NinjaItemUrl { get; set; } = "";
    /// <summary>Tier-3 training corpus logging (on by default).</summary>
    public bool CorpusEnabled { get; set; } = true;

    /// <summary>Auto-embed/launch PoB2 on app start (downloads once if absent).</summary>
    public bool AutoEmbedPob { get; set; } = true;
    /// <summary>Ops scheduling (HH:mm, blank = manual): churn window + daily runs.</summary>
    public string ChurnStartAt { get; set; } = "";
    public string ChurnStopAt { get; set; } = "";
    public string RescoreAt { get; set; } = "";
    public string TrainAt { get; set; } = "";

    /// <summary>Register in HKCU Run so MossRaven starts with Windows (for churn).</summary>
    public bool LaunchAtStartup { get; set; } = false;
    /// <summary>Start hidden in the tray (pairs with LaunchAtStartup).</summary>
    public bool StartMinimized { get; set; } = false;
    /// <summary>Close button hides to tray; exit only via tray right-click.</summary>
    public bool CloseToTray { get; set; } = true;

    public const int HistoryCap = 50;

    /// <summary>First-run defaults: the three built-ins, keys empty (env fallback).</summary>
    public void EnsureDefaultProviders()
    {
        if (Providers.Count > 0) return;
        Providers.Add(new ProviderConfig
        {
            Name = "cerebras",
            BaseUrl = "https://api.cerebras.ai/v1",
            Model = "gpt-oss-120b",
            ApiKey = "",
            EnabledTier2 = true,
        });
        Providers.Add(new ProviderConfig
        {
            Name = "groq",
            BaseUrl = "https://api.groq.com/openai/v1",
            Model = "llama-3.3-70b-versatile",
            ApiKey = "",
            EnabledTier2 = true,
        });
        Providers.Add(new ProviderConfig
        {
            Name = "gemini",
            BaseUrl = "https://generativelanguage.googleapis.com/v1beta/openai",
            Model = "gemini-2.5-flash-lite",
            ApiKey = "",
            EnabledTier2 = true,
        });
    }

    /// <summary>
    /// Environment overrides for the spawned service, expressing this
    /// settings object through the service's existing env contract.
    /// Empty-string values deliberately BLANK a var (the service treats
    /// empty as unset) so disabling a built-in here beats a machine-level
    /// setx key.
    /// </summary>
    public Dictionary<string, string> ToServiceEnvironment()
    {
        var env = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);

        foreach (var p in Providers)
        {
            var name = (p.Name ?? "").Trim().ToLowerInvariant();
            if (name is not ("cerebras" or "groq" or "gemini")) continue; // custom = Tier-1-only
            var prefix = name.ToUpperInvariant();
            // Key: explicit value, or BLANK to drop a setx-level key when the
            // provider is disabled for the Tier-2 chain.
            env[$"{prefix}_API_KEY"] = p.EnabledTier2 ? (p.ApiKey ?? "") : "";
            if (p.EnabledTier2 && !string.IsNullOrWhiteSpace(p.Model))
                env[$"{prefix}_MODEL"] = p.Model;
            if (name == "cerebras" && p.EnabledTier2 && !string.IsNullOrWhiteSpace(p.BaseUrl))
                env["CEREBRAS_BASE_URL"] = p.BaseUrl;
        }

        switch ((Tier1Provider ?? "").Trim().ToLowerInvariant())
        {
            case "":
                break; // automatic — leave the service's own priority chain alone
            case "anthropic":
                env["MOSSRAVEN_ANTHROPIC_API_KEY"] = AnthropicApiKey ?? "";
                if (!string.IsNullOrWhiteSpace(AnthropicModel))
                    env["MOSSRAVEN_ANTHROPIC_MODEL"] = AnthropicModel;
                break;
            default:
                var sel = Providers.Find(x =>
                    string.Equals(x.Name, Tier1Provider, StringComparison.OrdinalIgnoreCase));
                if (sel != null)
                {
                    // Explicit T1 selection must WIN over the automatic chain:
                    // blank the Anthropic var and pin the OpenAI-compat trio.
                    env["MOSSRAVEN_ANTHROPIC_API_KEY"] = "";
                    env["MOSSRAVEN_T1_BASE_URL"] = sel.BaseUrl ?? "";
                    env["MOSSRAVEN_T1_MODEL"] = sel.Model ?? "";
                    env["MOSSRAVEN_T1_API_KEY"] = sel.ApiKey ?? "";
                }
                break;
        }
        // Ops + grounding toggles (SPEC 3.7 / 1.1.2).
        env["MOSSRAVEN_NINJA"] = NinjaEnabled ? "1" : "";
        if (!string.IsNullOrWhiteSpace(NinjaItemUrl))
            env["MOSSRAVEN_NINJA_ITEM_URL"] = NinjaItemUrl;
        if (!CorpusEnabled)
            env["MOSSRAVEN_CORPUS"] = "0";

        return env;
    }
}

public sealed class ProviderConfig
{
    public string Name { get; set; } = "";
    public string BaseUrl { get; set; } = "";
    public string Model { get; set; } = "";
    public string ApiKey { get; set; } = "";
    /// <summary>Included in the Tier-2 failover chain (built-ins only).</summary>
    public bool EnabledTier2 { get; set; } = true;
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
