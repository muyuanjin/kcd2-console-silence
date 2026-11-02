# CONSOLE SILENCE — LUA API

A global table `ConsoleSilence` is registered when the game loads its first script, so it
is available inside a mod's init code.

```lua
local cs = ConsoleSilence

cs.mute(pattern [, opts]) -> id        -- stop these lines from appearing
cs.unmute(id) -> bool                  -- take it back (idempotent)
cs.clear([owner]) -> n                 -- remove rules, optionally only one owner's
cs.list() -> rows                      -- what is in effect right now
cs.stats() -> table                    -- counters
cs.which(line) -> id | nil             -- who eats this line? (read-only)

cs.enabled(bool) -> bool               -- master switch: false = swallow nothing
cs.observe(bool) -> bool               -- write every console line to the log
cs.fold(bool)    -> bool               -- collapse identical repeated lines (off by default)
```

## Quick start

```lua
-- shortest form: literal substring, matched line is dropped
ConsoleSilence.mute("r_ColorGradingChartImage")

-- keep the first line of a burst, fold the rest (status lines stay readable)
ConsoleSilence.mute("[MyMod] thread status", { action = "throttle:5000" })

-- wildcard: only * and ? are special, [ ] . ( ) are ordinary characters
ConsoleSilence.mute("*colorcharts*.dds", { kind = "wildcard" })

-- full regex, opt-in, compiled at call time; an invalid pattern throws here
ConsoleSilence.mute("chart.*\\.dds$", { kind = "regex", owner = "ERVWHDO" })
```

## `opts`

| Field | Values | Default | Meaning |
|---|---|---|---|
| `kind` | `contains` `prefix` `suffix` `equals` `wildcard` `regex` | `contains` | how a line is recognised |
| `action` | `"drop"` / `"throttle:<ms>"` | `drop` | swallow the line, or let one through per window |
| `owner` | any string | `""` | ownership; `clear(owner)` only removes your own rules |
| `ttl_ms` | milliseconds, `0` = forever | `0` | lifetime; expire the rule when the burst is over |

A bare string is **always** literal. Writing `3.a24.dds` as a pattern and getting a regex
would silently swallow far more than intended, and the effect is impossible to reproduce
after the fact.

## Contract

- **Commands still run.** A matched `ExecuteString` executes with `bSilentMode = true`;
  only the echo disappears.
- **`kcd.log` is untouched.** The filter only affects the console buffer.
- **Rules are session-scoped.** Nothing is written to disk; a mod calls the API each
  launch.
- **Cap of 64 rules.** `mute` throws (`full`) beyond that.
- **Repeating `mute` is idempotent.** The same `(owner, pattern)` returns the same id
  instead of adding a second rule, and **does not extend `ttl_ms`**; renew a lifetime with
  `clear(owner)` followed by `mute`.
- **`which()` is a mirror, not a probe.** It does not increment hit counters and does not
  advance a throttle window.
- **Matching is ASCII case-insensitive,** and only the first 512 bytes of a line are
  examined.

## Debugging

```lua
-- which rule would swallow this line?
local id = ConsoleSilence.which("$4[MyMod] guard stopped")
if id then
    for _, row in ipairs(ConsoleSilence.list()) do
        if row.id == id then print(row.kind, row.pattern, row.action, row.hits) end
    end
end
```

Nothing seems to be filtered when it should be? Compare with the filter off:

```lua
ConsoleSilence.enabled(false)         -- nothing is swallowed; hooks stay in place
ConsoleSilence.enabled(true)

for _, row in ipairs(ConsoleSilence.list()) do print(row.id, row.pattern, row.hits) end
```

`ConsoleSilence.observe(true)` writes **every** console line to `consolesilence.log`,
which answers "who is flooding the console" without guessing at patterns. It has a hard
budget (8 MB) and switches itself off when that is reached.

## When it is available

The table is registered on the game thread, at the first `IScriptSystem::ExecuteBuffer`
call — before mods initialise, so `mute` can be called from a mod's init code. A
registration failure is reported once in `consolesilence.log` and does not affect
filtering.

## Not provided

| Absent | Because |
|---|---|
| `onLine(fn)` callback | running Lua inside the print path costs a cross-VM call and an allocation per line, and can re-enter the console |
| `allow` / `priority` | first-match-wins with an idempotent `drop` is enough; whitelists put the flood back |
| writing a config file | two writers lose updates, and the file stops being reviewable |
| tuning the folding policy | folding is a global switch, not a per-rule decision |
