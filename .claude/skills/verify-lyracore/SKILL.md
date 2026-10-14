---
name: verify-lyracore
description: Launch a local LyraCore Realm and drive it the way a player does, with the Benilla 1.12.1 client, the Headless Client smoke and `spacetime sql` for side effects. Use when a change must be proven in a running Realm or a real client, not only in cargo tests.
---

# Verify LyraCore

A player touches LyraCore through a 1.12.1 client. The Realm is a Gateway (logon `:3724`, world
`:8085`) in front of SpacetimeDB (`:3000`), where the Module holds all game state. You prove a
change in three layers:

1. **Benilla**: a scripted 1.12.1 client. It logs in, sends chat and GM commands, runs Lua in the
   stock UI, and takes screenshots. This is the user path.
2. **`./lyracore dev smoke`**: the pinned Headless Client. It proves logon, world entry and
   enumeration over the wire, without a display.
3. **`spacetime sql`**: the rows the action wrote. Use it for side effects, never as the action.

The session tells you the feature: the command or UI action, the steps, and the expected end state.
This skill is the recipe you apply to it.

## Targets

- **Local dev stack** (default). One stack per machine. The ports `3000`, `3724` and `8085` are
  fixed, and every checkout shares the SpacetimeDB data dir and the database names. A publish from
  a worktree replaces the Module that another checkout published.
- **The live test realm**: only when the user tells you to use it. The host, SSH access and test
  accounts are in the operator's private instructions; never write them into tracked files,
  commits, issues, PRs or published evidence. Do not run `./lyracore dev …` or `publish` against
  it. For server-side reads there, use the `lyracore-operator` skill. A Benilla login kicks
  whoever holds that account, so use only an account the user names for testing.

## Launch (local)

Run from the LyraCore checkout under test. Wrap builds in the build lock; `flock -o` keeps the
started daemons from holding it.

```bash
./lyracore dev status        # record this; if it was already up, you do not own it
./lyracore packages list     # an installed Package is part of the Module build
flock -o ~/.cache/lyracore-build.lock ./lyracore dev up
```

Ready means `dev up` exits 0 and `dev status` shows `gateway  healthy` and every database
`published, gateway connected`. A cold build takes several minutes. `dev status` also prints the
topology: `single` puts everything, Realm-core included, in database `lyracore`; `sharded` uses
`lyracore`, `lyracore-kalimdor`, `lyracore-instances` and `lyracore-realm`.

Then provision the fixture account and give its character GM, so `.tele`, `.gps`, `.level`,
`.additem` and the rest work (`module/src/gm.rs` lists them). `account create` on an existing
account resets its password.

```bash
printf 'test123' | ./lyracore account create TEST --password-stdin
./lyracore character gm Tester true
```

`TEST` owns `Tester`, a level 1 Human Warrior in Northshire. Benilla can play the seed fixture.
`dev smoke` and `dev check` cannot: they ask the Module's `check_world_content`, which refuses until
`./lyracore import` completes (`no completed world import for map N`). An import needs a 1.12.1
client's `Data/` and runs for tens of minutes; run it only when the feature needs imported
content.

If `dev up` fails to compile `packages/<name>/`, the installed Package is older than core.
`./lyracore packages list` shows its revision and any `LOCALLY DRIFTED` state. The official
collection tag can lag core `main`; a Package folder from an up-to-date `packages` checkout
(`./lyracore packages add <folder>`) is the usual fix. Fixing it changes the user's checkout, so
report it and ask first.

## Doctor

Run this first, and again whenever anything looks wrong. It changes nothing.

```bash
./lyracore dev status
ss -ltnp | grep -E ':(3000|3724|8085)\b'
pid=$(ss -ltnp | sed -n 's/.*:8085 .*pid=\([0-9]*\).*/\1/p'); readlink "/proc/$pid/cwd"
spacetime sql --server http://127.0.0.1:3000 lyracore "SELECT name, level, map_id, online FROM game_character"
```

Worth driving when: all three ports listen, the Gateway's cwd is this checkout (anything else
means another checkout owns the stack; stop and ask), and `game_character` answers with `Tester`.
`./lyracore dev logs gateway` shows the Gateway log.

## Drive

### Benilla (the user path)

