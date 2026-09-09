# todo

## 1. `devkit-container healthcheck` — replace the bash one-liner in every compose file

Today every managed service's compose block carries the same shell healthcheck (rendered by
devkit's `templates/docker/compose.template.yaml`):

```yaml
healthcheck:
  test:
    - CMD-SHELL
    - bash -ec '[ -f /app/persisted_data/logs/heartbeat.txt ] && ts=$$(cat /app/persisted_data/logs/heartbeat.txt 2>/dev/null) && [ -n "$$ts" ] && hb=$$(date -d "$$ts" +%s 2>/dev/null) && now=$$(date +%s) && [ $$((now - hb)) -lt 180 ]'
```

That is a bash spawn plus two `date` spawns and a `cat` on every probe. Replace it with a
subcommand on the binary already installed in the image:

```yaml
test: ["CMD", "/app/.venv/bin/devkit-container", "healthcheck"]
```

**Why:** one exec of a static-ish Rust binary instead of a shell pipeline — cheaper per interval,
and noticeably snappier when Coolify triggers a healthcheck by hand from the UI. It also moves the
staleness logic into code that is tested and versioned with the image, instead of a quoted string
duplicated across every project's compose file (where `$$` escaping and `date -d` availability are
silent footguns).

**Behavior:** read the heartbeat file, require non-empty contents, parse the timestamp, exit 0 if
`now - heartbeat < max_age`, else exit 1 with a one-line reason on stderr (missing / empty /
unparseable / stale by N seconds) so `docker inspect` shows something useful.

**Open questions:**

- Config source: flags (`--heartbeat`, `--max-age`) baked into the compose line, or keys under
  `[tool.docker]` in `/app/pyproject.toml` like `required_persisted_dirs`? The latter keeps the
  compose line uniform across every service but adds a TOML parse to each probe.
- Timestamp format: the app writes `datetime.now(UTC).isoformat()`. Decide whether to accept only
  RFC 3339 or be lenient like `date -d` was.
- Cross-repo coordination: the compose template lives in devkit, the binary here. The new compose
  line only works once every image ships a `devkit-container` new enough to have the subcommand —
  needs a release here first, then a devkit template bump, and a story for projects rendered
  before that (`devkit docker-pin`?).
- Whether the healthcheck should also cover liveness signals beyond the heartbeat file.
