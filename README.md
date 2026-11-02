# ConsoleSilence

An ASI plugin for **Kingdom Come: Deliverance II** that decides what the engine console
shows. It exists for mods whose per-tick loop floods the console while the console is
open — the ERVWHDO / ENHANCED_Realistic LUT guard is the case it was written for.

It hooks two things, both on `IConsole`:

| Slot | Method | What it changes |
|---|---|---|
| 26 | `PrintLine(const char*)` | a matched line never enters the console buffer |
| 35 | `ExecuteString(cmd, silent, defer)` | a matched command runs with `silent = true`, so it executes but is not echoed |

Commands still execute, and **`kcd.log` is never touched** — the log keeps every line, so
support cases stay diagnosable.

```
$9[ERVWHDO LUT Guard] ...        <- 60x/second while the console is open, gone
everything else                  <- unchanged
kcd.log                          <- unchanged
```

## Install

1. An ASI loader in the game's `Bin\Win64MasterMasterSteamPGO\` directory
   ([Ultimate ASI Loader](https://github.com/ThirteenAG/Ultimate-ASI-Loader) as
   `dinput8.dll`).
2. Copy `ConsoleSilence.asi` next to `KingdomCome.exe`.
3. Start the game. There is nothing to configure.

The plugin writes no config file and filters nothing by itself. It produces two
diagnostic files next to the ASI: `consolesilence.log` and `ConsoleSilence.status.json`.
To uninstall, delete the `.asi` (and those two files).

## Enable it from a mod

A global Lua table `ConsoleSilence` is registered when the game loads its first script:

```lua
if ConsoleSilence then                                       -- users may not have the ASI
    ConsoleSilence.mute("r_ColorGradingChartImage")            -- literal text, case-insensitive
    ConsoleSilence.mute("[ERVWHDO LUT Guard]", { action = "throttle:5000" })  -- one per 5 s
end
```

Six functions: `mute` / `unmute` / `clear` / `list` / `stats` / `which`, plus three
switches `enabled` / `observe` / `fold`. Rules are session-scoped, capped at 64, and
never persisted — a mod calls the API each launch, which keeps a single source of truth.

- Contract, `opts` and error semantics: [docs/LUA_API.md](docs/LUA_API.md)
- Copy-paste guide for mod authors: [docs/FOR_MOD_AUTHORS.md](docs/FOR_MOD_AUTHORS.md)
- Why it is shaped this way: [docs/DESIGN.md](docs/DESIGN.md)

**Nothing is swallowed until a mod asks.** There is no default rule set, no config file
and no implicit folding: an out-of-the-box filter that decides on its own which lines are
noise is a support problem, not a feature.

## Finding the exact line (30 seconds)

```lua
ConsoleSilence.observe(true)          -- every console line lands in consolesilence.log
-- reproduce the spam, then:
ConsoleSilence.which("the exact words")   -- id of the rule that would swallow it, or nil
ConsoleSilence.mute("the exact words")
ConsoleSilence.observe(false)
```

`ConsoleSilence.status.json` (rewritten every 2 s) carries the counters — `seen`,
`dropped`, `passed`, `repeatSuppressed`, `commandsSilenced`, per-rule `ruleHits`,
`consoleOpen` and both hook addresses. If `dropped` stays 0 while the console spams,
either no rule matches or the line never passes through `PrintLine`.

Dropping a file named `ConsoleSilence.dump` into the Bin directory makes the plugin write
the console's own line buffer (last 400 lines, read through `IConsole::GetLineNo`) to
`ConsoleSilence.console.txt` — the ground truth for "what the console actually contains".

## Compatibility

- Verified against `WHGame.dll` 1.5.6 in a real session, with ERVWHDO, Better Rain and
  the KCD Native Menus dependency installed.
- `pConsole` is found by an anchor scan, so absolute addresses do not matter across game
  patches. Both vtable slots are validated at runtime (each must point inside
  `WHGame.dll`) and the hooks are re-installed if something overwrites them.
- If the engine changes the `IConsole` interface, the plugin refuses to hook and says so
  in `consolesilence.log` instead of guessing.

## Build

```powershell
cargo build --release --target x86_64-pc-windows-msvc
cargo test  --release --target x86_64-pc-windows-msvc     # 54 offline tests
cargo clippy --release --target x86_64-pc-windows-msvc    # zero-warning gate

pwsh -NoProfile -File tools\build-install.ps1              # build + deploy + hash check
pwsh -NoProfile -File tools\e2e.ps1                        # full in-game acceptance
```

`tools\build-install.ps1` and `tools\launch.ps1` take the game location from
`$env:KCD2_ROOT` / `$env:KCD2_BIN` (default: a standard Steam install). The acceptance
script additionally needs an external Lua console runner to execute code inside the
game; everything else runs offline.

Layout:

```
crates/kcd2-abi           host cage: FFI, module/RVA resolution, guarded memory reads,
                          the single vtable-write primitive, file log
crates/console-silence    the plugin
  console.rs              IConsole discovery, both hooks, install/verify/restore, dump
  filter.rs               snapshot publication point (single pointer, reader guard,
                          retirement queue) — no locks or allocations on the print path
  rules.rs                matchers: literal, wildcard, regex (compiled once)
  spec.rs / overlay.rs    rule declarations (owner, ttl) and the only write side
  bridge.rs + shim.lua    the Lua API
  repeat.rs               identical-line folding (off unless a mod enables it)
  status.rs               counters → status.json
```

Disassembly evidence for the vtable slots is in [docs/RE_FINDINGS.md](docs/RE_FINDINGS.md);
what the test layers do and do not prove is in [docs/TESTING.md](docs/TESTING.md).

## License

MIT — see [LICENSE](LICENSE).
