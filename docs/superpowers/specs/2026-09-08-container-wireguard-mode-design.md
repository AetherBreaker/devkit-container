# devkit-container: supervisor, wireguard mode, heartbeat healthcheck, and the template language

Date: 2026-09-08; rewritten 2026-09-10 after review. Status: approved. One implementation plan
covers every repo in section 1; the `aeth_ext` part lands on a branch and a PR, unmerged.

Consuming project's view: `ScheduledReportAggregator/docs/superpowers/specs/2026-09-08-wireguard-db-access-design.md`
(the tunnel lives inside the app container). Its monitoring table changes with section 7: the
tunnel's Pushover path is the healthchecks.io integration, and the `aeth_ext` status-file reader it
lists as follow-on work no longer exists.

## 1. Boundary

| Repo | Changes |
| --- | --- |
| `aeth-devkit` (`setup`) | the template language of section 2, replacing the `setup-project:` markers, the structural `if-dep` gates, the hard-coded `aeth-ext` gate and the `RULES` table; the compose template read from the `devkit_container` package; the generic HEAD-vs-working-copy refusal |
| `devkit-templates` | every marker rewritten in the new syntax; the compose template removed |
| `devkit-container` | the compose template as package data beside the Dockerfile; the `supervise` and `wireguard` switches; the supervisor; the tunnel; the `healthcheck` subcommand; the ping; the Dockerfile `wireguard` block; the `[tool.docker]` schema doc; tests |
| `aeth_ext` | `HEARTBEAT_SLUG` from the environment; the periodic ping stands down under `DEVKIT_SUPERVISED_PING` |

The split spec's contract table changes in one row: the compose template joins the Dockerfile
template under `devkit-container`'s ownership. The template language stays `setup`'s.

The hub, the office PC and the test project are the companion spec's. The plan runs against
sibling checkouts under one workspace folder; a repo that is not cloned on the executing machine
(`devkit-templates` is not, on the machine this was written on) is cloned first per
`aeth-devkit/WORKSPACE.md`, decided at execution time, not assumed.

## 2. Template language

Why it changes: this spec needs a gate on a `[tool.docker]` key, a compound condition, a rule for
a new compose key, and a gate on the Dockerfile. In today's `setup` each of those is a hard-coded
edit (a context flag, a gate name, a `RULES` entry, a gating call), and every future key would
need the same four edits and a devkit release. The language below moves that knowledge into the
templates, so `setup` owns the grammar and the template owner owns the conditions.

### 2.1 Markers

A marker is a comment whose text starts with `!`, using the file's own comment syntax: `# !…` in
YAML, TOML, Dockerfiles, `.env`, `.gitignore` and `.gitattributes`; `<!-- !… -->` in markdown;
`// !…` in JSONC. The space before `!` is mandatory in `#` files: `#!` is a shebang and is never a
marker. Markers match at any indentation, since editors reformat templates on save and comments
move with their block. Every marker is removed from the rendered output. A marker whose word
is not `if`, `end`, `service-block` or `rule` is a render error, so a typo cannot pass through
as an ordinary comment.

### 2.2 Blocks

```yaml
# !if <expr>:                  opens an explicit block; closed by an end
# !if <expr> as <label>:       the same, named for a targeted end
# S!if <expr>:                 opens a structural block; no end line (2.3)
<content line>  # !if <expr>   a one-line block: gates that line only
# !end                         closes the innermost open explicit block
# !end <name>                  closes the named block and every block inside it
<content line>  # !end [name]  the same, trailing the last line of the block
```

A block's name is its `as` label, else its expression text verbatim, trimmed. Blocks nest without
limit. The colon distinguishes a block opener from a one-liner: an opener on its own line without
a colon, or a trailing opener with one, is an error.

### 2.3 Structural blocks

`S!if` gates the next structural unit of the file, starting at the next non-marker line:

