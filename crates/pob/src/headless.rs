//! Path of Building 2 headless integration.
//!
//! Provides a Rust interface to Path of Building 2's
//! calculation engine via embedded Lua.

use mlua::{Lua, Result as LuaResult};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Lua table field-extraction macros
// ---------------------------------------------------------------------------

/// Extract a typed field from a Lua table, returning the default on failure.
///
/// ```ignore
/// lua_get!(table, "name" => String)          // unwrap_or_default()
/// lua_get!(table, "quality" => i64, 0)       // unwrap_or(0)
/// ```
macro_rules! lua_get {
    ($table:expr, $field:expr => $ty:ty) => {
        $table.get::<$ty>($field).unwrap_or_default()
    };
    ($table:expr, $field:expr => $ty:ty, $default:expr) => {
        $table.get::<$ty>($field).unwrap_or($default)
    };
}

/// Insert a single Lua table field into a [`serde_json::Map`], skipping on failure.
///
/// Handles type-specific conversion. Supports key renaming with `=>` syntax.
///
/// ```ignore
/// lua_json_insert!(map, table, "name", String);
/// lua_json_insert!(map, table, "displayLabel" => "label", String);
/// ```
macro_rules! lua_json_insert {
    ($map:expr, $table:expr, $field:expr, $ty:ident) => {
        lua_json_insert!(@do $map, $table, $field, $field, $ty)
    };
    ($map:expr, $table:expr, $lua_key:expr => $json_key:expr, $ty:ident) => {
        lua_json_insert!(@do $map, $table, $lua_key, $json_key, $ty)
    };
    (@do $map:expr, $table:expr, $lua_key:expr, $json_key:expr, String) => {
        if let Ok(v) = $table.get::<String>($lua_key) {
            $map.insert($json_key.to_owned(), serde_json::Value::String(v));
        }
    };
    (@do $map:expr, $table:expr, $lua_key:expr, $json_key:expr, f64) => {
        if let Ok(v) = $table.get::<f64>($lua_key) {
            if let Some(n) = serde_json::Number::from_f64(v) {
                $map.insert($json_key.to_owned(), serde_json::Value::Number(n));
            }
        }
    };
    (@do $map:expr, $table:expr, $lua_key:expr, $json_key:expr, i64) => {
        if let Ok(v) = $table.get::<i64>($lua_key) {
            $map.insert($json_key.to_owned(), serde_json::Value::Number(v.into()));
        }
    };
    (@do $map:expr, $table:expr, $lua_key:expr, $json_key:expr, bool) => {
        if let Ok(v) = $table.get::<bool>($lua_key) {
            $map.insert($json_key.to_owned(), serde_json::Value::Bool(v));
        }
    };
}

/// Build a [`serde_json::Map`] from multiple optional Lua table fields.
///
/// Each field specifies its type. Absent fields are skipped.
/// Use tuple syntax for key renaming.
///
/// ```ignore
/// let map = lua_json_map!(table, {
///     "name": String,
///     "dps": f64,
///     ("displayLabel", "label"): String,
/// });
/// ```
macro_rules! lua_json_map {
    ($table:expr, { $( $field:tt : $ty:ident ),* $(,)? }) => {{
        #[allow(unused_mut)]
        let mut map = serde_json::Map::new();
        $(
            lua_json_map!(@field map, $table, $field, $ty);
        )*
        map
    }};
    (@field $map:ident, $table:expr, ($lua_key:expr, $json_key:expr), $ty:ident) => {
        lua_json_insert!($map, $table, $lua_key => $json_key, $ty);
    };
    (@field $map:ident, $table:expr, $field:expr, $ty:ident) => {
        lua_json_insert!($map, $table, $field, $ty);
    };
}

#[derive(Error, Debug)]
pub enum PobError {
    #[error("Lua error: {0}")]
    Lua(#[from] mlua::Error),

    #[error("PoB not initialized")]
    NotInitialized,

    #[error("Calculation failed: {0}")]
    CalculationFailed(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Path of Building headless instance.
pub struct PobHeadless {
    lua: Lua,
    initialized: bool,
    /// PoB `src/` directory — needed as CWD for Lua calls that trigger Build:Init.
    pob_src_path: Option<std::path::PathBuf>,
}

/// Build statistics from PoB calculations.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BuildStats {
    #[serde(rename = "dps")]
    pub total_dps: f64,
    pub effective_hp: f64,
    pub life: f64,
    pub energy_shield: f64,
    pub armour: f64,
    pub evasion: f64,
    pub fire_res: i32,
    pub cold_res: i32,
    pub lightning_res: i32,
    pub chaos_res: i32,
}

impl PobHeadless {
    /// Create a new PoB headless instance.
    pub fn new() -> LuaResult<Self> {
        let lua = Lua::new();
        Ok(Self {
            lua,
            initialized: false,
            pob_src_path: None,
        })
    }

    /// Initialize PoB with the path to PoB2 installation.
    ///
    /// `pob_path` should point to the PoB2 root directory (containing `src/`).
    pub fn init(&mut self, pob_path: &str) -> Result<(), PobError> {
        let pob_path = Path::new(pob_path);
        let pob_src_path = pob_path.join("src");
        let pob_runtime_lua = pob_path.join("runtime/lua");

        // PoB's Lua files use relative dofile() calls, so we must change to src/
        let original_cwd = std::env::current_dir()?;
        std::env::set_current_dir(&pob_src_path)?;

        tracing::info!("Initializing PoB from {:?}", pob_src_path);

        // Set up Lua package.path to include PoB's runtime/lua directory.
        // This is where xml.lua and other dependencies live.
        //
        // MossRaven diff vs. upstream: normalize Windows backslashes to forward
        // slashes before splicing the path into Lua source. Lua 5.4 rejects
        // unknown escapes like `\#` or `\?` (which appear in `C:\#AppProjects\...`
        // and any path followed by `/?.lua`); LuaJIT silently accepts them as
        // literal backslash. Forward-slash paths work on Windows at the C
        // runtime layer, so this is the lossless portable encoding.
        let runtime_lua_path = pob_runtime_lua
            .to_str()
            .ok_or_else(|| PobError::CalculationFailed("Invalid path".to_owned()))?
            .replace('\\', "/");

        self.lua
            .load(format!(
                r#"package.path = package.path .. ";{0}/?.lua;{0}/?/init.lua""#,
                runtime_lua_path
            ))
            .exec()?;

        // Provide a minimal lua-utf8 stub since it's a C module we can't load in safe mode
        // This provides basic string operations that fall back to regular string functions
        self.lua
            .load(
                r#"
                package.preload['lua-utf8'] = function()
                    local utf8 = {}
                    utf8.reverse = string.reverse
                    utf8.gsub = string.gsub
                    utf8.find = string.find
                    utf8.sub = string.sub
                    utf8.match = string.match
                    utf8.len = string.len
                    function utf8.next(s, i, offset)
                        if offset == -1 then
                            return i > 1 and i - 1 or nil
                        else
                            return i < #s and i + 1 or nil
                        end
                    end
                    return utf8
                end
                "#,
            )
            .exec()?;

        // Provide empty arg table (command line arguments)
        self.lua.load("arg = {}").exec()?;

        // CRITICAL: Redirect Lua `print` and PoB's Con* host fns to stderr.
        // PoB2's Launch.lua calls print() and ConPrintf() liberally during
        // init ("Loading main script...", "missing node X", "Startup time: 0 ms",
        // etc.). When mossraven-service runs as an MCP subprocess of the WPF
        // shell or Claude Code, stdout is RESERVED for newline-delimited
        // JSON-RPC framing — any non-JSON line corrupts the client's parser.
        //
        // io.stderr:write is unaffected by the parent's stdout capture, so
        // these messages still surface in the WPF "service status" pane
        // (which tees stderr separately) but stdout stays clean for JSON-RPC.
        self.lua
            .load(
                r#"
                local function to_stderr(...)
                    local parts = {}
                    for i = 1, select('#', ...) do
                        parts[i] = tostring(select(i, ...))
                    end
                    io.stderr:write(table.concat(parts, '\t'), '\n')
                end
                print = to_stderr
                -- PoB's host functions (provided by SimpleGraphic on desktop).
                -- In headless they don't exist; PoB falls back to print, which we
                -- just redirected. Stub them anyway for defence-in-depth.
                ConPrintf  = function(fmt, ...) to_stderr(string.format(fmt, ...)) end
                ConPrintln = function(fmt, ...) to_stderr(string.format(fmt, ...)) end
                ConClear   = function() end
                ConSetColor = function() end
                "#,
            )
            .exec()?;

        // MossRaven diff vs. upstream: LuaJIT → bare-Lua compatibility shim.
        //
        // Upstream poe2-agent uses LuaJIT (Lua 5.1 semantics + JIT). MossRaven
        // runs on Lua 5.1 via mlua's `lua51` + `vendored` features because the
        // LuaJIT MSVC bootstrap fails on Windows hosts with the
        // NoDefaultCurrentDirectoryInExePath=1 security default (release builds
        // for Windows users get real LuaJIT, cross-compiled from Linux — see
        // deploy/release/ for the runbook).
        //
        // PoB2 was written against Lua 5.1, so Lua 5.1 gives us nearly-native
        // compatibility:
        //   - `unpack`, `loadstring`, `setfenv`, `table.maxn` → all native on 5.1
        //   - `string.format("%d", float)` → lenient on 5.1 (5.3+ added integer strictness)
        //   - `bit.*` library → NOT in bare 5.1; LuaJIT shipped it on top.
        //                       Polyfilled here in pure Lua.
        //   - `jit.*` table → LuaJIT-only. Stubbed (single ref at Launch.lua:17).
        //
        // The `bit` polyfill is pure-Lua arithmetic over 32-bit words. Slower
        // than native bitwise but correctness-equivalent for PoB2's uses
        // (passive-tree encoding, base64, item-mod hashing — none in a hot
        // inner loop). If profiling later shows it's a bottleneck, the proper
        // fix is to switch back to LuaJIT (where `bit` is a fast C library)
        // via the Linux cross-compile path, not to expand this shim.
        self.lua
            .load(
                r#"
                -- jit table — single PoB2 reference at Launch.lua:17 (jit.opt.start).
                if jit == nil then
                    jit = {
                        opt    = { start = function(...) end },
                        on     = function(...) end,
                        off    = function(...) end,
                        flush  = function(...) end,
                        status = function() return false end,
                        version_num = 0,
                        version = "stub-no-jit",
                    }
                end

                -- Pure-Lua BitOp polyfill. Operates on 32-bit unsigned values.
                if bit == nil then
                    local floor = math.floor
                    local MASK32 = 4294967296  -- 2^32
                    local function tobit(a)
                        local x = floor(a or 0) % MASK32
                        if x < 0 then x = x + MASK32 end
                        return x
                    end
                    local function band2(a, b)
                        local r, pow = 0, 1
                        for _ = 1, 32 do
                            if a % 2 == 1 and b % 2 == 1 then r = r + pow end
                            a = floor(a / 2); b = floor(b / 2); pow = pow * 2
                        end
                        return r
                    end
                    local function bor2(a, b)
                        local r, pow = 0, 1
                        for _ = 1, 32 do
                            if a % 2 == 1 or b % 2 == 1 then r = r + pow end
                            a = floor(a / 2); b = floor(b / 2); pow = pow * 2
                        end
                        return r
                    end
                    local function bxor2(a, b)
                        local r, pow = 0, 1
                        for _ = 1, 32 do
                            if (a % 2) ~= (b % 2) then r = r + pow end
                            a = floor(a / 2); b = floor(b / 2); pow = pow * 2
                        end
                        return r
                    end
                    local function fold(op, a, b, ...)
                        local r = op(tobit(a), tobit(b))
                        local args = {...}
                        for i = 1, #args do r = op(r, tobit(args[i])) end
                        return r
                    end
                    bit = {
                        tobit   = tobit,
                        band    = function(a, b, ...) return fold(band2, a, b, ...) end,
                        bor     = function(a, b, ...) return fold(bor2,  a, b, ...) end,
                        bxor    = function(a, b, ...) return fold(bxor2, a, b, ...) end,
                        bnot    = function(a) return bxor2(tobit(a), 0xFFFFFFFF) end,
                        lshift  = function(a, n) return floor((tobit(a) * 2^n)) % MASK32 end,
                        rshift  = function(a, n) return floor(tobit(a) / 2^n) end,
                        arshift = function(a, n) return floor(tobit(a) / 2^n) end,  -- no sign extension; PoB uses on unsigned
                        tohex   = function(a, n)
                            n = n or 8
                            local upper = n < 0
                            local width = upper and -n or n
                            local fmt = string.format("%%0%d%s", width, upper and "X" or "x")
                            return string.format(fmt, tobit(a))
                        end,
                    }
                end
                "#,
            )
            .exec()?;

        // Load HeadlessWrapper.lua which bootstraps everything
        let result = self.lua.load("dofile('HeadlessWrapper.lua')").exec();

        // Always restore the original working directory
        std::env::set_current_dir(original_cwd)?;

        result?;

        self.initialized = true;
        self.pob_src_path = Some(pob_src_path.to_owned());
        tracing::info!("PoB headless initialized successfully");
        Ok(())
    }

    /// Load a build from XML export.
    pub fn load_build_xml(&self, xml: &str) -> Result<(), PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        // loadBuildFromXML triggers Build:Init which re-creates all tabs via
        // LoadModule (relative paths).  CWD must be the PoB src/ directory.
        self.with_pob_cwd(|lua| {
            let load_fn: mlua::Function = lua.globals().get("loadBuildFromXML")?;
            load_fn.call::<()>((xml, "imported_build"))
        })?;

        tracing::debug!("Build loaded from XML");
        Ok(())
    }

    /// Calculate build statistics from the currently loaded build.
    pub fn calculate(&self) -> Result<BuildStats, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        // Navigate: build.calcsTab.mainOutput
        let build: mlua::Table = self.lua.globals().get("build")?;
        let calcs_tab: mlua::Table = build.get("calcsTab")?;
        let main_output: mlua::Table = calcs_tab.get("mainOutput")?;

        // For EHP, we need calcsOutput
        let calcs_output: mlua::Table = calcs_tab.get("calcsOutput")?;

        // Extract stats with safe defaults.
        //
        // DPS fallback chain — `TotalDPS` is hit-DPS of the main skill only.
        // Trigger builds (Cast on Critical, Cast on Elemental Ailment — the
        // standard 0.5 Tornado Druid pattern) and DoT builds report
        // `TotalDPS=0` with the real number in `CombinedDPS` (hit+DoT) or
        // `FullDPS` (all FullDPS-flagged skills summed). The old
        // `.or_else()` chain only fired when the KEY was missing; a present-
        // but-zero TotalDPS short-circuited it and every trigger build scored
        // 0. Take the first POSITIVE value instead.
        let dps_candidates = ["TotalDPS", "CombinedDPS", "FullDPS"];
        let total_dps = dps_candidates
            .iter()
            .filter_map(|k| main_output.get::<f64>(*k).ok())
            .find(|v| *v > 0.0)
            .unwrap_or(0.0);

        let life = lua_get!(main_output, "Life" => f64);
        let energy_shield = lua_get!(main_output, "EnergyShield" => f64);
        let armour = lua_get!(main_output, "Armour" => f64);
        let evasion = lua_get!(main_output, "Evasion" => f64);

        let fire_res = lua_get!(main_output, "FireResist" => i32);
        let cold_res = lua_get!(main_output, "ColdResist" => i32);
        let lightning_res = lua_get!(main_output, "LightningResist" => i32);
        let chaos_res = lua_get!(main_output, "ChaosResist" => i32);

        let effective_hp = lua_get!(calcs_output, "PhysicalMaximumHitTaken" => f64);

        Ok(BuildStats {
            total_dps,
            effective_hp,
            life,
            energy_shield,
            armour,
            evasion,
            fire_res,
            cold_res,
            lightning_res,
            chaos_res,
        })
    }

