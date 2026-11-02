# FOR MOD AUTHORS

ConsoleSilence only decides what the **engine console** shows. `kcd.log` is untouched and
every console command still executes — a matched command simply stops being echoed.

The plugin ships with **zero rules** and reads no configuration, so it changes nothing
until a mod asks it to. Silencing your own spam costs two lines.

---

## Shortest version

Wherever your script runs once at load time (top level, or your `Init`):

```lua
if ConsoleSilence then
    ConsoleSilence.mute("r_ColorGradingChartImage")
end
```

Keep the `if ConsoleSilence then` guard: users may not have the ASI installed, and
`ConsoleSilence` is `nil` for them.

A bare string means **"any line containing this text is not displayed"**, ASCII
case-insensitive.

---

## The LUT-guard case

A guard has a window: it starts when the filter changes and stops after at most 25
minutes. Let the rule follow the window:

```lua
if ConsoleSilence then
    -- while the guard re-applies the chart, these lines stay quiet;
    -- the rule expires on its own after 25 minutes.
    ConsoleSilence.mute("r_ColorGradingChartImage", { owner = "ERVWHDO", ttl_ms = 1500000 })
    ConsoleSilence.mute("Unsupported color chart dimensions", { owner = "ERVWHDO" })
end
```

- `owner` scopes removal: `clear("ERVWHDO")` only touches your own rules.
- `ttl_ms` is a lifetime. The rule expires by itself, so outside the window the same line
  becomes visible again — which is what you want if it signals a real failure.

If a status line should stay visible but must not repeat 60 times a second, throttle it:

```lua
ConsoleSilence.mute("[ERVWHDO LUT Guard]", { action = "throttle:5000", owner = "ERVWHDO" })
```

`throttle:5000` lets the first line per 5 seconds through and folds the rest.

---

## Arguments

```lua
local id = ConsoleSilence.mute(pattern [, opts])
```

| `opts` field | Values | Default | Meaning |
|---|---|---|---|
| `kind` | `contains` `prefix` `suffix` `equals` `wildcard` `regex` | `contains` | how a line is recognised |
| `action` | `"drop"` / `"throttle:<ms>"` | `drop` | swallow it, or let one through per window |
| `owner` | string | `""` | ownership, used by `clear(owner)` |
| `ttl_ms` | milliseconds, `0` = forever | `0` | lifetime |

The other entry points:

```lua
ConsoleSilence.unmute(id)          -- undo (idempotent)
ConsoleSilence.clear("ERVWHDO")    -- remove all of your rules, returns the count
ConsoleSilence.list()              -- rules in effect, with hit counts
ConsoleSilence.stats()             -- seen / dropped / passed / commandsSilenced ...
ConsoleSilence.which(line)         -- id of the rule that would swallow it, or nil
ConsoleSilence.enabled(false)      -- master switch: swallow nothing (A/B comparison)
ConsoleSilence.observe(true)       -- every console line into consolesilence.log
ConsoleSilence.fold(true)          -- collapse identical repeated lines
```

## Three things to get right

1. **A bare string is always literal.** Regexes must be explicit: `{ kind = "regex" }`.
   The pattern is compiled at `mute` time, so a typo throws immediately instead of failing
   silently later.
2. **In `wildcard`, only `*` and `?` are special** — `[ ] . ( )` are ordinary characters.
   `ConsoleSilence.mute("[ERVWHDO LUT Guard]*", { kind = "wildcard" })` matches the
   bracket literally, while the same string as a regex is a character class.
3. **Call `mute` at init and on state changes, not every frame.** Repeating the same
   `(owner, pattern)` is idempotent and returns the same id, but there is no reason to
   call it per tick.

## When you are not sure what to silence

```lua
-- paste the console line verbatim and ask who would swallow it
local id = ConsoleSilence.which("$4[ERVWHDO LUT Guard] stopped")
print(id)
```

With no rules yet, turn on observe mode and look at what is actually flooding:

```lua
ConsoleSilence.observe(true)       -- every line lands in consolesilence.log
ConsoleSilence.observe(false)      -- remember to switch it off (8 MB budget)
```

---

## What it never does

- does not modify `kcd.log` — support logs stay complete;
- does not stop commands — a matched command still takes effect, it is only not echoed;
- does not persist anything — rules added at runtime are gone after the session, so add
  them from your Lua each launch.
