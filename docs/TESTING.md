# TESTING

Two layers, and what each one can and cannot prove. Offline tests do not prove "it works
in the game"; one in-game run does not prove "it works on another build".

## 1. Offline — `cargo test`

```powershell
cargo test --release --target x86_64-pc-windows-msvc
```

54 tests: 17 in `kcd2-abi` (guarded memory reads, the pointer-write primitive, RVA
bounds, the panic guard) and 37 in the plugin.

| Module | Invariants under test |
|---|---|
| `rules` | `contains` / `prefix` / `suffix` / `equals` semantics, ASCII case-insensitivity, first match wins, `scan_limit` really bounds the inspected prefix, an empty pattern never matches, wildcard treats `[ ] . ( )` as literals, an invalid regex is rejected with a reason, ids stay stable when other rules are removed |
| `spec` / `overlay` | a fresh install has zero rules; adding a rule takes effect immediately; `(owner, pattern)` is idempotent; `clear(owner)` only removes that owner's rules; TTL expiry removes exactly the expired rule; `which()` does not bump hit counters; the 64-rule cap; the master switch passes everything |
| `repeat` | folding starts at the configured repeat count, different texts do not interfere, the window restarts the count, a disabled policy never folds |
| `filter` | decisions are three-state (`pass` / `drop` / `fold`); commands and `which()` match without touching counters or throttle windows; retired snapshots are reclaimed only when no reader holds them |
| `status` | the JSON is parseable, escapes quotes and backslashes, counters accumulate |
| `bridge` | parameter readers see the type they request; a string argument stays alive across the call |

This layer proves "the policy is what it claims to be". It cannot prove "the hook is
installed at the right address".

## 2. In the game — `tools/e2e.ps1`

```powershell
pwsh -NoProfile -File tools\e2e.ps1                # launch → assert → close
pwsh -NoProfile -File tools\e2e.ps1 -SkipLaunch    # reuse a running session
```

Prerequisites (the script asserts them and fails loudly if one is missing):

- the game, with an ASI loader at `Bin\Win64MasterMasterSteamPGO\dinput8.dll`;
- `Mods\ERVWHDO` (the mod that motivated this plugin) plus its `KCD2 Native Menus`
  dependency;
- an external Lua console runner, used only to execute code inside the game — synthetic
  keyboard and mouse input is swallowed by the game, so it is the only reliable channel.

Channels:

| Direction | Mechanism |
|---|---|
| run code in the game | `tools/lua-run.ps1` → the runner's TCP endpoint (`127.0.0.1:28771`) |
| read "what the console contains" | drop a `ConsoleSilence.dump` trigger → the plugin writes the console ring buffer (via `IConsole::GetLineNo`) to `ConsoleSilence.console.txt` |
| read the plugin's own account | `ConsoleSilence.status.json`, a snapshot every 2 s |
| read the log side | `kcd.log` (incremental) and `consolesilence.log` |

### 37 assertions

| Group | What it asserts | Why it is not vacuous |
|---|---|---|
| environment | ASI present, ERVWHDO and its dependency present, Lua runner reachable | a missing piece makes every later assertion meaningless |
| T1 hooks | `hookState = installed`; `hookOriginal` lies inside `WHGame.dll` (observed RVA `0x8D9348`, the thunk behind vtable slot 26); `ruleCount = 0` | the address must be the target stored at `vtable + 26*8`, otherwise the wrong slot was hooked |
| T2 pass-through | a line matching no rule **does** appear in the console; a line matching a *future* rule appears too | this is the denominator for every "it disappeared" assertion |
| T3 rule drop | `mute(...)` then 30 matching lines → `dropped` grows by 25+, none survive in the dump; the same 30 lines are still in `kcd.log` | proves the console copy is filtered and the log is not |
| T4 folding | after `fold(true)`, 40 identical lines → `repeatSuppressed` grows by 25+, ≤3 survive, one `[ConsoleSilence] folded N ...` summary appears | folding is text-agnostic, so it covers a flood whose wording is unknown |
| T5 negative control | 5 mutually distinct lines all survive | stops "folding" from degenerating into "swallow everything" |
| T6 real path | `ERVWHDO:SetLUTSettings(chart)` (what the menu button calls) arms the guard (1500000 ms); over 20 s no chart line is added, `dropped` does not move, the process stays alive | reproduces the author's trigger; on this build that path produces no line by itself, so the assertion is "the guard really ran, added nothing, and nothing was over-swallowed" |
| T7b Lua surface | table registered; `mute` returns an id; `which` sees it; 12 tagged lines are swallowed; `list` reports it; `unmute` lets them through; `clear(owner)` removes them and leaves nothing behind | exercises the whole CRUD surface through the game's own script system |
| T8 stability | after 45 s the process is alive, no BugSplat report carries a fault, `consolesilence.log` has no `[ERROR]`, the hook is still installed, `rearmed = 0` | `rearmed` catches the silent failure mode where another ASI steals the slot |

A crash report is identified by content, not by size: BugSplat writes a 3-byte placeholder
on every launch, and a 75-byte `Entered Unhandled Exception Filter` stub when the process is
force-terminated. A real fault carries `MFA: WHGame!xxxx` or
`Minidump file successfully saved`.

### Evidence

`re/out/e2e-<timestamp>/`: `results.txt` (one PASS/FAIL per assertion), `status.json`, the
console dump, the tail of both logs, and every `probe-*.lua` the run executed.

## 3. Dependency smoke — `tools/dependency-smoke.ps1`

The Lua runner used by the end-to-end suite is a build of the `kcd2db` harness that
exposes a TCP endpoint; the packaged release of that mod does not. Since the mod is a
runtime dependency of ERVWHDO, it gets its own short run (7 assertions, ~40 s, separate
launch):

```powershell
pwsh -NoProfile -File tools\dependency-smoke.ps1 -Asi <path to kcd2db.asi>
```

Assertions come from its log, not from guessing: the SQLite database opens, the settings
entries load, the Lua method registers, ERVWHDO restores its settings through the database,
no Lua or DB error is logged, and its `PrintLine` hook coexists with this plugin.

## 4. What is *not* verified

- **The exact line the LUT guard produces.** No sample of the original wording is
  available, and this build does not reproduce a per-tick console line through the guard
  path (see `RE_FINDINGS.md` §3). Folding plus literal rules cover the flood without
  matching a string nobody has seen.
- **`PrintLinePlus`.** Its call surface was not measured, so it is not hooked.
- **Other game builds.** Only 1.5.6 was exercised. `re/scripts/vtable_probe.py` re-checks a
  slot's semantics in one command on a new build; `gEnv` and `pConsole` are found by
  anchor scan, so they are not hard-coded per version.
- **Window modes and launch flags.** The suite drives `PrintLine` and the ring buffer and
  should be independent of window shape, but that was not measured, so it is not claimed.
