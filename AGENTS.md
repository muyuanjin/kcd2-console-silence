# ConsoleSilence — repository rules

An ASI plugin for Kingdom Come: Deliverance II that decides what the engine console
shows. Two hooks on `IConsole`: `PrintLine` (slot 26) drops matched lines, `ExecuteString`
(slot 35) runs matched commands with `bSilentMode = true` so they are not echoed.
`kcd.log` is never touched.

## Read first

| You are about to | Read |
|---|---|
| change code | this file, `README.md` (public contract), `docs/DESIGN.md` (why it is shaped this way) |
| change behaviour a player sees | `docs/LUA_API.md` — it is the public contract |
| verify something | `docs/TESTING.md` (what each layer proves, and what it does not) |
| re-derive an engine fact | `docs/RE_FINDINGS.md`, or `re/scripts/` (idalib probes) |

## Layout

```text
crates/kcd2-abi           host cage: FFI, module/RVA resolution, guarded memory reads,
                          the single vtable-write primitive, file log
crates/console-silence    the plugin (cdylib → ConsoleSilence.asi)
  console.rs              IConsole discovery, both hooks, install/verify/restore, dump
  filter.rs               snapshot publication: one pointer, reader guard, retirement queue
  rules.rs                literal / wildcard / regex matchers, compiled once per rule
  spec.rs                 rule declarations (owner, ttl) — plain data
  overlay.rs              the only write side: specs → compiled snapshot
  bridge.rs + shim.lua    the Lua API
  repeat.rs               identical-line folding (off unless a mod enables it)
  status.rs               counters → ConsoleSilence.status.json
tools/                    build-install / launch / e2e / lua-run / dependency-smoke
re/scripts/               idalib probes (vtable slots, string xrefs)
docs/                     public documentation
```

## Commands

```powershell
cargo test --release --target x86_64-pc-windows-msvc     # 54 offline tests
cargo clippy --release --target x86_64-pc-windows-msvc   # must stay at zero warnings
pwsh -NoProfile -File tools\build-install.ps1            # build + deploy + hash check
pwsh -NoProfile -File tools\e2e.ps1                      # launch game → 37 assertions → close
```

`KCD2_ROOT` / `KCD2_BIN` override the game location; both default to a standard Steam
install.

## Rules

1. **No configuration file, no default rules.** The plugin ships with zero rules and reads
   nothing from disk. A mod enables filtering at runtime through the Lua API. Do not add a
   built-in rule set, and do not add persistence.
2. **Never hook `ILog`.** The log is the only complete diagnostic channel; filtering it
   would hide the evidence users and mod authors need. Only the console copy is filtered.
3. **Never guess an address.** `pConsole` comes from an anchor scan; every vtable slot must
   point inside `WHGame.dll` before it is hooked, otherwise installation is refused and
   logged. All memory access goes through `kcd2_abi::mem` (guarded reads, never a raw
   dereference).
4. **The print path allocates nothing, locks nothing and never panics.** It runs on the
   game thread inside the engine's print path. The rule set is an immutable snapshot
   published by a single pointer swap; old snapshots are released by the supervisor thread
   once no reader holds them. Each hook carries its own unwind guard, because a panic
   escaping `extern "system"` aborts the process — a failing hook forwards the line
   instead.
5. **Lua VM state is only touched on the game thread.** Registration happens at the first
   `IScriptSystem::ExecuteBuffer` call, where the VM is not executing; the supervisor
   thread only retries installing the vtable hook, which touches no VM state.
6. **`status.json` is a snapshot** (every 2 s). Assertions must poll it; reading it once
   produces false failures.
7. **Crash evidence is identified by content, not size.** BugSplat writes a 3-byte
   placeholder per launch and a 75-byte teardown stub on forced termination; a real fault
   carries `MFA: WHGame!` or `Minidump file successfully saved`.
8. **Measure before claiming.** Anything in `docs/` marked unverified stays unverified
   until a run proves it. When a claim in the code and a claim in the docs disagree, the
   run wins.