| File | Unit |
| --- | --- |
| TOML | to the line before the next table header (`[…]` or `[[…]]`) |
| markdown | the heading and its section, to the line before the next heading of the same or a higher level; headings inside fenced code do not count |
| YAML | the next content line and every following line indented deeper than it |
| Dockerfile | the next instruction and its `\` continuation lines |

Explicit blocks opened inside a structural block must close before the unit ends. A structural
block has no end line, so an `end` naming one is an error. Editors re-indent comments but not
content, and the unit is found from content, so formatting a template cannot change a structural
block's extent.

### 2.4 Expressions

The text between `if` and the colon is a Python expression, evaluated for truthiness. Three kinds
of name are available, resolved by `setup` before evaluation:

- `keys.<path>`: a value from the project's `pyproject.toml`, the path written with TOML key names
  verbatim, hyphens included (`keys.tool.ruff.lint.per-file-ignores`). TOML keys are not Python
  identifiers, so `setup` rewrites each path occurrence to a generated input variable holding the
  value: tables become dicts, arrays lists, strings, integers, floats and booleans themselves,
  datetimes strings, and a missing path `None`. Item syntax (`keys["tool"]`) is not supported.
- `dep("<name>")`: `True` when the project depends on the package, in `[project].dependencies`
  or any dependency group, or is that package. The argument must be a string literal; `setup`
  resolves the call to a boolean input the same way.
- bare flags for facts not in `pyproject.toml`: `rust` (a `Cargo.toml` at the root) and
  `publish_index` (an index with a publish URL). The plan checks each existing gate's predicate in
  `setup` and adds a flag only where a predicate cannot be expressed through `keys`.

Everything else is Python as Python defines it: `not`, `and`, `or`, comparisons, `in`, string
methods, `any`, `all`, parentheses. Examples:

```yaml
# !if keys.tool.docker.wireguard:
# !if dep("aeth-ext") and keys.project.name != "aeth-ext":
# !if "smoke" in (keys.tool.docker.services or ()):
# S!if dep("mypy"):
- FOO=bar  # !if not publish_index
```

An expression that fails to parse, raises, or names something unknown is a render error naming
the template, the line and the message. Unknown names are never silently false: today an old
devkit renders an unknown gate as absent, and this spec's own Dockerfile block would have
vanished that way. The split spec's constraint machinery (a templates package declares the devkit
it needs) is what makes the hard error safe.

### 2.5 Evaluation

Gate text is static, so `setup` sweeps every template it will render, collects each distinct
expression, resolves the names, evaluates the list once, and renders from the answers. The
resolver also yields, per expression, the set of `keys` paths it read, known statically from the
rewrite.

The evaluator is the `monty` crate (pydantic's Python-subset interpreter in Rust: Ruff's parser, its
own VM, no CPython, no C toolchain), pinned exactly, behind a one-function seam in its own module:
a list of expressions, each with its named inputs, in; a truth value or an error per
expression out. Nothing outside that module names a Monty type. The seam exists so that `rustpython-vm` can
replace Monty if the gates ever want more Python than the subset offers: that swap is a rewrite
of the one module and a Cargo change, with the module's tests as the acceptance suite.

**The refusal.** A committing run merges into HEAD's `pyproject.toml` but renders from the working
copy's, so any value the render depended on must agree between the two. `cli` today compares
`[tool.docker].services` by name. It now compares `services` plus every `keys` path any evaluated
gate read, and refuses naming the first path that differs. No key is special-cased.

### 2.6 Compose annotations

Two markers are read by the compose scaffold parser after gating, not by the gate pass:

- `# !service-block:` … `# !end service-block` delimit the per-service block, as
  `service-block` / `end-service-block` do today.
- `# !rule <kind>` on the line above a key declares how the rule engine treats that key in an
  existing compose file: `exact`, `presence`, `repo`, `volume-target`, `env-keys` or `exact-list`,
  the kinds `compose_rules.rs` implements today. The `RULES` table is deleted; the engine reads the
  kinds from the rendered scaffold, keyed by the annotated key's path. A key without an annotation
  is rendered into new files and never enforced on existing ones. A `rule` line must be followed
  by the key it annotates: to gate a key together with its rule, put both inside the block. The
  top-level `networks.coolify.external` handling stays as it is; annotations apply inside the
  service block only.

### 2.7 Migration

Every `setup-project:` marker in `devkit-templates` and in `setup`'s fixtures is rewritten:
`if-<flag>` → `!if <expr>:` with an `end`, `if-no-<flag>` → `!if not <expr>:`, `if-dep X` above a
table or heading → `S!if dep("X"):`, `if-aeth-ext` → `dep("aeth-ext")`, `if-docker-services` and
`if-docker` → `keys.tool.docker.services` where that is their predicate, `service-block` →
`!service-block:`. The compose template in `devkit-templates` gains its `rule` annotations in
the same rewrite, with today's content otherwise unchanged, since the `RULES` table goes with the
same devkit release. The structural gating in `md_block.rs` and `toml_merge.rs`, the `aeth-ext`
predicate in `scaffold.rs`, the gate call sites in `lib.rs`, and `refuse_uncommitted_services` are
replaced by the one gate pass, the one resolver and the generic refusal.

