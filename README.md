# devkit-container

The image-side helper for devkit-managed Docker projects. One small static-free binary,
`devkit-container`, installed into the project's venv like any dependency, plus the Dockerfile
template `devkit setup-project` renders for the project. Both ship in one wheel, so the
Dockerfile a project builds with always matches the binary its image installs.

## How a project uses it

`setup-project` adds `devkit-container` to `[project].dependencies` of every project with
`[tool.docker].services`, locks it to the newest release the installed devkit accepts, and
renders `docker/Dockerfile` from `devkit_container/template.Dockerfile` in the venv. The image
installs the package with `uv sync --frozen` and uses `/app/.venv/bin/devkit-container` both for
the build-time queries and as the entrypoint. `devkit docker-pin` refreshes a Dockerfile that
drifted from the locked version before it pins. No Python runs in the image outside the app
itself.

## Subcommands

- `app-extra` prints `--extra app` when `[project.optional-dependencies].app` exists.
- `readme` prints `project.readme` (string or `{ file = … }` form).
- `run` is the entrypoint (Linux only). Must be root. Resolves the single `run-app-*` script
  in `[project.scripts]`; checks every `[tool.docker].required_persisted_dirs` entry is backed
  by a bind mount (the path or an ancestor below `/app`, per `/proc/self/mountinfo`) and refuses
  to start otherwise; `mkdir -p` + recursive chown to `999:999`; `setgroups([])`, `setgid`,
  `setuid`; `exec /app/.venv/bin/<script>`. `/app` itself stays root-owned: the app writes only
  to its mounted dirs or temp dirs. Entries that are empty, `.`, `..`, absolute or escape `/app`
  are errors; a table still carrying `chown_paths`/`mkdirs` is refused with the migration hint.
  Flags `--pyproject`, `--app-root`, `--mountinfo` exist for tests.

## `[tool.docker]` schema

| Key | Meaning |
|---|---|
| `services` | compose services `setup-project` manages; the only Docker switch |
| `required_persisted_dirs` | paths relative to `/app` the entrypoint guarantees exist, are bind-mounted and are owned by nonroot |
| `silence_unlisted_services_warning` | quiets `setup-project`'s warning when Docker files exist but `services` is empty |

`chown_paths` and `mkdirs` are legacy keys the entrypoint refuses.

## Tests

`cargo test` covers the parsers and, on Linux as root, the entrypoint. The smoke test
(`cargo test --test docker_smoke -- --ignored --nocapture`; CI runs it) builds the wheel for the
image platform, builds the template Dockerfile around a scratch app with that wheel installed
into the venv, starts it on a named volume and checks the app's own report: PID 1, uid/gid 999,
`/app` read-only, the persisted dirs created, owned and writable, the venv, the `app` extra and
the wheel install; a run without the volume or as non-root is refused first.

## Releasing

`uv run devkit release <bump>`. The standard release workflow builds Windows and manylinux
wheels and publishes them to SFTPyPI. The Windows wheel is required, not optional: projects
install this package on Windows dev machines too, where only the query subcommands run.
