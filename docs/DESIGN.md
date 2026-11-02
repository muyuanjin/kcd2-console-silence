# DESIGN

What a rule is, what it is not, and which invariants the implementation has to keep.
This is the rationale behind the shape of the API and the hot path; the API reference
itself is [LUA_API.md](LUA_API.md).

## 1. The one job

**Decide what appears in the engine console.** Not what appears in `kcd.log` (that log is
the diagnostic lifeline and stays byte-for-byte complete), and not whether a command
executes (it must, or a mod's protection loop stops working).

> **I1 — Observation and execution are separate.** The plugin changes what is *visible*,
> never what *happens*.

Consequently: a matched `ExecuteString` flips `bSilentMode`, so the command still runs and
the engine simply does not echo it. A matched `PrintLine` never reaches the console
buffer. Neither path touches `ILog`.

## 2. Three concepts, not two

| Concept | What it is | What it is not |
|---|---|---|
| **Rule** | a matcher plus an action: *how to recognise* + *what to recognise* + *what to do* + a human label | not a callback, not code |
| **Line** | text the engine is about to put into the console buffer | not a command |
| **Command** | an instruction the engine is about to execute | not its output; the echo is a side effect of executing it |

Lines and commands share the same rule table but not the same action vocabulary:

- matched **line** → `drop`, or `throttle:<ms>` (let the first one per window through);
- matched **command** → silence the echo. There is no throttle for commands: a command
  either runs or does not, and this plugin never suppresses execution.

> **I2 — A rule can only subtract.** It removes something from the console; it never adds
> a line back, never reorders, never rewrites text.
>
> **I3 — Line state and command state are disjoint.** A rule that quietly swallowed a
> thousand lines must not make a *command* decision depend on that state. Commands match
> statelessly: no hit counters, no throttle window.

## 3. Where rules come from: one place, at runtime

```
mod Lua  ──mute/unmute/clear──▶  rule specs  ──compile──▶  immutable snapshot  ──▶  hooks
```

No file, no defaults, no layers. The plugin starts with zero rules, and a rule only exists
because a mod asked for it during this session.

> **I4 — One source of truth.** Rules live in memory only. A mod that wants them every
> launch calls the API every launch; a config file would be a second source of truth that
> can disagree with the first.
>
> **I5 — Nothing is implicit.** No default rule set, no implicit folding, no "smart"
> filtering. Whatever the console loses, a mod asked for explicitly.
>
> **I6 — Session scope.** Rules do not persist across launches and are not tied to saves.

## 4. Who the users are

| Role | Situation | What they need |
|---|---|---|
| **Mod author** | their own mod floods the console; they want it gone before release | one call that works, a hit counter, an immediate undo |
| **Player** | installs and forgets | to never see the API at all; the console should look untouched |
| **Diagnostician** | "which line is actually flooding?" | observe mode, and a way to ask "who eats this line?" |

> **I7 — A player never calls the API.** Its reason to exist is authoring and field
> debugging. With no Lua call, the plugin is behaviourally absent.

## 5. Hot-path invariants

Both hooks run synchronously on the game thread inside the engine's print path: a few
lines per second normally, 60+ per second while something floods.

> **I8 — The print path does not lock, allocate, format, compile or panic.** It reads one
> pointer, matches bytes, and touches two atomics.
>
> **I9 — The rule set is an immutable snapshot.** A mutation builds a new snapshot and
> publishes it with a single pointer swap; the print path never sees a half-built table.
> The old snapshot is released later, by a thread that is not the hook, once no reader
> holds it — `RegexSet` destruction is two orders of magnitude more expensive than a match.
>
> **I10 — Cheap matchers first.** Literal comparison runs before the compiled automaton;
> wildcards and regexes are compiled once, at `mute` time, and are rejected there if the
> pattern is invalid.

Both hooks also carry their own unwind guard: a panic escaping an `extern "system"`
function aborts the process, so a hook that fails fails *open* (the line is forwarded).

## 6. What is deliberately absent

| Not provided | Why |
|---|---|
| a per-line Lua callback | running the VM inside the print path costs a cross-VM call and an allocation per line, and can re-enter the console |
| `allow` / `priority` / `replace` | a whitelist puts the flood back; ordering rules turn "why did my rule not apply" into a sorting exercise. Matching is first-match-wins, `drop` is idempotent |
| writing the config from Lua | two writers lose updates, and the file stops being reviewable |
| persistence of any kind | see I4 |