No dual-syntax period: the devkit release that reads the new syntax and the templates release
that uses it ship together, the templates package raising its `aeth-devkit` floor to that devkit.
`aeth-devkit`'s README gains the language reference, the content of 2.1 to 2.6.

## 3. Compose template ownership

`compose.template.yaml` moves into the `devkit_container` package beside `template.Dockerfile`.
`scaffold::load` reads it from the installed package through the same lookup `static_files::render`
uses, falling back to the templates package until a release of `devkit-container` that carries it
exists (section 10 removes the fallback). The argument is the one that moved the Dockerfile: every
line this spec adds to the compose file is a fact about the binary (the healthcheck subcommand and
its flags, `HEARTBEAT_SLUG`, the `WG_*` contract, `cap_add`, a `start_period` derived from the
handshake timeout), and facts about the binary version with the binary. The `.dockerignore`
templates stay in `devkit-templates`; they are not the binary's contract.

## 4. Switches

Both under `[tool.docker]` in `pyproject.toml`, read by the binary at `run` (the image carries
the file).

- `supervise = true`: the entrypoint spawns and supervises the app instead of exec'ing it
  (section 5). Off by default: an app that never spawns a child gains nothing from a reaper and
  should not pay for one.
- `wireguard = true`: the tunnel (section 6). Implies `supervise`. Also read by the compose and
  Dockerfile gates, so the refusal in 2.5 covers it with no special case.

Both off is the entrypoint's behaviour today, exactly: `exec`, the app is PID 1. The one addition on that path is
removing a leftover `wireguard-heartbeat.txt` (section 7) before the exec.

## 5. Entrypoint

In order: root check; launch script; mount check; the tunnel (section 6) when on; `prepare`; then
one branch. The mount check precedes the tunnel because it is a pure read that fails in
milliseconds, and a missing volume should not wait out a handshake timeout. `prepare` follows the
tunnel so every check still happens before the filesystem is touched.

**Exec** (neither switch): `setgroups`, `setgid`, `setuid`, `exec`, as today.

**Supervise**: spawn `/app/.venv/bin/<run-app-*>` as 999:999 with empty supplementary groups and
empty capability sets, via `pre_exec` doing what the exec path does. The supervisor stays PID 1.
Without `wireguard` it drops to 999 itself before spawning, so no root process lingers; with
`wireguard` it stays root because re-upping the tunnel needs `NET_ADMIN`. Duties:

- reap zombies; forward `SIGTERM`, `SIGINT` and `SIGHUP` to the child;
- every `WG_POLL_SECS` (default 30, the name is kept in both modes): the tunnel check (section 6)
  when on, then the heartbeat adjudication and the ping (section 7);
- on child exit: bring `wg0` down if up, send `/fail` on a nonzero exit (section 7), exit with the
  child's code, signal death as 128+n.

The child's environment is the supervisor's minus `WG_PRIVATE_KEY` and `WG_PEER_PRESHARED_KEY`,
plus `DEVKIT_SUPERVISED_PING=1` when the supervisor owns the ping (section 7). A failure reading
the environment names the variable, never the value.

## 6. Wireguard mode

Root phase, in this order:

1. **Preflight.** `wg` and `ip` on PATH, else refused with "image built without the wireguard
   block; rerun setup-project with a devkit that knows it". `WG_PERSISTENT_KEEPALIVE` nonzero
   (default 25), else refused: with no keepalive and an idle app WireGuard never re-handshakes,
   so every poll after `WG_STALE_SECS` would read as stale.
2. **Bring-up** with `ip link add wg0 type wireguard`, `wg set` (keys over stdin, never a file),
   `ip address add`, `ip link set up`, one `ip route add` per allowed IP. No `wg-quick`, no config
   file. The public key derived from `WG_PRIVATE_KEY` (`wg pubkey`, stdin again) is logged every
   start: not a secret, and what the hub operator enrols.
3. **First handshake** within `WG_HANDSHAKE_TIMEOUT_SECS` (default 60), else a refused start
   naming the endpoint. With `restart: no` that leaves the container down until a redeploy;
   accepted, because "the app never runs without its tunnel" is the invariant, and a deploy that
   fails loudly is the right signal for a hub that is not there.