    /// Query extended build stats (~40 fields) grouped by category.
    ///
    /// Reads from `mainOutput` and `calcsOutput`, returning a JSON object
    /// with keys: offense, defense, resources, speed, charges.
    pub fn query_build_stats(&self) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        let build: mlua::Table = self.lua.globals().get("build")?;
        let calcs_tab: mlua::Table = build.get("calcsTab")?;
        let main_output: mlua::Table = calcs_tab.get("mainOutput")?;
        let calcs_output: mlua::Table = calcs_tab.get("calcsOutput")?;

        let offense_fields = &[
            "TotalDPS",
            "CombinedDPS",
            "AverageHit",
            "Speed",
            "CritChance",
            "CritMultiplier",
            "HitChance",
            "TotalDot",
            "BleedDPS",
            "IgniteDPS",
            "PoisonDPS",
            "FullDPS",
            "WithPoisonDPS",
            "WithIgniteDPS",
            "WithBleedDPS",
            "TotalDotDPS",
            "Damage",
            "PhysicalDamage",
            "ElementalDamage",
            "FireDamage",
            "ColdDamage",
            "LightningDamage",
            "ChaosDamage",
        ];

        let defense_fields = &[
            "TotalEHP",
            "PhysicalMaximumHitTaken",
            "FireMaximumHitTaken",
            "ColdMaximumHitTaken",
            "LightningMaximumHitTaken",
            "ChaosMaximumHitTaken",
            "Armour",
            "PhysicalDamageReduction",
            "Evasion",
            "EvadeChance",
            "BlockChance",
            "SpellBlockChance",
            "SpellSuppressionChance",
            "FireResist",
            "ColdResist",
            "LightningResist",
            "ChaosResist",
            "FireResistOverCap",
            "ColdResistOverCap",
            "LightningResistOverCap",
            "ChaosResistOverCap",
        ];

        let resource_fields = &[
            "Life",
            "LifeUnreserved",
            "LifeRegenRecovery",
            "Mana",
            "ManaUnreserved",
            "ManaRegenRecovery",
            "EnergyShield",
            "EnergyShieldRegenRecovery",
            "Spirit",
        ];

        let speed_fields = &[
            "EffectiveMovementSpeedMod",
            "AreaOfEffectRadiusMetres",
            "Duration",
            "ManaCost",
        ];

        let charge_fields = &["PowerChargesMax", "FrenzyChargesMax", "EnduranceChargesMax"];

        let offense = read_fields(&main_output, offense_fields);
        let mut defense = read_fields(&main_output, defense_fields);
        // TotalEHP and MaxHitTaken fields come from calcsOutput
        merge_fields(&mut defense, &read_fields(&calcs_output, defense_fields));
        let resources = read_fields(&main_output, resource_fields);
        let speed = read_fields(&main_output, speed_fields);
        let charges = read_fields(&main_output, charge_fields);