`drive-benilla.sh` builds Benilla, runs one unattended session, and writes the evidence. Set
`BENILLA_DIR` (a Benilla checkout) and `WOW_DATA` (a 1.12.1 client's `Data/` directory); the
operator's private instructions name both. Benilla opens a window, so it needs a display.

```bash
EVIDENCE=/tmp/lyracore-verify/goldshire \
WOW_USER=TEST WOW_PASS=test123 WOW_CHAR=Tester \
WOW_PROBE_CHAT='.gps;.tele goldshire;.gps' WOW_PROBE_CHAT_AT=12 WOW_PROBE_CHAT_EVERY=6 \
WOW_PROBE_LUA='ProbeLog("zone=" .. GetZoneText())' WOW_PROBE_LUA_AT=35 \
WOW_LIVE_SHOT_AT=38 \
  .claude/skills/verify-lyracore/drive-benilla.sh 45
```

The argument is the run length in seconds. That run prints, among its log lines:

```text
net: server says — Position: map 0 | x -8949.95 y -132.49 z 83.53 | o 0.000
net: server says — Position: map 0 | x -9339.46 y 171.41 z 61.56 | o 0.000
probe-log: zone=Elwynn Forest
```

No `probe-log:` line means the run never reached the world: read `benilla.log` for the login
error. The actuation channels, all timed in seconds after world entry:

| Variable | Does |
| --- | --- |
| `WOW_PROBE_CHAT="<line>;<line>"` | Sends each line as chat: GM dot-commands, `/say`, slash commands. `_AT` (default 8) and `_EVERY` space them. Replies log as `server says`. |
| `WOW_PROBE_LUA="<chunk>"` | Runs Lua in the live UI at `_AT` (default 10). Call the stock API the way a button does (`AcceptQuest()`, `StaticPopup_OnClick(StaticPopup1, 1)`) and report with `ProbeLog("…")`. |
| `WOW_PROBE_KEY="<key>@<secs>[:<hold>]"` | Presses keys (`W@15:2` walks forward for two seconds). |
| `WOW_LIVE_SHOT_AT=<secs>` | When the screenshot is taken (default 12). `WOW_SHOT_REQUIRE=<unit name>` waits until that unit is in frame. |
| `WOW_MOVE_TRACE=<file>` | Logs movement and packets (`snd`/`rly` lines), about 6 MB for 45 s. Put it under `$EVIDENCE`; `WOW_MOVE_TRACE_TAGS="move,snd"` cuts it down. |

Benilla prints the full probe table with `WOW_PROBE=list` and documents the rest in its
`docs/CONTRIBUTING.md`. Prefer Lua over pointer input: a UI click through `WOW_PROBE_HOVER` or
`WOW_PROBE_DRAG` depends on frame layout. Two clients (trade, party, chat) means two accounts and two
runs in parallel, with `EVIDENCE` set apart; see Benilla's `scripts/summon-live.sh` for the shape.

### Headless Client

```bash
printf 'test123\n' | ./lyracore dev smoke TEST Tester --password-stdin
```

It runs the content check before it reads the password. On the seed fixture it stops there with
`no completed world import for map N`, so it proves nothing until an import completes.

### Side effects

Query the database that owns the row: the Character's Home Shard for world state, Realm-core for
accounts and sessions. Read the table and column names in `docs/schema.md` or the `#[table]`
struct. Always pass `--server http://127.0.0.1:3000`; the CLI's default server can be maincloud.
`spacetime sql` has no `ORDER BY`, `IN` or subqueries, and the Gateway persists a logout one to
three seconds late, so poll.

```bash
spacetime sql --server http://127.0.0.1:3000 lyracore \
  "SELECT guid, name, map_id, zone_id, x, y, z, online FROM game_character WHERE name = 'Tester'" \
  > "$EVIDENCE/sql-after.txt"
```

Do not use `spacetime call debug_*` reducers to perform the action under test. They may stage a
fixture before the run; say so in the evidence.

## Evidence

Everything for one proof goes in one `$EVIDENCE` directory under `/tmp/lyracore-verify/`:

- `benilla.log`: the full client log; `probe.log`: its `probe-log:`, `server says` and `ERROR`
  lines.
- `shot.png`: the screen after the action. Look at it: a login screen or a black frame is a
  failed run, not a proof.
- The `spacetime sql` output before and after the action, saved as files.
- `./lyracore dev logs gateway` lines for the window of the run, when the Gateway matters.

A proof shows the action and the state it produced, on the path a player takes. "Teleported" needs
the `.gps` reply before and after, the zone the UI reports, and the `game_character` position. A
system message alone is not enough. For a refusal, capture the refusal message and show that
the row did not change.

## Cleanup

- Benilla exits by itself at the run length; `timeout` stops a stuck run. Stop any other client
  only by the PID you started. Never kill by process name.
- If `dev status` showed the stack down before you started, run `./lyracore dev down`. If
  `dev up` failed halfway, `dev status` reports SpacetimeDB as `external` and `dev down` leaves
  it; stop that node by the PID `ss -ltnp` shows on `:3000`, after you check that its start time
  falls inside your run (`ps -o lstart= -p <pid>`).
- Leave the fixture account and its Characters; `dev up` never clears them, and the next run reuses
  them.
- Keep `$EVIDENCE`. Cleanup never deletes it.