Per poll: `wg show wg0 latest-handshakes`. A handshake older than `WG_STALE_SECS` (default 180,
WireGuard's own reject-after-time) is stale. First response: re-set the peer endpoint (`wg set
wg0 peer … endpoint …`), which re-resolves its DNS; the companion spec lets the hub move hosts.
If the next poll is still stale: `wg0` down and up. Every re-up is logged.

Environment contract. The `[tool.docker]` schema doc in the README records it.

| Variable | Meaning | Required |
| --- | --- | --- |
| `WG_PRIVATE_KEY` | this peer's private key; secret | yes |
| `WG_ADDRESS` | this peer's tunnel address, CIDR (`10.8.0.20/32`) | yes |
| `WG_PEER_PUBLIC_KEY` | the hub's public key | yes |
| `WG_PEER_ENDPOINT` | `host:port` of the hub | yes |
| `WG_PEER_ALLOWED_IPS` | comma-separated CIDRs routed through the hub | yes |
| `WG_PEER_PRESHARED_KEY` | secret | no |
| `WG_PERSISTENT_KEEPALIVE` | seconds; default 25; zero refused | no |
| `WG_HANDSHAKE_TIMEOUT_SECS`, `WG_POLL_SECS`, `WG_STALE_SECS` | defaults 60, 30, 180 | no |

Empty is read as unset.

## 7. Heartbeats, healthcheck, ping

**One concept.** A heartbeat file holds one timestamp, written by a process while it is healthy.
Fresh means younger than 180 s. The timestamp is what `datetime.isoformat()` produces: an offset
is honoured, a bare timestamp is read as container-local time (`TZ`, else UTC), because `date -d`
did and existing apps write both forms.

**Files.**

- The app's, `/app/persisted_data/logs/heartbeat.txt`, written by `aeth_ext` every 60 s, as today.
- The tunnel's, `/app/persisted_data/logs/wireguard-heartbeat.txt`: the supervisor writes the
  current time on each poll while the handshake is fresh, and nothing otherwise. Current time,
  not handshake time, so the file means what the app's means: "a process that knows it is fine
  said so at T". A stale tunnel and a wedged supervisor both stop the beats. World-readable,
  written atomically. It lives beside the app's because that directory is a host bind mount:
  another container mounting the same path read-only can check it. Removed at start when the
  mode is off, so a leftover cannot read as a stale tunnel.

**`devkit-container healthcheck`** replaces the compose `bash -ec` one-liner. `--file PATH`
(repeatable; default the app's file) and `--max-age SECS` (default 180). Exit 0 when every file is
fresh; else exit 1 with one line on stderr per problem (missing, empty, unparseable, stale by N s),
which is what `docker inspect` shows. It reads files only: no root, no capabilities, no `wg`, no
pyproject parse, no dependence on the supervisor being alive. A project without `supervise` uses
it exactly as one with.

**The ping**, sent by the supervisor, follows `aeth_ext.monitoring.ping` exactly: URL from
`ALERTS_HEALTHCHECK_PING_URL`, else `https://hc-ping.com/<PINGKEY>/<HEARTBEAT_SLUG>` with
`?create=1`; `/start` once, when every file is first fresh after boot; a plain ping on every poll
while every file the supervisor is responsible for is fresh (the app's, plus the tunnel's when
on); `/fail` on the transition to stale and on a nonzero child exit, with the reason or the code
in the request body, plain pings resuming when every file is fresh again; 10 s timeout;
best-effort, one log line per failure, never fatal. The check's period is set at or above the poll
interval and its grace at or above 180 s, on the healthchecks.io side. No URL and no key, or a key
without a slug: no pinging, one log line at start (a warning in wireguard mode, where the tunnel is
then visible to Docker but not to healthchecks.io). The client is a Rust HTTPS client with rustls
(plan's call; the static musl smoke build must keep working). It runs as root in wireguard mode:
outbound only, to one host, and the response body is never parsed.

**Ownership.** When the supervisor has a URL, or a key and a slug, at spawn, it owns the ping and
sets `DEVKIT_SUPERVISED_PING=1` on the child. `aeth_ext` skips its periodic ping under that
variable and keeps the file write; `/fail` for a known job failure stays the app's to send, on the
same check. Without the variable the app pings as today. Exactly one process pings, by
construction, and the environment is the only channel: it flows down once at spawn, which is the
one direction it flows.

**Slug.** `HEARTBEAT_SLUG`, set by compose to the service name (section 8), read by both. It is
per-container data and lives in the service block for the same reason `container_name` does: two
services of one project share an image and a `pyproject.toml`, and "the first service" would be
right by luck and wrong exactly when the list exists. Fallback when the variable is absent: the
single entry of `[tool.docker].services` when there is exactly one, else no pinging and the log
line. `aeth_ext` reads the same variable in place of its `HEARTBEAT_SLUG` code constant.

**Docker health** in wireguard mode is the two-file line in section 8: app fresh and tunnel fresh,
same threshold, same meaning, the stderr line saying which. It costs nothing operationally:
Docker acts on nothing with `restart: no`, Coolify shows the badge and gates a deploy on it, and a
deploy with a dead tunnel failing is correct.

## 8. Rendering

**Compose** (this repo's template, section 3). The service block, abbreviated to what changes;
`{service}`, `{package}`, `{git_repo}` and `{git_tag}` substitute as today, every key that the rule
engine enforces carries its `rule` line, and the three gated regions read:

```yaml
    # !if dep("aeth-ext") or keys.tool.docker.supervise or keys.tool.docker.wireguard as heartbeat:
    # !rule env-keys
    environment:
      - HEARTBEAT_SLUG={service}
    # !end heartbeat
    # !if dep("aeth-ext"):
      - ALERTS_EMAIL=info@sweetfiretobacco.com
      - ALERTS_EMAIL_PWD=${ALERTS_EMAIL_PWD:?}
      - ALERTS_RECIPIENTS=["jacob.ogden@sweetfiretobacco.com"]
    # !end
    # !if keys.tool.docker.wireguard:
      - WG_PRIVATE_KEY=${WG_PRIVATE_KEY:?}
      - WG_ADDRESS=${WG_ADDRESS:?}
      - WG_PEER_PUBLIC_KEY=${WG_PEER_PUBLIC_KEY:?}
      - WG_PEER_ENDPOINT=${WG_PEER_ENDPOINT:?}
      - WG_PEER_ALLOWED_IPS=${WG_PEER_ALLOWED_IPS:?}
      - WG_PEER_PRESHARED_KEY=${WG_PEER_PRESHARED_KEY:-}
      - WG_PERSISTENT_KEEPALIVE=${WG_PERSISTENT_KEEPALIVE:-}
      - WG_HANDSHAKE_TIMEOUT_SECS=${WG_HANDSHAKE_TIMEOUT_SECS:-}
      - WG_POLL_SECS=${WG_POLL_SECS:-}
      - WG_STALE_SECS=${WG_STALE_SECS:-}
    # !end
    # !if keys.tool.docker.wireguard:
    # !rule presence
    cap_add:
      - NET_ADMIN
    # !end
```

and under `healthcheck:`, one of two `test` lists with its `start_period`:

```yaml
      # !if keys.tool.docker.wireguard:
      # !rule exact-list
      test:
        - CMD
        - /app/.venv/bin/devkit-container
        - healthcheck
        - --file
        - /app/persisted_data/logs/heartbeat.txt
        - --file
        - /app/persisted_data/logs/wireguard-heartbeat.txt
      # !rule exact
      start_period: 90s
      # !end
      # !if not keys.tool.docker.wireguard:
      # !rule exact-list
      test:
        - CMD
        - /app/.venv/bin/devkit-container
        - healthcheck
      # !rule exact
      start_period: 15s
      # !end
```

The `environment:` key renders whenever any consumer of `HEARTBEAT_SLUG` exists and never empty;
a supervised project without aeth-ext is not silently left without it. Optional `WG_*` lines are
rendered as `${NAME:-}` rather than omitted, so the knobs are visible; empty is read as unset.
`PINGKEY` and `ALERTS_HEALTHCHECK_PING_URL` are not rendered; they come from the deploy
environment as today. No `/dev/net/tun`, no `src_valid_mark` sysctl: the first serves only
userspace WireGuard, the second only `wg-quick`'s full-tunnel fwmark, and neither applies to a
fixed peer subnet.

**Rule engine.** Unchanged in behaviour: `env-keys` appends `HEARTBEAT_SLUG` and the `WG_*` lines
to an existing file, `exact-list` replaces `healthcheck.test`, `exact` sets `start_period`,
`presence` inserts `cap_add`. Rules whose key the rendered scaffold lacks are skipped, so a project
with the mode off is untouched. Keys are never removed: a project turning the mode off keeps its
`WG_*` lines until edited by hand.

**Dockerfile** (this repo's template): in the final stage,

```dockerfile
# !if keys.tool.docker.wireguard:
RUN apt-get update && apt-get install -y --no-install-recommends wireguard-tools iproute2 \
  && rm -rf /var/lib/apt/lists/*
# !end
```

so the binary and the tools it shells out to version together. `static_files::render` runs the
gate pass, which it does not today (it only substitutes).

## 9. `aeth_ext`

On a branch, PR opened, not merged or released; the owner does both.

- `_auto_slug` prefers `os.environ["HEARTBEAT_SLUG"]`; the constant lookup stays as the fallback
  for hosts that are not containers.
- `run_heartbeat_async` and `HeartbeatThread` skip the ping, keeping the file write, when
  `DEVKIT_SUPERVISED_PING` is set. `send_heartbeat(failure=True)` is unaffected.

Both are one-condition changes; the contract (the two names and their meaning) is owned here.

## 10. Release order

1. `aeth-devkit`: the language (section 2), the compose scaffold from the container package with
   the templates fallback, the generic refusal. Together with it, `devkit-templates`: every marker
   rewritten, the `aeth-devkit` floor raised to this release. Neither is usable without the other;
   the constraint machinery pairs them in every project.
2. `devkit-container`: the compose template, the switches, the supervisor, the tunnel, the
   healthcheck, the ping, the Dockerfile block. Its smoke tests are the gate. The pyproject
   template floors `devkit-container>={latest}` and `setup-project` advances the package every
   run, so a project rendering the new healthcheck line gets a binary that has the subcommand in
   the same run.
3. `devkit-templates`: the compose template removed. `aeth-devkit`: the fallback removed. Both
   small; either order.
4. `aeth_ext`: the owner's, after the PR.

## 11. Host requirements

A Docker host kernel with the wireguard module (Linux 5.6 or later) and a deploy platform that
passes `cap_add` through; both verified on the first deploy per the companion spec's checklist.
The CI runner loads the module (`sudo modprobe wireguard`) before the wireguard smoke test.

## 12. Tests

**`setup`, the language.** A table of expressions against a fixture `pyproject.toml`: each
operator, `keys` paths present, missing and hyphenated, each value type, `dep` by dependency,
by group and by self, each flag, and the error cases (syntax, unknown name, non-literal `dep`
argument). Block structure: explicit, one-liner, structural per file type (each unit rule, fences
in markdown, continuations in Dockerfiles), nesting three deep with a named end unwinding two,
trailing ends, markers at every indent. Errors: unclosed block, unknown end name, an end naming a
structural block, an explicit block crossing a structural boundary, an opener without a colon on
its own line. Evaluation: each distinct expression evaluated once per run; the recorded `keys`
paths; the refusal on a path a gate read and not on one it did not. The evaluator seam: its tests
are written against the seam's signature, not Monty's, so they are the acceptance suite for a swap.
Compose: `rule` kinds read from the scaffold, a `rule` line not followed by a key refused, the
scaffold read from the container package when present and from the templates package otherwise.
Every existing test of the retired code is rewritten against the new markers.

**This repo, unit.** Timestamp parsing (offset, bare, garbage), freshness with an injected clock,
environment scrubbing, the stale-and-re-up state machine with an injected clock, ping URL and
suffix building, slug fallback.

**This repo, render.** CI renders a scratch Docker project through the released devkit,
`--dry-run`, with the mode off and on, and fails on a render error or a marker left in the
output. The scratch project's lock points `devkit-container` at this checkout's wheel, the way
the smoke test's does, so `setup-project` reads this checkout's templates and cannot advance the
package past them. This is the guard the templates repo has, and
the only place the two templates meet the parser before a release.

**Smoke, off mode.** Today's test unchanged and green, plus the `healthcheck` subcommand run in
the image against a fresh and a stale file. The smoke tests apply the Dockerfile template's one gate
with a local strip, since the parser lives in `setup`.

**Smoke, supervise without wireguard.** The app reports its parent is PID 1, uid 999, empty
capability sets; `SIGTERM` reaches it and its exit code passes through; `DEVKIT_SUPERVISED_PING`
is present when a slug and key are given and absent otherwise.

**Smoke, wireguard.** A hub container with generated keys and an image built from the template
with the mode on, on one Docker network. Asserts: handshake within the timeout; child uid 999
with empty capability sets and `WG_PRIVATE_KEY` absent from its environment; the tunnel heartbeat
present, readable by 999, fresh; `healthcheck --file` both files exits 0; removing the peer on the
hub and restoring it makes the tunnel file go stale (the healthcheck exits 1 naming it) then fresh
again, with a re-up logged; `SIGTERM` and exit code pass through. Poll and stale intervals are set
short through the environment. The ping is tested against a local HTTP listener standing in for
healthchecks.io: `/start` once, plain while healthy, `/fail` on the stale transition.

## 13. Done means

- `aeth-devkit` renders every template in `devkit-templates` and both in this repo with no
  `setup-project:` marker left anywhere, and its README documents the language.
- The existing smoke test is unchanged and green with both switches off.
- The three new smoke tests and the render check are green in this repo's CI.
- `setup-project` on a project with the mode off renders no wireguard line anywhere; turning it
  on changes only the gated blocks, `cap_add`, `healthcheck.test` and `start_period`.
- `ScheduledReportAggregator` builds and starts in both modes; with the mode on, the companion
  spec's first-deploy checks pass.
- The `aeth_ext` PR is open and its tests pass on the branch.

## 14. Decisions taken, and what was rejected

- **Python via Monty, over a custom expression parser and over a Python subprocess.** A custom
  parser was sized at a few hundred lines once comparisons, `in` and precedence were in; "like
  Python's" semantics would have needed documenting and could drift. A subprocess of the venv's
  Python was simpler than the parser but made rendering and `setup`'s tests need a Python on
  PATH. Monty evaluates in-process with real Python semantics for expressions, no C toolchain,
  and a small dependency tree. `rustpython-vm` (a full interpreter, sixty-plus crates, tens of
  megabytes) is the fallback behind the seam if the subset ever bites.
- **`keys` paths rewritten by `setup`, not a live object.** TOML keys contain hyphens and Python
  identifiers cannot, so `keys.tool.ruff.lint.per-file-ignores` is only possible as a rewrite.
  The rewrite also yields the read set statically, which the refusal needs.
- **Explicit ends, not indentation.** Editors format templates on save, so whitespace cannot
  carry meaning; and in YAML a sibling key shares the gated key's indent, so no indentation rule
  can say "this key and its subtree, then stop". `S!` keeps the implicit end where the file's own
  structure supplies one.
- **Unknown names are errors, not false.** The silent-false behaviour let an old devkit drop a
  block without a trace; the constraint machinery makes the hard error safe.
- **Lockstep rename, no dual syntax.** The old markers are not accepted after the language ships;
  the machinery that pairs a templates release with the devkit it needs exists for exactly this.
- **Rule kinds in the template, not a table in `setup`.** The table was the last place `setup`
  hard-coded knowledge of one template's keys.
- **Compose template owned by the container.** See section 3.
- **Supervisor opt-in, not universal.** A universal supervisor was considered for `/fail` on
  crash and uniform monitoring; rejected because most apps never spawn a child and would pay for a
  reaper and a signal path they cannot use. `supervise` gives the benefit to projects that want it.
- **No status file.** `/run/devkit/wireguard.json` was in the original design for `aeth_ext` to
  read and alert on; the ping covers that. It was then kept for the healthcheck and for a ping
  handoff; the tunnel heartbeat file and the spawn-time environment variable cover those, with no
  dependence on the supervisor's poll loop and no second file format.
- **Keys over stdin; `ip` + `wg set`; no `wg-quick`.** No key touches a filesystem, no bash in the
  root phase, and the two compose extras `wg-quick` would have needed are gone with it.
- **Stale counts as unhealthy, immediately.** A local retry budget before the badge flips was
  rejected: healthchecks.io's grace period already filters transients server-side, and
  `WG_STALE_SECS` is a deploy-time knob if the badge is ever noisy.
- **Ping ownership decided by configuration presence, not a switch.** A `[tool.docker]` key for
  who pings was rejected once the slug moved to compose: key and slug present means the supervisor
  pings, and the child is told at spawn. A consent file written by the app was rejected for the
  same reason.
- **Slug from compose, not the first service.** See section 7.