        Ok(serde_json::json!({
            "offense": offense,
            "defense": defense,
            "resources": resources,
            "speed": speed,
            "charges": charges,
        }))
    }

    /// Query the list of skills with their DPS and gem links.
    ///
    /// Reads from `SkillDPS` array in `mainOutput` and
    /// `socketGroupList` in `skillsTab`.
    pub fn query_skill_list(&self) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        let build: mlua::Table = self.lua.globals().get("build")?;
        let calcs_tab: mlua::Table = build.get("calcsTab")?;
        let main_output: mlua::Table = calcs_tab.get("mainOutput")?;

        // Read SkillDPS array from mainOutput
        let mut skill_dps_list = Vec::new();
        if let Ok(skill_dps_table) = main_output.get::<mlua::Table>("SkillDPS") {
            for i in 1..=skill_dps_table.raw_len() {
                if let Ok(entry) = skill_dps_table.get::<mlua::Table>(i) {
                    let skill = lua_json_map!(entry, {
                        "name": String,
                        "dps": f64,
                        "count": i64,
                        "trigger": String,
                        "skillPart": String,
                    });
                    if !skill.is_empty() {
                        skill_dps_list.push(serde_json::Value::Object(skill));
                    }
                }
            }
        }

        // Read socket group list from skillsTab
        let mut socket_groups = Vec::new();
        if let Ok(skills_tab) = build.get::<mlua::Table>("skillsTab") {
            if let Ok(group_list) = skills_tab.get::<mlua::Table>("socketGroupList") {
                for i in 1..=group_list.raw_len() {
                    if let Ok(group) = group_list.get::<mlua::Table>(i) {
                        let mut group_obj = lua_json_map!(group, {
                            ("displayLabel", "label"): String,
                            "enabled": bool,
                            "slot": String,
                        });

                        // Read gem list for this group
                        let mut gems = Vec::new();
                        if let Ok(gem_list) = group.get::<mlua::Table>("gemList") {
                            for j in 1..=gem_list.raw_len() {
                                if let Ok(gem) = gem_list.get::<mlua::Table>(j) {
                                    let gem_obj = lua_json_map!(gem, {
                                        ("nameSpec", "name"): String,
                                        "level": i64,
                                        "quality": i64,
                                        "enabled": bool,
                                    });
                                    if !gem_obj.is_empty() {
                                        gems.push(serde_json::Value::Object(gem_obj));
                                    }
                                }
                            }
                        }
                        if !gems.is_empty() {
                            group_obj.insert("gems".to_owned(), serde_json::Value::Array(gems));
                        }

                        if !group_obj.is_empty() {
                            socket_groups.push(serde_json::Value::Object(group_obj));
                        }
                    }
                }
            }
        }

        Ok(serde_json::json!({
            "skill_dps": skill_dps_list,
            "socket_groups": socket_groups,
        }))
    }

    /// Query the build configuration flags.
    ///
    /// Reads `build.configTab.input` as flat key-value pairs.
    pub fn query_config(&self) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        let build: mlua::Table = self.lua.globals().get("build")?;
        let config_tab: mlua::Table = build.get("configTab")?;
        let input: mlua::Table = config_tab.get("input")?;

        let mut config = serde_json::Map::new();
        for pair in input.pairs::<String, mlua::Value>() {
            let (key, value) = pair?;
            let json_val = match value {
                mlua::Value::Boolean(b) => serde_json::Value::Bool(b),
                mlua::Value::Integer(n) => serde_json::Value::Number(n.into()),
                mlua::Value::Number(n) => {
                    if let Some(num) = serde_json::Number::from_f64(n) {
                        serde_json::Value::Number(num)
                    } else {
                        continue;
                    }
                }
                mlua::Value::String(s) => {
                    serde_json::Value::String(s.to_str().map(|s| s.to_owned()).unwrap_or_default())
                }
                _ => continue,
            };
            config.insert(key, json_val);
        }

        Ok(serde_json::Value::Object(config))
    }

    /// Query the item equipped in the given slot.
    ///
    /// Returns a JSON object with the item's name, base type, rarity, quality,
    /// spirit, sockets, and all mod lines (implicit, explicit, enchant, rune).
    /// If the slot is empty, returns `{ "slot": "<name>", "empty": true }`.
    pub fn query_item(&self, slot: &str) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        let build: mlua::Table = self.lua.globals().get("build")?;
        let items_tab: mlua::Table = build.get("itemsTab")?;
        let active_item_set: mlua::Table = items_tab.get("activeItemSet")?;

        // Look up the slot in the active item set
        let slot_entry: mlua::Table = match active_item_set.get::<mlua::Table>(slot) {
            Ok(t) => t,
            Err(_) => return Ok(serde_json::json!({ "slot": slot, "empty": true })),
        };

        // Read selItemId — if nil or 0, the slot is empty
        let sel_item_id: i64 = lua_get!(slot_entry, "selItemId" => i64);
        if sel_item_id == 0 {
            return Ok(serde_json::json!({ "slot": slot, "empty": true }));
        }

        // Look up the item in items[selItemId]
        let items: mlua::Table = items_tab.get("items")?;
        let item: mlua::Table = match items.get::<mlua::Table>(sel_item_id) {
            Ok(t) => t,
            Err(_) => return Ok(serde_json::json!({ "slot": slot, "empty": true })),
        };

        let d = ItemFields::from_lua(&item);

        Ok(serde_json::json!({
            "slot": slot,
            "name": d.name,
            "base": d.base_name,
            "rarity": d.rarity,
            "quality": d.quality,
            "spirit": d.spirit,
            "sockets": d.sockets,
            "implicits": d.implicits,
            "explicits": d.explicits,
            "enchants": d.enchants,
            "runes": d.runes,
        }))
    }

    /// Query all equipped items with compact mod summaries.
    ///
    /// Returns gear (all 16 slots with item details or empty marker), jewels
    /// from the passive tree, and counts for empty/filled slots.
    pub fn query_equipped_items(&self) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        const ALL_SLOTS: &[&str] = &[
            "Weapon 1",
            "Weapon 2",
            "Helmet",
            "Body Armour",
            "Gloves",
            "Boots",
            "Amulet",
            "Ring 1",
            "Ring 2",
            "Ring 3",
            "Belt",
            "Charm 1",
            "Charm 2",
            "Charm 3",
            "Flask 1",
            "Flask 2",
        ];

        let build: mlua::Table = self.lua.globals().get("build")?;
        let items_tab: mlua::Table = build.get("itemsTab")?;
        let active_item_set: mlua::Table = items_tab.get("activeItemSet")?;
        let items: mlua::Table = items_tab.get("items")?;

        let mut gear = Vec::new();
        let mut empty_count: u32 = 0;
        let mut filled_count: u32 = 0;

        for &slot in ALL_SLOTS {
            let sel_item_id: i64 = active_item_set
                .get::<mlua::Table>(slot)
                .and_then(|entry| entry.get::<i64>("selItemId"))
                .unwrap_or(0);

            if sel_item_id == 0 {
                empty_count += 1;
                gear.push(serde_json::json!({ "slot": slot, "empty": true }));
                continue;
            }

            match items.get::<mlua::Table>(sel_item_id) {
                Ok(item) => {
                    filled_count += 1;
                    let d = ItemFields::from_lua(&item);
                    gear.push(serde_json::json!({
                        "slot": slot,
                        "name": d.name,
                        "base": d.base_name,
                        "rarity": d.rarity,
                        "mods": d.all_mods(),
                    }));
                }
                Err(_) => {
                    empty_count += 1;
                    gear.push(serde_json::json!({ "slot": slot, "empty": true }));
                }
            }
        }

        // Collect jewels from passive tree
        let spec: mlua::Table = build.get("spec")?;
        let mut jewels = Vec::new();
        if let Ok(jewels_table) = spec.get::<mlua::Table>("jewels") {
            for pair in jewels_table.pairs::<mlua::Value, mlua::Value>() {
                let (key, val) = pair?;
                let socket_id = lua_value_to_i64(&key).unwrap_or(0);
                let item_id = lua_value_to_i64(&val).unwrap_or(0);
                if socket_id == 0 || item_id == 0 {
                    continue;
                }

                if let Ok(item) = items.get::<mlua::Table>(item_id) {
                    let d = ItemFields::from_lua(&item);
                    jewels.push(serde_json::json!({
                        "socket_id": socket_id,
                        "name": d.name,
                        "base": d.base_name,
                        "rarity": d.rarity,
                        "mods": d.all_mods(),
                    }));
                }
            }
        }

        Ok(serde_json::json!({
            "gear": gear,
            "jewels": jewels,
            "empty_count": empty_count,
            "filled_count": filled_count,
        }))
    }

    /// Query a jewel socketed in a passive tree socket node.
    ///
    /// Returns a JSON object with the jewel's name, base type, rarity, quality,
    /// and all mod lines. If the socket is empty or invalid, returns
    /// `{ "socket_id": N, "empty": true }`.
    pub fn query_jewel(&self, socket_id: i64) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        let build: mlua::Table = self.lua.globals().get("build")?;
        let spec: mlua::Table = build.get("spec")?;

        // build.spec.jewels[socketNodeId] → itemId
        let jewels: mlua::Table = match spec.get::<mlua::Table>("jewels") {
            Ok(t) => t,
            Err(_) => return Ok(serde_json::json!({ "socket_id": socket_id, "empty": true })),
        };

        let item_id: i64 = match jewels.get::<i64>(socket_id) {
            Ok(id) if id != 0 => id,
            _ => return Ok(serde_json::json!({ "socket_id": socket_id, "empty": true })),
        };

        // build.itemsTab.items[itemId] → jewel item object
        let items_tab: mlua::Table = build.get("itemsTab")?;
        let items: mlua::Table = items_tab.get("items")?;
        let item: mlua::Table = match items.get::<mlua::Table>(item_id) {
            Ok(t) => t,
            Err(_) => return Ok(serde_json::json!({ "socket_id": socket_id, "empty": true })),
        };

        let d = ItemFields::from_lua(&item);

        Ok(serde_json::json!({
            "socket_id": socket_id,
            "name": d.name,
            "base": d.base_name,
            "rarity": d.rarity,
            "quality": d.quality,
            "implicits": d.implicits,
            "explicits": d.explicits,
            "enchants": d.enchants,
            "runes": d.runes,
        }))
    }

    /// Query the allocated passive tree nodes.
    ///
    /// Returns class, ascendancy, total node count, categorized node lists
    /// (keystones, notables, ascendancy nodes, masteries, jewel sockets),
    /// and aggregated stat totals from Normal nodes.
    pub fn query_passive_tree(&self) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        let build: mlua::Table = self.lua.globals().get("build")?;
        let spec: mlua::Table = build.get("spec")?;

        let class_name = lua_get!(spec, "curClassName" => String);
        let ascendancy_name = lua_get!(spec, "curAscendClassName" => String);

        let alloc_nodes: mlua::Table = spec.get("allocNodes")?;

        let mut keystones = Vec::new();
        let mut notables = Vec::new();
        let mut ascendancy_nodes = Vec::new();
        let mut masteries = Vec::new();
        let mut jewel_sockets = Vec::new();
        let mut total_allocated: u32 = 0;
        // Aggregate Normal node stats: stat line text → (count, sum of values)
        let mut normal_stat_agg: HashMap<String, (u32, f64)> = HashMap::new();

        for pair in alloc_nodes.pairs::<mlua::Value, mlua::Table>() {
            let (key, node) = pair?;
            total_allocated += 1;

            let node_type = lua_get!(node, "type" => String);
            let name = lua_get!(node, "dn" => String);
            let asc_name: Option<String> = node.get::<String>("ascendancyName").ok();

            // Ascendancy nodes go into their own bucket regardless of type
            if asc_name.is_some() {
                let stats = read_node_stats(&node);
                let mut entry = serde_json::json!({
                    "name": name,
                    "type": node_type,
                });
                if !stats.is_empty() {
                    entry["stats"] = serde_json::Value::Array(
                        stats.into_iter().map(serde_json::Value::String).collect(),
                    );
                }
                ascendancy_nodes.push(entry);
                continue;
            }

            match node_type.as_str() {
                "Keystone" => {
                    let stats = read_node_stats(&node);
                    let mut entry = serde_json::json!({ "name": name });
                    if !stats.is_empty() {
                        entry["stats"] = serde_json::Value::Array(
                            stats.into_iter().map(serde_json::Value::String).collect(),
                        );
                    }
                    keystones.push(entry);
                }
                "Notable" => {
                    let stats = read_node_stats(&node);
                    let mut entry = serde_json::json!({ "name": name });
                    if !stats.is_empty() {
                        entry["stats"] = serde_json::Value::Array(
                            stats.into_iter().map(serde_json::Value::String).collect(),
                        );
                    }
                    notables.push(entry);
                }
                "Mastery" => {
                    let stats = read_node_stats(&node);
                    let mut entry = serde_json::json!({ "name": name });
                    if !stats.is_empty() {
                        entry["stats"] = serde_json::Value::Array(
                            stats.into_iter().map(serde_json::Value::String).collect(),
                        );
                    }
                    masteries.push(entry);
                }
                "Socket" => {
                    let node_id = lua_value_to_i64(&key).unwrap_or(0);
                    jewel_sockets.push(serde_json::json!({
                        "node_id": node_id,
                        "name": name,
                    }));
                }
                "Normal" => {
                    // Aggregate stat lines from Normal nodes
                    let stats = read_node_stats(&node);
                    for stat_line in stats {
                        let value = extract_stat_value(&stat_line);
                        let entry = normal_stat_agg.entry(stat_line).or_insert((0, 0.0));
                        entry.0 += 1;
                        entry.1 += value;
                    }
                }
                // ClassStart, AscendClassStart — counted but not listed
                _ => {}
            }
        }

        // Build stat_totals sorted by total value descending
        let mut stat_totals: Vec<serde_json::Value> = normal_stat_agg
            .into_iter()
            .map(|(stat, (count, total))| {
                serde_json::json!({
                    "stat": stat,
                    "count": count,
                    "total": total,
                })
            })
            .collect();
        stat_totals.sort_by(|a, b| {
            let va = b["total"].as_f64().unwrap_or(0.0);
            let vb = a["total"].as_f64().unwrap_or(0.0);
            va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(serde_json::json!({
            "class": class_name,
            "ascendancy": ascendancy_name,
            "total_allocated": total_allocated,
            "keystones": keystones,
            "notables": notables,
            "ascendancy_nodes": ascendancy_nodes,
            "masteries": masteries,
            "jewel_sockets": jewel_sockets,
            "stat_totals": stat_totals,
        }))
    }

    /// Query how much of one or more stats comes from allocated passives and what's nearby.
    ///
    /// Performs case-insensitive substring matching on passive node stat descriptions.
    /// Uses multi-source BFS from allocated nodes to find nearby unallocated nodes
    /// with matching stats within `radius` hops. A single BFS pass serves all patterns.
    pub fn query_passive_stats(
        &self,
        stats: &[String],
        radius: u32,
    ) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        let patterns: Vec<String> = stats.iter().map(|s| s.to_lowercase()).collect();
        let build: mlua::Table = self.lua.globals().get("build")?;
        let spec: mlua::Table = build.get("spec")?;
        let alloc_nodes: mlua::Table = spec.get("allocNodes")?;
        let all_nodes: mlua::Table = spec.get("nodes")?;

        // Per-pattern accumulators
        struct StatAccum {
            allocated_total: f64,
            allocated_nodes: Vec<serde_json::Value>,
            nearby_total: f64,
            nearby_nodes: Vec<serde_json::Value>,
        }
        let mut accums: Vec<StatAccum> = patterns
            .iter()
            .map(|_| StatAccum {
                allocated_total: 0.0,
                allocated_nodes: Vec::new(),
                nearby_total: 0.0,
                nearby_nodes: Vec::new(),
            })
            .collect();

        // Phase A: Collect allocated node IDs and find matching stats
        let mut allocated_ids = HashSet::new();

        for pair in alloc_nodes.pairs::<mlua::Value, mlua::Table>() {
            let (key, node) = pair?;
            let node_id = lua_value_to_i64(&key).unwrap_or(0);
            allocated_ids.insert(node_id);

            for (pi, pattern) in patterns.iter().enumerate() {
                let (matching_stats, value) = match_node_stats(&node, pattern);
                if !matching_stats.is_empty() {
                    accums[pi].allocated_total += value;
                    accums[pi].allocated_nodes.push(serde_json::json!({
                        "name": lua_get!(node, "dn" => String),
                        "value": value,
                        "matching_stats": matching_stats,
                    }));
                }
            }
        }

        // Phase B: Build adjacency graph from all nodes
        let mut adjacency: HashMap<i64, Vec<i64>> = HashMap::new();

        for pair in all_nodes.pairs::<mlua::Value, mlua::Table>() {
            let (key, node) = pair?;
            let node_id = lua_value_to_i64(&key).unwrap_or(0);

            let mut neighbors = Vec::new();
            if let Ok(linked_ids) = node.get::<mlua::Table>("linkedId") {
                for i in 1..=linked_ids.raw_len() {
                    if let Ok(linked_id) = linked_ids.get::<i64>(i) {
                        neighbors.push(linked_id);
                    }
                }
            }
            adjacency.insert(node_id, neighbors);
        }

        // Phase C: Multi-source BFS from allocated nodes (single pass, all patterns)
        let mut visited = allocated_ids.clone();
        let mut queue = VecDeque::new();

        for &id in &allocated_ids {
            queue.push_back((id, 0u32));
        }

        while let Some((current_id, dist)) = queue.pop_front() {
            if let Some(neighbors) = adjacency.get(&current_id) {
                for &neighbor_id in neighbors {
                    if visited.contains(&neighbor_id) {
                        continue;
                    }
                    visited.insert(neighbor_id);

                    let next_dist = dist + 1;

                    // Check this unallocated node against all patterns
                    if let Ok(node) = all_nodes.get::<mlua::Table>(neighbor_id) {
                        for (pi, pattern) in patterns.iter().enumerate() {
                            let (matching_stats, value) = match_node_stats(&node, pattern);
                            if !matching_stats.is_empty() {
                                accums[pi].nearby_total += value;
                                accums[pi].nearby_nodes.push(serde_json::json!({
                                    "name": lua_get!(node, "dn" => String),
                                    "value": value,
                                    "distance": next_dist,
                                    "matching_stats": matching_stats,
                                }));
                            }
                        }
                    }

                    if next_dist < radius {
                        queue.push_back((neighbor_id, next_dist));
                    }
                }
            }
        }

        // Sort nearby nodes by distance, then by value descending
        for accum in &mut accums {
            accum.nearby_nodes.sort_by(|a, b| {
                let da = a["distance"].as_u64().unwrap_or(0);
                let db = b["distance"].as_u64().unwrap_or(0);
                da.cmp(&db).then_with(|| {
                    let va = b["value"].as_f64().unwrap_or(0.0);
                    let vb = a["value"].as_f64().unwrap_or(0.0);
                    va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal)
                })
            });
        }

        // Build result grouped by stat pattern
        let mut result_stats = serde_json::Map::new();
        for (pi, stat_name) in stats.iter().enumerate() {
            let accum = &accums[pi];
            result_stats.insert(
                stat_name.clone(),
                serde_json::json!({
                    "allocated": {
                        "total_value": accum.allocated_total,
                        "nodes": accum.allocated_nodes,
                    },
                    "nearby_available": {
                        "total_value": accum.nearby_total,
                        "nodes": accum.nearby_nodes,
                    },
                }),
            );
        }

        Ok(serde_json::json!({ "stats": result_stats }))
    }

    /// Query ascendancy nodes: which are allocated and which are available.
    ///
    /// Returns primary and secondary ascendancy names, lists of allocated and
    /// available nodes with stats, and point counts for each ascendancy.
    pub fn query_unallocated_ascendancy(&self) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        let build: mlua::Table = self.lua.globals().get("build")?;
        let spec: mlua::Table = build.get("spec")?;
        let tree: mlua::Table = spec.get("tree")?;

        // Read ascendancy class names
        let primary_name = lua_get!(spec, "curAscendClassName" => String, "None".to_owned());
        let secondary_name =
            lua_get!(spec, "curSecondaryAscendClassName" => String, "None".to_owned());

        // Build set of secondary ascendancy names for classification
        let secondary_asc_names: HashSet<String> =
            if let Ok(map) = tree.get::<mlua::Table>("secondaryAscendNameMap") {
                map.pairs::<String, mlua::Value>()
                    .filter_map(|pair| pair.ok().map(|(k, _)| k))
                    .collect()
            } else {
                HashSet::new()
            };

        // Collect allocated node IDs for fast lookup
        let alloc_nodes: mlua::Table = spec.get("allocNodes")?;
        let mut allocated_ids = HashSet::new();
        for pair in alloc_nodes.pairs::<mlua::Value, mlua::Table>() {
            let (key, _) = pair?;
            if let Some(id) = lua_value_to_i64(&key) {
                allocated_ids.insert(id);
            }
        }

        // Determine which ascendancy names belong to this build
        let has_primary = primary_name != "None" && !primary_name.is_empty();
        let has_secondary = secondary_name != "None" && !secondary_name.is_empty();

        let mut primary_nodes = Vec::new();
        let mut secondary_nodes = Vec::new();
        let mut primary_points_spent: u32 = 0;
        let mut secondary_points_spent: u32 = 0;

        // Iterate all nodes, filter for ascendancy nodes belonging to this build
        let all_nodes: mlua::Table = spec.get("nodes")?;
        for pair in all_nodes.pairs::<mlua::Value, mlua::Table>() {
            let (key, node) = pair?;
            let node_id = lua_value_to_i64(&key).unwrap_or(0);

            let asc_name = match node.get::<String>("ascendancyName") {
                Ok(name) => name,
                Err(_) => continue, // Not an ascendancy node
            };

            // Skip nodes that don't belong to this build's ascendancies
            let is_secondary = secondary_asc_names.contains(&asc_name);
            let belongs = if is_secondary {
                has_secondary && asc_name == secondary_name
            } else {
                has_primary && asc_name == primary_name
            };
            if !belongs {
                continue;
            }

            // Skip start nodes — they're auto-allocated and don't cost points
            let node_type = lua_get!(node, "type" => String);
            if node_type == "AscendClassStart" {
                continue;
            }

            let name = lua_get!(node, "dn" => String);
            let stats = read_node_stats(&node);
            let is_multiple_choice_option = lua_get!(node, "isMultipleChoiceOption" => bool);
            let is_allocated = allocated_ids.contains(&node_id);

            let mut entry = serde_json::json!({
                "name": name,
                "type": node_type,
                "allocated": is_allocated,
            });
            if !stats.is_empty() {
                entry["stats"] = serde_json::Value::Array(
                    stats.into_iter().map(serde_json::Value::String).collect(),
                );
            }

            if is_secondary {
                if is_allocated && !is_multiple_choice_option {
                    secondary_points_spent += 1;
                }
                secondary_nodes.push(entry);
            } else {
                if is_allocated && !is_multiple_choice_option {
                    primary_points_spent += 1;
                }
                primary_nodes.push(entry);
            }
        }

        let mut result = serde_json::json!({
            "primary_ascendancy": primary_name,
            "primary_nodes": primary_nodes,
            "primary_points_spent": primary_points_spent,
        });

        if has_secondary {
            result["secondary_ascendancy"] = serde_json::Value::String(secondary_name);
            result["secondary_nodes"] = serde_json::Value::Array(secondary_nodes);
            result["secondary_points_spent"] = serde_json::json!(secondary_points_spent);
        }

        Ok(result)
    }

    /// Query detailed DPS breakdown for a specific skill.
    ///
    /// Finds the skill by name in `activeSkillList`, sets it as main skill,
    /// recalculates, and reads the detailed output table. Since each query
    /// loads the build fresh, modifying main skill selection is safe.
    ///
    /// Requires CWD to be the PoB `src/` directory because `LoadModule("Calcs")`
    /// uses `loadfile` with a relative path.
    pub fn query_skill_breakdown(&self, skill_name: &str) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        let skill_name = skill_name.to_owned();
        // Run with CWD set to PoB src/ since LoadModule("Calcs") needs it
        self.with_pob_cwd(|lua| {
            lua.load(LUA_SKILL_BREAKDOWN).exec()?;
            let breakdown_fn: mlua::Function = lua.globals().get("getSkillBreakdown")?;
            let result_str: String = breakdown_fn.call(skill_name)?;
            Ok(result_str)
        })
        .and_then(|result_str| {
            serde_json::from_str(&result_str).map_err(|e| {
                PobError::CalculationFailed(format!("failed to parse Lua JSON result: {e}"))
            })
        })
    }

    /// Search item bases by type and/or name substring.
    ///
    /// Queries `data.itemBases` in the Lua runtime. Returns base name, type,
    /// implicit, level req, and weapon/armour stats. Caps results at 20.
    pub fn query_search_bases(
        &self,
        item_type: Option<&str>,
        query: Option<&str>,
    ) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        self.lua.load(LUA_SEARCH_BASES).exec()?;
        let search_fn: mlua::Function = self.lua.globals().get("searchBases")?;

        let args = self.lua.create_table()?;
        if let Some(t) = item_type {
            args.set("item_type", t)?;
        }
        if let Some(q) = query {
            args.set("query", q)?;
        }

        let result_str: String = search_fn.call(args)?;
        serde_json::from_str(&result_str).map_err(|e| {
            PobError::CalculationFailed(format!("failed to parse Lua JSON result: {e}"))
        })
    }

    /// Search item mods by stat text, item type tag, and/or mod type.
    ///
    /// Queries `data.itemMods.Item` in the Lua runtime. Returns mod ID, affix name,
    /// stat text with ranges, level, group. Caps results at 20.
    pub fn query_search_mods(
        &self,
        query: Option<&str>,
        item_type_tag: Option<&str>,
        mod_type: Option<&str>,
    ) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        self.lua.load(LUA_SEARCH_MODS).exec()?;
        let search_fn: mlua::Function = self.lua.globals().get("searchMods")?;

        let args = self.lua.create_table()?;
        if let Some(q) = query {
            args.set("query", q)?;
        }
        if let Some(t) = item_type_tag {
            args.set("item_type_tag", t)?;
        }
        if let Some(m) = mod_type {
            args.set("mod_type", m)?;
        }

        let result_str: String = search_fn.call(args)?;
        serde_json::from_str(&result_str).map_err(|e| {
            PobError::CalculationFailed(format!("failed to parse Lua JSON result: {e}"))
        })
    }

    /// Search the gem database by name, type (active/support), and/or tags.
    ///
    /// Iterates `data.gems` in the Lua runtime. Deduplicates support gem tiers
    /// by `gemFamily`, keeping only the highest tier variant. Caps results at 15.
    pub fn query_search_gems(
        &self,
        query: Option<&str>,
        gem_type: Option<&str>,
        tags: &[String],
    ) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        self.lua.load(LUA_SEARCH_GEMS).exec()?;
        let search_fn: mlua::Function = self.lua.globals().get("searchGems")?;

        // Build args table for Lua
        let args = self.lua.create_table()?;
        if let Some(q) = query {
            args.set("query", q)?;
        }
        if let Some(t) = gem_type {
            args.set("gem_type", t)?;
        }
        if !tags.is_empty() {
            let tags_table = self.lua.create_table()?;
            for (i, tag) in tags.iter().enumerate() {
                tags_table.set(i as i64 + 1, tag.as_str())?;
            }
            args.set("tags", tags_table)?;
        }

        let result_str: String = search_fn.call(args)?;
        serde_json::from_str(&result_str).map_err(|e| {
            PobError::CalculationFailed(format!("failed to parse Lua JSON result: {e}"))
        })
    }

    /// Search the unique item database by name, slot, and/or level range.
    ///
    /// Iterates `data.uniques` in the Lua runtime. Parses raw item text to
    /// extract name, base, mods, and variant info. Caps results at 15.
    pub fn query_search_uniques(
        &self,
        query: Option<&str>,
        slot: Option<&str>,
        min_level: Option<u32>,
        max_level: Option<u32>,
    ) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        self.lua.load(LUA_SEARCH_UNIQUES).exec()?;
        let search_fn: mlua::Function = self.lua.globals().get("searchUniques")?;

        // Build args table for Lua
        let args = self.lua.create_table()?;
        if let Some(q) = query {
            args.set("query", q)?;
        }
        if let Some(s) = slot {
            args.set("slot", s)?;
        }
        if let Some(min) = min_level {
            args.set("min_level", min)?;
        }
        if let Some(max) = max_level {
            args.set("max_level", max)?;
        }

        let result_str: String = search_fn.call(args)?;
        serde_json::from_str(&result_str).map_err(|e| {
            PobError::CalculationFailed(format!("failed to parse Lua JSON result: {e}"))
        })
    }

    /// List all charm bases with trigger condition, buff effect, duration, and charges.
    ///
    /// Iterates `data.itemBases` in the Lua runtime, filtering for items with
    /// `base.type == "Charm"`. Returns all 13 charms sorted by level requirement.
    pub fn query_list_charms(&self) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        self.lua.load(LUA_LIST_CHARMS).exec()?;
        let list_fn: mlua::Function = self.lua.globals().get("listCharms")?;

        let result_str: String = list_fn.call(())?;
        serde_json::from_str(&result_str).map_err(|e| {
            PobError::CalculationFailed(format!("failed to parse Lua JSON result: {e}"))
        })
    }

    /// Search the rune and soul core database by name, stat text, and/or slot.
    ///
    /// Iterates `data.itemMods.Runes` in the Lua runtime. Runes give different
    /// bonuses per equipment slot. Caps results at 15.
    pub fn query_search_runes(
        &self,
        query: Option<&str>,
        slot: Option<&str>,
    ) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        self.lua.load(LUA_SEARCH_RUNES).exec()?;
        let search_fn: mlua::Function = self.lua.globals().get("searchRunes")?;

        let args = self.lua.create_table()?;
        if let Some(q) = query {
            args.set("query", q)?;
        }
        if let Some(s) = slot {
            args.set("slot", s)?;
        }

        let result_str: String = search_fn.call(args)?;
        serde_json::from_str(&result_str).map_err(|e| {
            PobError::CalculationFailed(format!("failed to parse Lua JSON result: {e}"))
        })
    }

    /// Create an item from PoB item text, equip it in the given slot, and return
    /// the created item details plus a before/after stat delta.
    ///
    /// The item text must follow PoB's newline-delimited format:
    /// `Rarity: RARE\nTitle\nBase Type\nItem Level: 86\nImplicits: 1\n+10 to Str\n+50 Life`
    ///
    /// The mutation is applied to the in-memory Lua state only. The build XML is
    /// not modified — subsequent queries that reload XML will see the original build.
    pub fn create_item(&self, slot: &str, item_text: &str) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        let result_str = self.with_pob_cwd(|lua| {
            lua.load(LUA_CREATE_ITEM).exec()?;
            let create_fn: mlua::Function = lua.globals().get("createItemInSlot")?;

            let args = lua.create_table()?;
            args.set("slot", slot)?;
            args.set("item_text", item_text)?;

            let result: String = create_fn.call(args)?;
            Ok(result)
        })?;

        serde_json::from_str(&result_str).map_err(|e| {
            PobError::CalculationFailed(format!("failed to parse Lua JSON result: {e}"))
        })
    }

    /// Analyze gear mods: tier info, roll quality, and upgrade potential.
    ///
    /// Runs a Lua-side analysis that leverages PoB's native data structures
    /// (`item.affixes`, `item.prefixes`/`item.suffixes`) to compute tier
    /// position, roll quality, and best available tier at item level.
    ///
    /// Returns JSON serialized by dkjson on the Lua side.
    pub fn query_gear_mod_analysis(&self, slot: &str) -> Result<serde_json::Value, PobError> {
        if !self.initialized {
            return Err(PobError::NotInitialized);
        }

        let build: mlua::Table = self.lua.globals().get("build")?;
        let items_tab: mlua::Table = build.get("itemsTab")?;
        let active_item_set: mlua::Table = items_tab.get("activeItemSet")?;

        // Look up the slot in the active item set
        let slot_entry: mlua::Table = match active_item_set.get::<mlua::Table>(slot) {
            Ok(t) => t,
            Err(_) => return Ok(serde_json::json!({ "slot": slot, "empty": true })),
        };

        let sel_item_id: i64 = lua_get!(slot_entry, "selItemId" => i64);
        if sel_item_id == 0 {
            return Ok(serde_json::json!({ "slot": slot, "empty": true }));
        }

        let items: mlua::Table = items_tab.get("items")?;
        let item: mlua::Table = match items.get::<mlua::Table>(sel_item_id) {
            Ok(t) => t,
            Err(_) => return Ok(serde_json::json!({ "slot": slot, "empty": true })),
        };

        // Register and call the Lua analysis function
        self.lua.load(LUA_GEAR_MOD_ANALYSIS).exec()?;
        let analyze_fn: mlua::Function = self.lua.globals().get("analyzeItemMods")?;
        let result_str: String = analyze_fn.call(item)?;

        // Parse the JSON result and inject the slot name
        let mut result: serde_json::Value = serde_json::from_str(&result_str).map_err(|e| {
            PobError::CalculationFailed(format!("failed to parse Lua JSON result: {e}"))
        })?;

        if let Some(obj) = result.as_object_mut() {
            obj.insert(
                "slot".to_owned(),
                serde_json::Value::String(slot.to_owned()),
            );
        }

        Ok(result)
    }

    /// Run a closure with CWD set to the PoB `src/` directory,
    /// restoring the original CWD afterwards.
    fn with_pob_cwd<F, R>(&self, f: F) -> Result<R, PobError>
    where
        F: FnOnce(&Lua) -> LuaResult<R>,
    {
        let pob_src = self.pob_src_path.as_ref().ok_or(PobError::NotInitialized)?;
        let original_cwd = std::env::current_dir()?;
        std::env::set_current_dir(pob_src)?;
        let result = f(&self.lua);
        std::env::set_current_dir(original_cwd)?;
        Ok(result?)
    }
}

impl Default for PobHeadless {
    fn default() -> Self {
        Self::new().expect("Failed to create Lua runtime")
    }
}

// SAFETY: PobHeadless is !Send because mlua::Lua with LuaJIT is !Send.
// PobParser handles this by pinning it to a dedicated OS thread.

/// Lua function that computes a detailed DPS breakdown for a specific skill.
///
/// Finds the skill in the active skill list by case-insensitive substring match,
/// sets it as the main skill, recalculates via `calcs.perform`, and reads the
/// detailed output fields. Uses dkjson for JSON serialization.
const LUA_SKILL_BREAKDOWN: &str = r#"
local dkjson = require("dkjson")

function getSkillBreakdown(skillName)
    local calcs = LoadModule("Calcs")
    local env = calcs.buildOutput(build, "CALCS")

    if not env or not env.player or not env.player.activeSkillList then
        return dkjson.encode({ error = "Failed to build calculation environment" })
    end

    -- Find the skill by case-insensitive substring match
    local searchName = skillName:lower()
    local matchedSkill = nil

    for _, skill in ipairs(env.player.activeSkillList) do
        if skill.activeEffect and skill.activeEffect.grantedEffect then
            local name = skill.activeEffect.grantedEffect.name or ""
            if name:lower():find(searchName, 1, true) then
                matchedSkill = skill
                break
            end
        end
    end

    if not matchedSkill then
        -- List available skills for the error message
        local available = {}
        for _, skill in ipairs(env.player.activeSkillList) do
            if skill.activeEffect and skill.activeEffect.grantedEffect then
                local name = skill.activeEffect.grantedEffect.name
                if name and name ~= "" then
                    table.insert(available, name)
                end
            end
        end
        return dkjson.encode({
            error = "Skill not found: " .. skillName,
            available_skills = available,
        })
    end

    -- Set as main skill and recalculate
    env.player.mainSkill = matchedSkill
    env.player.output = {}
    calcs.perform(env)

    local output = env.player.output or {}
    local skillName = matchedSkill.activeEffect.grantedEffect.name or skillName

    -- Helper to safely read a number
    local function num(key)
        local v = output[key]
        if type(v) == "number" then return v end
        return nil
    end

    -- Read damage type min/max
    local damageTypes = {}
    for _, dtype in ipairs({"Physical", "Fire", "Cold", "Lightning", "Chaos"}) do
        local min = num(dtype .. "Min")
        local max = num(dtype .. "Max")
        if min and max and (min > 0 or max > 0) then
            damageTypes[dtype:lower()] = { min = min, max = max }
        end
    end

    -- Read conversion table if present
    local conversions = {}
    if matchedSkill.conversionTable then
        for fromType, convTable in pairs(matchedSkill.conversionTable) do
            if type(convTable) == "table" then
                for toType, fraction in pairs(convTable) do
                    if type(fraction) == "number" and fraction > 0 and toType ~= "mult" then
                        local key = fromType:lower() .. "_to_" .. toType:lower()
                        conversions[key] = math.floor(fraction * 100 + 0.5)
                    end
                end
            end
        end
    end

    -- Build flags from skillData
    local flags = {}
    local skillData = matchedSkill.skillData or {}
    if matchedSkill.skillFlags then
        flags.is_attack = matchedSkill.skillFlags.attack or false
        flags.is_spell = matchedSkill.skillFlags.spell or false
        flags.is_projectile = matchedSkill.skillFlags.projectile or false
        flags.is_area = matchedSkill.skillFlags.area or false
        flags.is_melee = matchedSkill.skillFlags.melee or false
        flags.is_totem = matchedSkill.skillFlags.totem or false
        flags.is_trap = matchedSkill.skillFlags.trap or false
        flags.is_mine = matchedSkill.skillFlags.mine or false
    end

    local result = {
        skill_name = skillName,
        total_dps = num("TotalDPS"),
        average_hit = num("AverageHit"),
        crit_chance = num("CritChance"),
        crit_multiplier = num("CritMultiplier"),
        hit_chance = num("HitChance"),
        speed = num("Speed"),
        damage_types = damageTypes,
        conversions = conversions,
        ailments = {
            bleed_dps = num("BleedDPS"),
            ignite_dps = num("IgniteDPS"),
            poison_dps = num("PoisonDPS"),
        },
        combined_dps = num("CombinedDPS"),
        flags = flags,
    }

    return dkjson.encode(result)
end
"#;

/// Lua function that analyzes an item's mods against the full affix database.
///
/// Lua function that searches the gem database (`data.gems`).
///
/// Accepts a table with optional `query` (name substring), `gem_type`
/// Lua function that searches item bases (`data.itemBases`).
///
/// Accepts a table with optional `item_type` (e.g. "Bow", "Helmet") and `query`
/// (case-insensitive name substring). At least one must be provided.
/// Returns up to 20 results sorted by level_req ascending.
const LUA_SEARCH_BASES: &str = r#"
local dkjson = require("dkjson")

function searchBases(args)
    local itemType = args.item_type or nil
    local query = args.query and args.query:lower() or nil

    local matches = {}

    for baseName, base in pairs(data.itemBases) do
        local dominated = false

        -- Type filter
        if itemType and base.type ~= itemType then
            dominated = true
        end

        -- Name substring filter
        if not dominated and query and not baseName:lower():find(query, 1, true) then
            dominated = true
        end

        if not dominated then
            local entry = {
                name = baseName,
                type = base.type or "",
                level_req = base.req and base.req.level or 0,
            }

            -- Implicit mods
            if base.implicit then
                entry.implicit = base.implicit
            end

            -- Weapon stats
            if base.weapon then
                local w = base.weapon
                entry.weapon = {
                    type = w.type or "",
                    min = w.min or 0,
                    max = w.max or 0,
                    base_speed = w.attackRateBase or 0,
                    crit_chance = w.CritChance or 0,
                }
            end

            -- Armour stats
            if base.armour then
                local a = base.armour
                local armourInfo = {}
                if (a.ArmourBase or 0) > 0 then armourInfo.armour = a.ArmourBase end
                if (a.EvasionBase or 0) > 0 then armourInfo.evasion = a.EvasionBase end
                if (a.EnergyShieldBase or 0) > 0 then armourInfo.energy_shield = a.EnergyShieldBase end
                if (a.WardBase or 0) > 0 then armourInfo.ward = a.WardBase end
                if next(armourInfo) then
                    entry.armour = armourInfo
                end
            end

            -- Tags (useful for mod searches)
            if base.tags then
                local tagList = {}
                for tag, _ in pairs(base.tags) do
                    table.insert(tagList, tag)
                end
                if #tagList > 0 then
                    table.sort(tagList)
                    entry.tags = tagList
                end
            end

            table.insert(matches, entry)
        end
    end

    local totalResults = #matches

    -- Sort by level_req ascending
    table.sort(matches, function(a, b)
        if a.level_req ~= b.level_req then
            return a.level_req < b.level_req
        end
        return a.name < b.name
    end)

    -- Cap at 20 results
    local results = {}
    local cap = 20
    for i = 1, math.min(cap, #matches) do
        table.insert(results, matches[i])
    end

    return dkjson.encode({
        results = results,
        total_results = totalResults,
    })
end
"#;

/// Lua function that searches item mods (`data.itemMods.Item`).
///
/// Accepts a table with optional `query` (stat text substring), `item_type_tag`
/// (filter to mods with weight > 0 for this tag), and `mod_type` ("prefix"/"suffix").
/// At least one of `query` or `item_type_tag` must be provided.
/// Returns up to 20 results sorted by level descending (highest tier first).
const LUA_SEARCH_MODS: &str = r#"
local dkjson = require("dkjson")

function searchMods(args)
    local query = args.query and args.query:lower() or nil
    local itemTypeTag = args.item_type_tag and args.item_type_tag:lower() or nil
    local modTypeFilter = args.mod_type and args.mod_type:lower() or nil

    local matches = {}

    -- data.itemMods.Item is keyed by mod group (prefix/suffix pools)
    -- Each entry: { [1] = "stat line", [2] = "stat line", ..., level = N,
    --   group = "...", type = "Prefix"/"Suffix", affix = "...",
    --   weightKey = {...}, weightVal = {...} }
    for modId, mod in pairs(data.itemMods.Item) do
        local dominated = false

        -- Mod type filter (prefix/suffix)
        if modTypeFilter then
            local mtype = (mod.type or ""):lower()
            if mtype ~= modTypeFilter then
                dominated = true
            end
        end

        -- Item type tag filter: check weightKey/weightVal arrays
        if not dominated and itemTypeTag then
            local hasWeight = false
            if mod.weightKey and mod.weightVal then
                for i, key in ipairs(mod.weightKey) do
                    if key:lower() == itemTypeTag and (mod.weightVal[i] or 0) > 0 then
                        hasWeight = true
                        break
                    end
                end
            end
            if not hasWeight then
                dominated = true
            end
        end

        -- Stat text query filter
        if not dominated and query then
            local found = false
            -- Check affix name
            if mod.affix and mod.affix:lower():find(query, 1, true) then
                found = true
            end
            -- Check stat lines (numeric array entries)
            if not found then
                local i = 1
                while mod[i] do
                    if mod[i]:lower():find(query, 1, true) then
                        found = true
                        break
                    end
                    i = i + 1
                end
            end
            if not found then
                dominated = true
            end
        end

        if not dominated then
            -- Collect stat lines
            local statLines = {}
            local i = 1
            while mod[i] do
                table.insert(statLines, mod[i])
                i = i + 1
            end

            local entry = {
                mod_id = modId,
                affix = mod.affix or "",
                type = mod.type or "",
                level = mod.level or 0,
                group = mod.group or "",
                stats = statLines,
            }

            table.insert(matches, entry)
        end
    end

    local totalResults = #matches

    -- Sort by level descending (highest tier first)
    table.sort(matches, function(a, b)
        if a.level ~= b.level then
            return a.level > b.level
        end
        return a.mod_id < b.mod_id
    end)

    -- Cap at 20 results
    local results = {}
    local cap = 20
    for i = 1, math.min(cap, #matches) do
        table.insert(results, matches[i])
    end

    return dkjson.encode({
        results = results,
        total_results = totalResults,
    })
end
"#;

/// ("active"/"support"), and `tags` (array of tag names, AND logic).
/// Deduplicates support gem tiers by `gemFamily`, keeping only the highest tier.
/// Returns up to 15 results sorted alphabetically, with `total_results` count.
const LUA_SEARCH_GEMS: &str = r#"
local dkjson = require("dkjson")

function searchGems(args)
    local query = args.query and args.query:lower() or nil
    local gemType = args.gem_type and args.gem_type:lower() or nil
    local tags = args.tags or {}

    local matches = {}

    for gemId, gem in pairs(data.gems) do
        local dominated = false

        -- Name filter
        if query and not (gem.name or ""):lower():find(query, 1, true) then
            dominated = true
        end

        -- Type filter
        if not dominated and gemType then
            if gemType == "active" then
                if not (gem.tags and gem.tags.grants_active_skill) then
                    dominated = true
                end
            elseif gemType == "support" then
                if gem.gemType ~= "Support" then
                    dominated = true
                end
            end
        end

        -- Tag filter (AND logic)
        if not dominated and #tags > 0 then
            for _, tag in ipairs(tags) do
                if not (gem.tags and gem.tags[tag:lower()]) then
                    dominated = true
                    break
                end
            end
        end

        if not dominated then
            table.insert(matches, gem)
        end
    end

    -- Deduplicate support gem tiers: keep highest tier per gemFamily
    local deduped = {}
    local familySeen = {}
    for _, gem in ipairs(matches) do
        if gem.gemType == "Support" and gem.gemFamily and gem.gemFamily ~= "" then
            local existing = familySeen[gem.gemFamily]
            if existing then
                if (gem.Tier or 1) > (existing.Tier or 1) then
                    familySeen[gem.gemFamily] = gem
                end
            else
                familySeen[gem.gemFamily] = gem
            end
        else
            table.insert(deduped, gem)
        end
    end
    for _, gem in pairs(familySeen) do
        table.insert(deduped, gem)
    end

    local totalResults = #deduped

    -- Sort alphabetically by name
    table.sort(deduped, function(a, b) return (a.name or "") < (b.name or "") end)

    -- Cap at 15 results
    local results = {}
    local cap = 15
    for i = 1, math.min(cap, #deduped) do
        local gem = deduped[i]
        local entry = {
            name = gem.name,
            gem_type = gem.gemType,
            tags = gem.tagString or "",
            tier = gem.Tier or 1,
            max_level = gem.naturalMaxLevel or 20,
            requirements = {
                str = gem.reqStr or 0,
                dex = gem.reqDex or 0,
                int = gem.reqInt or 0,
            },
        }
        if gem.gemFamily and gem.gemFamily ~= "" and gem.gemFamily ~= gem.name then
            entry.family = gem.gemFamily
        end
        table.insert(results, entry)
    end

    return dkjson.encode({
        results = results,
        total_results = totalResults,
    })
end
"#;

/// Lua function that searches the unique item database (`data.uniques`).
///
/// Accepts a table with optional `query` (name/mod substring), `slot` (slot type
/// substring), `min_level`, and `max_level`. Parses each unique item's raw text
/// to extract name, base, mods, and variant info. Resolves slot type and level
/// requirement from `data.itemBases`. Returns up to 15 results sorted
/// alphabetically, with `total_results` count.
const LUA_SEARCH_UNIQUES: &str = r#"
local dkjson = require("dkjson")

function searchUniques(args)
    local query = args.query and args.query:lower() or nil
    local slotFilter = args.slot and args.slot:lower() or nil
    local minLevel = args.min_level
    local maxLevel = args.max_level

    local matches = {}

    for slotKey, items in pairs(data.uniques) do
        if slotKey ~= "generated" then
            for _, rawText in ipairs(items) do
                -- Parse the multi-line raw string
                local lines = {}
                for line in rawText:gmatch("[^\n]+") do
                    table.insert(lines, line)
                end

                if #lines < 2 then
                    goto continue
                end

                local itemName = lines[1]
                local baseName = lines[2]

                -- Collect metadata and mods from remaining lines
                local variantNames = {}
                local variantCount = 0
                local explicitLevelReq = nil
                local modLines = {}  -- { text = "...", variants = {1,2} or nil }

                for i = 3, #lines do
                    local line = lines[i]

                    -- Variant tracking
                    local varName = line:match("^Variant: (.+)$")
                    if varName then
                        variantCount = variantCount + 1
                        table.insert(variantNames, varName)
                        goto nextline
                    end

                    -- Skip metadata lines
                    if line:match("^Implicits: %d+") then goto nextline end
                    if line:match("^League:") then goto nextline end
                    if line:match("^Source:") then goto nextline end
                    if line:match("^Limited to:") then goto nextline end
                    if line:match("^Sockets:") then goto nextline end
                    if line:match("^Has Alt Variant:") then goto nextline end

                    -- Explicit level requirement
                    local lvl = line:match("^Requires Level (%d+)")
                    if lvl then
                        explicitLevelReq = tonumber(lvl)
                        goto nextline
                    end

                    -- Check for variant-gated mod: {variant:N} or {variant:N,M,...}
                    local variantGate = nil
                    local afterVariant = line:match("^{variant:([^}]+)}(.+)$")
                    if afterVariant then
                        variantGate = {}
                        local nums = line:match("^{variant:([^}]+)}")
                        for n in nums:gmatch("%d+") do
                            table.insert(variantGate, tonumber(n))
                        end
                        line = afterVariant
                    end

                    -- Strip {tags:...} prefix
                    line = line:gsub("^{tags:[^}]+}", "")

                    -- Strip {range:...} prefix
                    line = line:gsub("^{range:[^}]+}", "")

                    if line ~= "" then
                        table.insert(modLines, { text = line, variants = variantGate })
                    end

                    ::nextline::
                end

                -- Variant handling: select "current" variant (last one)
                local currentVariant = variantCount > 0 and variantCount or nil
                local displayMods = {}
                for _, mod in ipairs(modLines) do
                    if mod.variants then
                        -- Only include if current variant is in the list
                        if currentVariant then
                            local included = false
                            for _, v in ipairs(mod.variants) do
                                if v == currentVariant then
                                    included = true
                                    break
                                end
                            end
                            if included then
                                table.insert(displayMods, mod.text)
                            end
                        end
                    else
                        -- No variant gate: always included
                        table.insert(displayMods, mod.text)
                    end
                end

                -- Resolve slot type and level requirement from item base data
                local baseData = data.itemBases[baseName]
                local slotType = baseData and baseData.type or "Unknown"
                local levelReq = baseData and baseData.req and baseData.req.level or 0
                if explicitLevelReq then
                    levelReq = explicitLevelReq
                end

                -- Apply filters
                -- Query: match against name or any mod text
                if query then
                    local matched = false
                    if itemName:lower():find(query, 1, true) then
                        matched = true
                    end
                    if not matched then
                        for _, modText in ipairs(displayMods) do
                            if modText:lower():find(query, 1, true) then
                                matched = true
                                break
                            end
                        end
                    end
                    if not matched then goto continue end
                end

                -- Slot filter
                if slotFilter then
                    if not slotType:lower():find(slotFilter, 1, true) then
                        goto continue
                    end
                end

                -- Level range filter
                if minLevel and levelReq < minLevel then goto continue end
                if maxLevel and levelReq > maxLevel then goto continue end

                table.insert(matches, {
                    name = itemName,
                    base = baseName,
                    slot = slotType,
                    level_req = levelReq,
                    mods = displayMods,
                })

                ::continue::
            end
        end
    end

    local totalResults = #matches

    -- Sort alphabetically by name
    table.sort(matches, function(a, b) return a.name < b.name end)

    -- Cap at 15 results
    local results = {}
    for i = 1, math.min(15, #matches) do
        table.insert(results, matches[i])
    end

    return dkjson.encode({
        results = results,
        total_results = totalResults,
    })
end
"#;

/// Lua function that lists all charm bases from `data.itemBases`.
///
/// No parameters. Filters for `base.type == "Charm"` and extracts trigger
/// condition (`implicit`), buff effect (`charm.buff[1]`), duration, and charge
/// info. Returns all 13 charms sorted by level requirement ascending.
const LUA_LIST_CHARMS: &str = r#"
local dkjson = require("dkjson")

function listCharms()
    local charms = {}

    for name, base in pairs(data.itemBases) do
        if base.type == "Charm" then
            local charm = base.charm or {}
            local buff = ""
            if charm.buff and charm.buff[1] then
                buff = charm.buff[1]
            end
            table.insert(charms, {
                name = name,
                trigger = base.implicit or "",
                buff = buff,
                duration = charm.duration or 0,
                charges_used = charm.chargesUsed or 0,
                charges_max = charm.chargesMax or 0,
                level_req = base.req and base.req.level or 0,
            })
        end
    end

    -- Sort by level requirement ascending
    table.sort(charms, function(a, b) return a.level_req < b.level_req end)

    return dkjson.encode({ charms = charms })
end
"#;

/// Lua function that creates an item from PoB text format, equips it in a slot,
/// recalculates, and returns item details + stat delta + exported XML.
///
/// Accepts a table `{ slot = "Weapon 1", item_text = "Rarity: RARE\n..." }`.
/// Returns JSON with `slot`, `item`, `delta`, and `xml` fields on success,
/// or `{ error = "..." }` on failure.
const LUA_CREATE_ITEM: &str = r#"
local dkjson = require("dkjson")

function createItemInSlot(args)
    local slotName = args.slot
    local itemText = args.item_text
    local mainOutput = build.calcsTab.mainOutput

    local STAT_FIELDS = {
        -- Offense
        "TotalDPS", "CombinedDPS", "AverageDamage", "Speed",
        "CritChance", "CritMultiplier",
        -- Resources
        "Life", "EnergyShield", "Mana", "Spirit",
        -- Defense
        "Armour", "Evasion", "Ward",
        -- Resistances
        "FireResist", "ColdResist", "LightningResist", "ChaosResist",
    }

    -- 1. Snapshot stats before
    local before = {}
    for _, field in ipairs(STAT_FIELDS) do
        before[field] = mainOutput[field] or 0
    end

    -- 2. Parse item from PoB text format
    local item = new("Item", itemText)

    -- 3. Validate: base type must be recognized
    if not item.base then
        -- Try to extract the attempted base name from item text for fuzzy matching
        local suggestions = {}
        local attemptedBase = nil
        local lines = {}
        for line in itemText:gmatch("[^\n]+") do
            table.insert(lines, line)
        end
        -- For RARE/UNIQUE: base is line after title (3rd content line after Rarity)
        -- For NORMAL/MAGIC: base is 2nd content line after Rarity
        local contentLines = {}
        for _, line in ipairs(lines) do
            local trimmed = line:match("^%s*(.-)%s*$")
            if trimmed ~= "" and not trimmed:match("^Rarity:") then
                table.insert(contentLines, trimmed)
            end
        end
        if #contentLines >= 2 then
            -- Check rarity to determine which line is the base
            local rarityLine = ""
            for _, line in ipairs(lines) do
                if line:match("^Rarity:") then
                    rarityLine = line:upper()
                    break
                end
            end
            if rarityLine:find("RARE") or rarityLine:find("UNIQUE") then
                attemptedBase = contentLines[2]
            else
                attemptedBase = contentLines[1]
            end
        end

        -- Search for similar base names
        if attemptedBase then
            local searchLower = attemptedBase:lower()
            for baseName, _ in pairs(data.itemBases) do
                if baseName:lower():find(searchLower, 1, true) or searchLower:find(baseName:lower(), 1, true) then
                    table.insert(suggestions, baseName)
                    if #suggestions >= 5 then break end
                end
            end
            -- If no substring match, try matching individual words
            if #suggestions == 0 then
                for word in searchLower:gmatch("%S+") do
                    if #word >= 3 then
                        for baseName, _ in pairs(data.itemBases) do
                            if baseName:lower():find(word, 1, true) then
                                table.insert(suggestions, baseName)
                                if #suggestions >= 5 then break end
                            end
                        end
                        if #suggestions >= 5 then break end
                    end
                end
            end
        end

        local msg = "Invalid item text: base type not recognized by PoB."
        if attemptedBase then
            msg = msg .. " Attempted base: '" .. attemptedBase .. "'."
        end
        if #suggestions > 0 then
            table.sort(suggestions)
            msg = msg .. " Did you mean: " .. table.concat(suggestions, ", ") .. "?"
        end
        msg = msg .. " Use the search_bases tool to find valid base type names."
        return dkjson.encode({ error = msg })
    end
    if not item.type then
        return dkjson.encode({
            error = "Item parsed but type could not be determined. " ..
                    "Ensure the base type is a valid PoE2 item base.",
        })
    end

    -- 4. Add to build item list (noAutoEquip = true)
    local ok, err = pcall(function() build.itemsTab:AddItem(item, true) end)
    if not ok then
        return dkjson.encode({
            error = "Failed to add item to build (headless GUI issue): " .. tostring(err),
        })
    end

    -- 5. Equip in the named slot
    local activeSet = build.itemsTab.activeItemSet
    if activeSet[slotName] == nil then
        return dkjson.encode({
            error = "Unknown slot: '" .. slotName .. "'. " ..
                    "Valid slots: Weapon 1, Weapon 2, Helmet, Body Armour, Gloves, Boots, " ..
                    "Amulet, Ring 1, Ring 2, Ring 3, Belt, Charm 1, Charm 2, Charm 3, " ..
                    "Flask 1, Flask 2.",
        })
    end
    activeSet[slotName].selItemId = item.id

    -- 6. Trigger recalculation (headless has no frame loop; must call manually)
    build.buildFlag = true
    runCallback("OnFrame")

    -- 7. Snapshot stats after and compute delta
    local after = {}
    local changed = {}
    for _, field in ipairs(STAT_FIELDS) do
        local a = mainOutput[field] or 0
        after[field] = a
        local b = before[field]
        if a ~= b then
            changed[field] = { before = b, after = a, delta = a - b }
        end
    end

    -- 8. Export the mutated build XML
    local xmlText = build:SaveDB("code")
    if not xmlText then
        return dkjson.encode({ error = "Failed to export build XML after mutation." })
    end

    -- 9. Read back item details
    local implicits = {}
    for _, line in ipairs(item.implicitModLines or {}) do
        table.insert(implicits, line.line or "")
    end
    local explicits = {}
    local matchedMods = 0
    local unmatchedMods = {}
    for _, line in ipairs(item.explicitModLines or {}) do
        table.insert(explicits, line.line or "")
        -- Check if modList is populated (non-empty = mod was recognized by PoB)
        if line.modList and #line.modList > 0 then
            matchedMods = matchedMods + 1
        else
            table.insert(unmatchedMods, line.line or "")
        end
    end

    return dkjson.encode({
        slot = slotName,
        item = {
            name = item.name or "",
            base = item.baseName or "",
            rarity = ({"NORMAL","MAGIC","RARE","UNIQUE"})[item.rarity + 1] or "UNKNOWN",
            type = item.type or "",
            quality = item.quality or 0,
            sockets = item.itemSocketCount or 0,
            implicits = implicits,
            explicits = explicits,
        },
        matched_mods = matchedMods,
        unmatched_mods = unmatchedMods,
        delta = {
            changed = changed,
        },
        -- Consumed by execute_tool to queue the mutation; stripped before LLM sees it
        xml = xmlText,
    })
end
"#;

/// Lua function that searches the rune and soul core database (`data.itemMods.Runes`).
///
/// Accepts a table with optional `query` (name/stat substring) and `slot` (slot key
/// substring). Rune mods are organized per-slot with stat lines as array entries.
/// Level requirement comes from `data.itemBases[runeName]`.
/// Returns up to 15 results sorted alphabetically, with `total_results` count.
const LUA_SEARCH_RUNES: &str = r#"
local dkjson = require("dkjson")

function searchRunes(args)
    local query = args.query and args.query:lower() or nil
    local slotFilter = args.slot and args.slot:lower() or nil

    local runeMap = {}

    for runeName, slotTable in pairs(data.itemMods.Runes) do
        for slotKey, mod in pairs(slotTable) do
            -- Extract stat lines (numeric array entries on the mod table)
            local stats = {}
            local i = 1
            while mod[i] do
                table.insert(stats, mod[i])
                i = i + 1
            end

            -- Check query match: rune name or any stat line in this slot
            local queryMatch = true
            if query then
                queryMatch = false
                if runeName:lower():find(query, 1, true) then
                    queryMatch = true
                else
                    for _, stat in ipairs(stats) do
                        if stat:lower():find(query, 1, true) then
                            queryMatch = true
                            break
                        end
                    end
                end
            end

            if queryMatch then
                if not runeMap[runeName] then
                    runeMap[runeName] = {}
                end
                runeMap[runeName][slotKey] = stats
            end
        end
    end

    -- Build results from runeMap, applying slot filter to displayed slots
    local matches = {}
    for runeName, slots in pairs(runeMap) do
        -- If slot filter is provided, only include matching slot entries
        local displaySlots = {}
        local hasSlot = false
        for slotKey, stats in pairs(slots) do
            if slotFilter then
                if slotKey:lower():find(slotFilter, 1, true) then
                    displaySlots[slotKey] = stats
                    hasSlot = true
                end
            else
                displaySlots[slotKey] = stats
                hasSlot = true
            end
        end

        if hasSlot then
            local baseData = data.itemBases[runeName]
            local levelReq = baseData and baseData.req and baseData.req.level or 0

            table.insert(matches, {
                name = runeName,
                level_req = levelReq,
                slots = displaySlots,
            })
        end
    end

    local totalResults = #matches

    -- Sort alphabetically by name
    table.sort(matches, function(a, b) return a.name < b.name end)

    -- Cap at 15 results
    local results = {}
    for i = 1, math.min(15, #matches) do
        table.insert(results, matches[i])
    end

    return dkjson.encode({
        results = results,
        total_results = totalResults,
    })
end
"#;

/// Runs entirely in Lua to avoid costly cross-boundary iteration over ~1700 affix
/// entries. Uses PoB's `dkjson` library for JSON serialization.
///
/// Handles two item paths:
/// - **Crafted items** (`item.crafted`): reads `item.prefixes[i].modId` + `.range`
///   directly for exact tier + roll quality.
/// - **Imported items**: reverse-matches `item.explicitModLines[i].line` against
///   all mods in `item.affixes` by building patterns from mod templates.
const LUA_GEAR_MOD_ANALYSIS: &str = r#"
local dkjson = require("dkjson")

function analyzeItemMods(item)
    -- Guard: items without affixes (uniques, flasks, etc.)
    if not item.affixes then
        return dkjson.encode({
            not_applicable = true,
            reason = "Item has no affix database (unique, flask, or special item type)",
            item_name = item.name or "",
            rarity = item.rarity or "",
        })
    end

    if item.rarity == "UNIQUE" then
        return dkjson.encode({
            not_applicable = true,
            reason = "Unique items have fixed mods — tier analysis does not apply",
            item_name = item.name or "",
            rarity = "UNIQUE",
        })
    end

    if item.rarity == "NORMAL" then
        return dkjson.encode({
            not_applicable = true,
            reason = "Normal items have no mods",
            item_name = item.name or "",
            rarity = "NORMAL",
        })
    end

    -- Build group index: group name -> sorted list of mods (T1 = first = highest level)
    local groups = {}
    local modById = {}
    for modId, mod in pairs(item.affixes) do
        if type(mod) == "table" and mod.group then
            modById[modId] = mod
            if not groups[mod.group] then
                groups[mod.group] = {}
            end
            table.insert(groups[mod.group], { modId = modId, mod = mod })
        end
    end
    for _, mods in pairs(groups) do
        table.sort(mods, function(a, b) return a.mod.level > b.mod.level end)
    end

    -- Helper: parse range notation from a template string.
    -- E.g. "+(40-59) to maximum Life" -> { {min=40, max=59} }
    local function parseRanges(template)
        local ranges = {}
        for sign, minStr, maxStr in template:gmatch("([%+-]?)%((%d+%.?%d*)%-(%d+%.?%d*)%)") do
            local minVal = tonumber(minStr)
            local maxVal = tonumber(maxStr)
            if sign == "-" then
                minVal = -minVal
                maxVal = -maxVal
                -- Swap so min < max
                minVal, maxVal = maxVal, minVal
            end
            table.insert(ranges, { min = minVal, max = maxVal })
        end
        return ranges
    end

    -- Helper: get tier info for a mod within its group
    local function getTierInfo(modId, mod)
        local group = groups[mod.group]
        if not group then
            return nil
        end
        local totalTiers = #group
        local tierNum = nil
        for i, entry in ipairs(group) do
            if entry.modId == modId then
                tierNum = i
                break
            end
        end
        if not tierNum then
            return nil
        end

        -- Best tier at item level (0 = unknown, treat as unlimited)
        local itemLevel = item.itemLevel or 0
        local bestTierAtIlvl = nil
        if itemLevel > 0 then
            for i, entry in ipairs(group) do
                if entry.mod.level <= itemLevel then
                    bestTierAtIlvl = i
                    break
                end
            end
        else
            -- No item level info — T1 is theoretically available
            bestTierAtIlvl = 1
        end

        -- T1 range (first entry in group = highest tier)
        local t1Ranges = parseRanges(group[1].mod[1] or "")
        local t1RangeStr = nil
        if #t1Ranges > 0 then
            t1RangeStr = string.format("T1 %s [%g-%g]",
                group[1].mod.affix or "",
                t1Ranges[1].min, t1Ranges[1].max)
        end

        -- Best tier at ilvl range
        local bestAtIlvlStr = nil
        if bestTierAtIlvl and bestTierAtIlvl < tierNum then
            local bestEntry = group[bestTierAtIlvl]
            local bestRanges = parseRanges(bestEntry.mod[1] or "")
            if #bestRanges > 0 then
                bestAtIlvlStr = string.format("T%d %s [%g-%g]",
                    bestTierAtIlvl, bestEntry.mod.affix or "",
                    bestRanges[1].min, bestRanges[1].max)
            end
        end

        return {
            tier = tierNum,
            total_tiers = totalTiers,
            tier_label = string.format("T%d/T%d", tierNum, totalTiers),
            best_tier_at_ilvl = bestAtIlvlStr,
            upgradeable = bestTierAtIlvl ~= nil and bestTierAtIlvl < tierNum,
            max_tier_range = t1Ranges[1] and { t1Ranges[1].min, t1Ranges[1].max } or nil,
        }
    end

    -- Helper: analyze a single mod entry (crafted path)
    local function analyzeCraftedMod(prefix, modType)
        if not prefix.modId or prefix.modId == "None" then
            return nil
        end
        local mod = modById[prefix.modId]
        if not mod then
            return nil
        end

        local template = mod[1] or ""
        local ranges = parseRanges(template)
        local tierInfo = getTierInfo(prefix.modId, mod)

        -- Compute roll quality from range parameter
        local rollPct = prefix.range or 0.5
        local rollValue = nil
        local currentRange = nil
        if #ranges > 0 then
            currentRange = { ranges[1].min, ranges[1].max }
            rollValue = ranges[1].min + rollPct * (ranges[1].max - ranges[1].min)
            rollValue = math.floor(rollValue + 0.5)
        end

        -- Build the display line by applying the range
        local line = template
        if itemLib and itemLib.applyRange then
            line = itemLib.applyRange(template, prefix.range or 0.5)
        end

        local result = {
            mod_id = prefix.modId,
            line = line,
            affix_name = mod.affix or "",
            type = modType,
            group = mod.group or "",
            required_level = mod.level or 0,
            current_range = currentRange,
            roll_value = rollValue,
            roll_pct = rollPct,
            tags = mod.modTags or {},
        }
        if tierInfo then
            for k, v in pairs(tierInfo) do
                result[k] = v
            end
        end
        return result
    end

    -- Helper: reverse-match a mod line against the affix database (imported path)
    local function reverseMatchLine(lineText)
        -- Try each mod in the affix database
        for modId, mod in pairs(item.affixes) do
            if type(mod) == "table" and mod[1] then
                local template = mod[1]

                -- Build a Lua pattern from the template:
                -- Replace "(min-max)" with a capture for the number
                -- Escape special pattern chars first
                local pattern = template
                -- Escape Lua pattern special chars (except parens which we handle)
                pattern = pattern:gsub("%%", "%%%%")
                pattern = pattern:gsub("%.", "%%.")
                pattern = pattern:gsub("%+", "%%+")
                pattern = pattern:gsub("%-", "%%-")
                pattern = pattern:gsub("%*", "%%*")
                pattern = pattern:gsub("%?", "%%?")
                pattern = pattern:gsub("%[", "%%[")
                pattern = pattern:gsub("%]", "%%]")
                pattern = pattern:gsub("%^", "%%^")
                pattern = pattern:gsub("%$", "%%$")

                -- Now replace the range patterns "(min-max)" with number captures
                -- The ranges look like "%(min%%-max%)" after escaping
                pattern = pattern:gsub("%((%d+%%.?%d*)%%%-(%d+%%.?%d*)%)", "(%%d+%%.?%%d*)")

                -- Try to match
                pattern = "^" .. pattern .. "$"
                local captures = { lineText:match(pattern) }
                if #captures > 0 then
                    local ranges = parseRanges(template)
                    local value = tonumber(captures[1])

                    -- Verify the value falls within a valid range for this mod
                    if value and #ranges > 0 then
                        -- Check if value is in range (with some tolerance for rounding)
                        if value >= ranges[1].min - 0.5 and value <= ranges[1].max + 0.5 then
                            local tierInfo = getTierInfo(modId, mod)
                            local rollPct = nil
                            if ranges[1].max > ranges[1].min then
                                rollPct = (value - ranges[1].min) / (ranges[1].max - ranges[1].min)
                                rollPct = math.floor(rollPct * 100 + 0.5) / 100
                            else
                                rollPct = 1.0
                            end

                            local result = {
                                mod_id = modId,
                                line = lineText,
                                affix_name = mod.affix or "",
                                type = mod.type or "",
                                group = mod.group or "",
                                required_level = mod.level or 0,
                                current_range = { ranges[1].min, ranges[1].max },
                                roll_value = value,
                                roll_pct = rollPct,
                                tags = mod.modTags or {},
                            }
                            if tierInfo then
                                for k, v in pairs(tierInfo) do
                                    result[k] = v
                                end
                            end
                            return result
                        end
                    end
                end
            end
        end
        return nil
    end

    local prefixes = {}
    local suffixes = {}
    local unmatchedMods = {}
    local prefixCount = 0
    local suffixCount = 0

    if item.crafted and item.prefixes and #item.prefixes > 0 then
        -- Crafted path: direct modId lookup
        for _, p in ipairs(item.prefixes) do
            if p.modId and p.modId ~= "None" then
                local info = analyzeCraftedMod(p, "Prefix")
                if info then
                    table.insert(prefixes, info)
                    prefixCount = prefixCount + 1
                end
            end
        end
        for _, s in ipairs(item.suffixes) do
            if s.modId and s.modId ~= "None" then
                local info = analyzeCraftedMod(s, "Suffix")
                if info then
                    table.insert(suffixes, info)
                    suffixCount = suffixCount + 1
                end
            end
        end
    else
        -- Imported path: reverse-match explicit mod lines
        if item.explicitModLines then
            for i = 1, #item.explicitModLines do
                local modLine = item.explicitModLines[i]
                if modLine and modLine.line then
                    local info = reverseMatchLine(modLine.line)
                    if info then
                        if info.type == "Prefix" then
                            table.insert(prefixes, info)
                            prefixCount = prefixCount + 1
                        else
                            table.insert(suffixes, info)
                            suffixCount = suffixCount + 1
                        end
                    else
                        table.insert(unmatchedMods, modLine.line)
                    end
                end
            end
        end
    end

    -- Determine affix limits
    local maxPrefixes = 3
    local maxSuffixes = 3
    if item.rarity == "MAGIC" then
        maxPrefixes = 1
        maxSuffixes = 1
    end
    if item.type == "Jewel" then
        maxPrefixes = 2
        maxSuffixes = 2
    end
    if item.prefixes and item.prefixes.limit then
        maxPrefixes = item.prefixes.limit
    end
    if item.suffixes and item.suffixes.limit then
        maxSuffixes = item.suffixes.limit
    end

    local baseName = item.baseName or ""

    local result = {
        item_name = item.name or "",
        base = baseName,
        rarity = item.rarity or "",
        item_level = item.itemLevel or 0,
        crafted = item.crafted or false,
        prefixes = prefixes,
        suffixes = suffixes,
        prefix_count = prefixCount,
        suffix_count = suffixCount,
        max_prefixes = maxPrefixes,
        max_suffixes = maxSuffixes,
        open_prefixes = maxPrefixes - prefixCount,
        open_suffixes = maxSuffixes - suffixCount,
        unmatched_mods = unmatchedMods,
    }

    return dkjson.encode(result)
end
"#;

// ---------------------------------------------------------------------------
// Shared item-field extraction
// ---------------------------------------------------------------------------

/// Common fields extracted from a PoB Lua item table.
///
/// Used by `query_item`, `query_jewel`, and `query_equipped_items` to avoid
/// duplicating the same get/unwrap chains for every item read.
struct ItemFields {
    name: String,
    base_name: String,
    rarity: String,
    quality: i64,
    spirit: i64,
    sockets: i64,
    implicits: Vec<String>,
    explicits: Vec<String>,
    enchants: Vec<String>,
    runes: Vec<String>,
}

impl ItemFields {
    fn from_lua(item: &mlua::Table) -> Self {
        Self {
            name: lua_get!(item, "name" => String),
            base_name: item
                .get::<mlua::Table>("base")
                .and_then(|b| b.get::<String>("name"))
                .unwrap_or_default(),
            rarity: lua_get!(item, "rarity" => String),
            quality: lua_get!(item, "quality" => i64),
            spirit: lua_get!(item, "spiritValue" => i64),
            sockets: lua_get!(item, "itemSocketCount" => i64),
            implicits: read_mod_lines(item, "implicitModLines"),
            explicits: read_mod_lines(item, "explicitModLines"),
            enchants: read_mod_lines(item, "enchantModLines"),
            runes: read_mod_lines(item, "runeModLines"),
        }
    }

    /// Combined implicit + explicit mod lines.
    fn all_mods(&self) -> Vec<String> {
        self.implicits
            .iter()
            .chain(self.explicits.iter())
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Lua table helpers
// ---------------------------------------------------------------------------

/// Read numeric fields from a Lua table into a JSON map.
/// Tries f64 first, then i64, skipping nil/missing values.
fn read_fields(table: &mlua::Table, fields: &[&str]) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    for &field in fields {
        if let Ok(v) = table.get::<f64>(field) {
            if v != 0.0 {
                if let Some(num) = serde_json::Number::from_f64(v) {
                    map.insert(field.to_owned(), serde_json::Value::Number(num));
                }
            }
        } else if let Ok(v) = table.get::<i64>(field) {
            if v != 0 {
                map.insert(field.to_owned(), serde_json::Value::Number(v.into()));
            }
        }
    }
    map
}

/// Read mod line strings from an item's mod array (e.g. `explicitModLines`).
///
/// Each entry in the Lua array is a table with a `line` field.
/// Returns an empty vec if the field is missing or has no entries.
fn read_mod_lines(item: &mlua::Table, field: &str) -> Vec<String> {
    let table = match item.get::<mlua::Table>(field) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    (1..=table.raw_len())
        .filter_map(|i| table.get::<mlua::Table>(i).ok())
        .filter_map(|entry| entry.get::<String>("line").ok())
        .collect()
}

/// Merge entries from `src` into `dst`, preferring values already in `dst`.
fn merge_fields(
    dst: &mut serde_json::Map<String, serde_json::Value>,
    src: &serde_json::Map<String, serde_json::Value>,
) {
    for (key, value) in src {
        dst.entry(key.clone()).or_insert_with(|| value.clone());
    }
}

/// Read the `sd` (stat description) array from a passive tree node table.
///
/// Returns an empty vec if the field is missing or has no entries.
fn read_node_stats(node: &mlua::Table) -> Vec<String> {
    let table = match node.get::<mlua::Table>("sd") {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    (1..=table.raw_len())
        .filter_map(|i| table.get::<String>(i).ok())
        .collect()
}

/// Extract an i64 from a Lua value (Integer or Number).
fn lua_value_to_i64(val: &mlua::Value) -> Option<i64> {
    match val {
        mlua::Value::Integer(n) => Some(*n),
        mlua::Value::Number(n) => Some(*n as i64),
        _ => None,
    }
}

/// Check a node's `sd` lines for case-insensitive substring matches against `pattern`.
///
/// Returns the matching stat line strings and the sum of extracted numeric values.
fn match_node_stats(node: &mlua::Table, pattern: &str) -> (Vec<String>, f64) {
    let sd = match node.get::<mlua::Table>("sd") {
        Ok(t) => t,
        Err(_) => return (Vec::new(), 0.0),
    };

    let mut matching = Vec::new();
    let mut total = 0.0;

    for i in 1..=sd.raw_len() {
        if let Ok(line) = sd.get::<String>(i) {
            if line.to_lowercase().contains(pattern) {
                total += extract_stat_value(&line);
                matching.push(line);
            }
        }
    }

    (matching, total)
}

/// Extract the first numeric value from a stat description line.
///
/// Handles integers and decimals, e.g. "+25% increased Fire Damage" → 25.0,
/// "0.5% of Fire Damage Leeched as Life" → 0.5. Returns 0.0 if no number found.
fn extract_stat_value(line: &str) -> f64 {
    let mut start = None;
    let mut has_dot = false;

    for (i, ch) in line.char_indices() {
        match ch {
            '0'..='9' => {
                if start.is_none() {
                    start = Some(i);
                }
            }
            '.' if start.is_some() && !has_dot => {
                has_dot = true;
            }
            _ => {
                if let Some(s) = start {
                    if let Ok(val) = line[s..i].parse::<f64>() {
                        return val;
                    }
                    start = None;
                    has_dot = false;
                }
            }
        }
    }

    // Check if the number extends to end of string
    if let Some(s) = start {
        line[s..].parse::<f64>().unwrap_or(0.0)
    } else {
        0.0
    }
}

// -- Smoke tests -------------------------------------------------------------
//
// Exercise the real mlua-backed `PobHeadless` against the checked-in fixture.
// Gated behind `#[ignore]` so `cargo test --lib` (and `just precommit`) stays
// fast and usable on a fresh clone without the PoB2 vendor directory.
//
// Run with:
//
//     cargo test --lib --features _unused -- --ignored pob::smoke_tests

#[cfg(test)]
mod smoke_tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// Path to the vendored PoB2 checkout at the workspace root, if it exists.
    /// (Upstream pointed at `<crate>/vendor`; MossRaven's vendor lives at the
    /// workspace root, so we walk up two directories from the crate manifest.)
    fn vendor_pob_path() -> Option<PathBuf> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/PathOfBuilding-PoE2");
        p.join("src/HeadlessWrapper.lua").exists().then_some(p)
    }

    fn fixture_xml() -> String {
        // Fixtures live in this crate's own tests/fixtures dir. Not committed
        // in v1 — drop a real PoB2 XML export there to exercise the smoke tests.
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ranger-with-gear.xml");
        std::fs::read_to_string(p).expect("fixture missing — drop a PoB XML at tests/fixtures/ranger-with-gear.xml")
    }

    /// Skip the test body if the vendored PoB checkout is absent. Prints a
    /// friendly message so CI / clean checkouts don't silently pass.
    macro_rules! require_vendor {
        () => {
            match vendor_pob_path() {
                Some(p) => p,
                None => {
                    eprintln!(
                        "skipping: vendor/PathOfBuilding-PoE2 not present — \
                         clone it to run this test"
                    );
                    return;
                }
            }
        };
    }

    #[test]
    #[ignore = "loads full PoB Lua VM — run with --ignored"]
    fn calculate_against_ranger_fixture_returns_plausible_stats() {
        let pob_path = require_vendor!();

        let mut pob = PobHeadless::new().expect("PobHeadless::new");
        pob.init(pob_path.to_str().unwrap()).expect("init");

        let xml = fixture_xml();
        pob.load_build_xml(&xml).expect("load_build_xml");
        let stats = pob.calculate().expect("calculate");

        // The fixture is a real ranger build — exact values may shift as PoB
        // updates, so assert broad shape rather than precise numbers.
        assert!(stats.total_dps > 0.0, "DPS should be positive: {stats:?}");
        assert!(stats.life > 0.0, "life should be positive: {stats:?}");
        // Resistances are in the i32 range and typically capped at 75% for PoE2.
        assert!(
            (-100..=95).contains(&stats.fire_res),
            "fire_res sane: {stats:?}"
        );
        assert!(
            (-100..=95).contains(&stats.cold_res),
            "cold_res sane: {stats:?}"
        );
    }

    #[test]
    #[ignore = "loads full PoB Lua VM — run with --ignored"]
    fn query_build_stats_returns_category_groupings() {
        let pob_path = require_vendor!();

        let mut pob = PobHeadless::new().expect("PobHeadless::new");
        pob.init(pob_path.to_str().unwrap()).expect("init");
        pob.load_build_xml(&fixture_xml()).expect("load");

        let stats = pob.query_build_stats().expect("query_build_stats");

        // Shape check — the tool returns grouped keys.
        assert!(stats.is_object());
        let obj = stats.as_object().unwrap();
        assert!(
            obj.contains_key("offense") || obj.contains_key("defense"),
            "expected offense/defense groupings, got keys: {:?}",
            obj.keys().collect::<Vec<_>>()
        );
    }

    /// Bisect what attribute change actually moves DPS. Take the seed and
    /// emit four mutants — each touching only one thing — and compare scores.
    #[test]
    #[ignore = "loads full PoB Lua VM — run with --ignored"]
    fn bisect_which_mutation_moves_dps() {
        let pob_path = require_vendor!();
        let seed_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/seed.xml");
        let seed = std::fs::read_to_string(&seed_path).expect("seed");

        let mut pob = PobHeadless::new().expect("PobHeadless::new");
        pob.init(pob_path.to_str().unwrap()).expect("init");

        let cases: Vec<(&str, String)> = vec![
            ("seed", seed.clone()),
            (
                "WS quality 0->20 + defaultGemLevel=custom",
                seed.clone()
                    .replace(r#"defaultGemLevel="normalMaximum""#, r#"defaultGemLevel="custom""#)
                    .replace(r#"defaultGemQuality="0""#, r#"defaultGemQuality="custom""#)
                    .replacen(
                        r#"level="20" enableGlobal1="true" variantId="WhirlingSlash" skillId="WhirlingSlashPlayer" quality="0""#,
                        r#"level="20" enableGlobal1="true" variantId="WhirlingSlash" skillId="WhirlingSlashPlayer" quality="20""#,
                        1,
                    ),
            ),
            (
                "Martial Tempo level 1->20 + defaults=custom",
                seed.clone()
                    .replace(r#"defaultGemLevel="normalMaximum""#, r#"defaultGemLevel="custom""#)
                    .replace(r#"defaultGemQuality="0""#, r#"defaultGemQuality="custom""#)
                    .replacen(
                        r#"level="1" enableGlobal1="true" variantId="FasterAttackSupport""#,
                        r#"level="20" enableGlobal1="true" variantId="FasterAttackSupport""#,
                        1,
                    ),
            ),
            (
                "Concentrated Effect level 1->20 + defaults=custom",
                seed.clone()
                    .replace(r#"defaultGemLevel="normalMaximum""#, r#"defaultGemLevel="custom""#)
                    .replace(r#"defaultGemQuality="0""#, r#"defaultGemQuality="custom""#)
                    .replacen(
                        r#"level="1" enableGlobal1="true" variantId="ConcentratedEffectSupport""#,
                        r#"level="20" enableGlobal1="true" variantId="ConcentratedEffectSupport""#,
                        1,
                    ),
            ),
            (
                "WS level 20->4 + defaults=custom",
                seed.clone()
                    .replace(r#"defaultGemLevel="normalMaximum""#, r#"defaultGemLevel="custom""#)
                    .replace(r#"defaultGemQuality="0""#, r#"defaultGemQuality="custom""#)
                    .replacen(
                        r#"level="20" enableGlobal1="true" variantId="WhirlingSlash" skillId="WhirlingSlashPlayer" quality="0""#,
                        r#"level="4" enableGlobal1="true" variantId="WhirlingSlash" skillId="WhirlingSlashPlayer" quality="0""#,
                        1,
                    ),
            ),
            (
                "defaultGemLevel=characterLevel (no other changes)",
                seed.clone()
                    .replace(r#"defaultGemLevel="normalMaximum""#, r#"defaultGemLevel="characterLevel""#),
            ),
        ];

        let mut baseline = 0.0;
        for (label, xml) in &cases {
            pob.load_build_xml(xml).expect(label);
            let stats = pob.calculate().expect(label);
            if label == &"seed" {
                baseline = stats.total_dps;
            }
            let pct = if baseline > 0.0 { ((stats.total_dps / baseline) - 1.0) * 100.0 } else { 0.0 };
            eprintln!("{:<55}  DPS={:>12.2}   {:+.2}%", label, stats.total_dps, pct);
        }
    }

    /// Sanity-check: load the cascade's *actual* m1 XML (dumped from the
    /// archive) and verify whether its score differs from the seed. The
    /// cascade reported 49,200 DPS for m1 (Whirling Slash level=20 quality=20)
    /// despite the seed only having quality=0 — investigating why.
    #[test]
    #[ignore = "loads full PoB Lua VM — run with --ignored"]
    fn cascade_m1_xml_score_vs_seed() {
        let pob_path = require_vendor!();
        let seed_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/seed.xml");
        let seed = std::fs::read_to_string(&seed_path).expect("seed");

        let m1_path = std::path::PathBuf::from(std::env::var("TEMP").unwrap_or_default())
            .join("m1.xml");
        if !m1_path.exists() {
            eprintln!("skipping: dump %TEMP%/m1.xml from the archive first");
            return;
        }
        let m1 = std::fs::read_to_string(&m1_path).expect("m1 dump");

        let mut pob = PobHeadless::new().expect("PobHeadless::new");
        pob.init(pob_path.to_str().unwrap()).expect("init");

        pob.load_build_xml(&seed).expect("load seed");
        let baseline = pob.calculate().expect("calc seed");
        eprintln!("SEED   DPS={:.4}", baseline.total_dps);

        pob.load_build_xml(&m1).expect("load m1");
        let mutated = pob.calculate().expect("calc m1");
        eprintln!("M1     DPS={:.4}", mutated.total_dps);

        let pct = ((mutated.total_dps / baseline.total_dps) - 1.0) * 100.0;
        eprintln!("m1 / seed = {pct:+.2}%");
    }

    /// Empirical check: does PoB actually respect the per-gem `level=` and
    /// `quality=` attributes in the XML, or does the build-wide
    /// `defaultGemLevel="normalMaximum"` clobber them?
    ///
    /// We load the workspace's seed.xml, score it, then load a mutant where
    /// every gem on the main socket group is level=1 + quality=0 and the
    /// Skills container declares `defaultGemLevel="custom"`. If PoB respects
    /// per-gem overrides, the mutant should score WAY lower DPS.
    #[test]
    #[ignore = "loads full PoB Lua VM — run with --ignored"]
    fn per_gem_level_quality_actually_affects_score() {
        let pob_path = require_vendor!();
        let seed_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/seed.xml");
        let seed = std::fs::read_to_string(&seed_path).expect("seed.xml at tests/fixtures/");

        let mut pob = PobHeadless::new().expect("PobHeadless::new");
        pob.init(pob_path.to_str().unwrap()).expect("init");
        pob.load_build_xml(&seed).expect("load seed");
        let baseline = pob.calculate().expect("calculate seed");
        eprintln!("BASELINE DPS={:.2}", baseline.total_dps);

        // Mutant: same seed but with defaultGemLevel="custom" + every level=20
        // turned into level=4 on the main skill (Whirling Slash). Should score
        // drastically lower DPS if the mutation is respected.
        let mutant = seed
            .replace(r#"defaultGemLevel="normalMaximum""#, r#"defaultGemLevel="custom""#)
            .replace(r#"defaultGemQuality="0""#, r#"defaultGemQuality="custom""#)
            // Whirling Slash starts at level=20 → drop to level=4
            .replacen(
                r#"level="20" enableGlobal1="true" variantId="WhirlingSlash""#,
                r#"level="4" enableGlobal1="true" variantId="WhirlingSlash""#,
                1,
            )
            // Berserk starts at level=20 → drop to level=4
            .replacen(
                r#"level="20" enableGlobal1="true" variantId="Berserk""#,
                r#"level="4" enableGlobal1="true" variantId="Berserk""#,
                1,
            );

        pob.load_build_xml(&mutant).expect("load mutant");
        let mutated = pob.calculate().expect("calculate mutant");
        eprintln!("MUTANT DPS={:.2} (Whirling Slash + Berserk dropped to level=4)", mutated.total_dps);

        // The whole point: if PoB respects per-gem levels, dropping the main
        // skill from 20→4 should significantly drop DPS. >5% drop is enough.
        let pct_drop = 1.0 - (mutated.total_dps / baseline.total_dps);
        eprintln!("relative drop: {:.1}%", pct_drop * 100.0);
        assert!(
            pct_drop > 0.05,
            "per-gem level/quality override is NOT being respected by PoB \
             (baseline={:.2}, mutant={:.2}, drop={:.2}%). The mutation applier \
             needs a different mechanism to make builds score distinctly.",
            baseline.total_dps,
            mutated.total_dps,
            pct_drop * 100.0
        );
    }
}
