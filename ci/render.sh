#!/usr/bin/env bash
# Render this checkout's two templates through the released devkit's parser, with the mode
# off and on, and fail on a render error. A scratch Docker project gets this checkout's wheel
# by path and a dry run of setup-project: a dry run advances no package, so the templates it
# renders are the ones in this wheel (a plain run would rewrite the source to the index and
# render the released package's). A dry run writes no files, so this proves "renders without
# error and lists both docker files"; the smoke tests check the rendered content (spec 12).
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
uv run maturin build --release --out "$here/dist"
wheel="$(ls "$here"/dist/devkit_container-*.whl | head -1)"
# On a Windows dev machine (Git Bash) uv needs the wheel's Windows path, not the POSIX one.
command -v cygpath > /dev/null && wheel="$(cygpath -m "$wheel")"
for mode in off on; do
  root="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/render-$mode"
  rm -rf "$root" && mkdir -p "$root/src/scratch_app" && : > "$root/src/scratch_app/__init__.py"
  {
    printf '[project]\nname = "scratch-app"\nversion = "0.1.0"\nrequires-python = ">=3.14"\ndependencies = ["devkit-container"]\n\n'
    printf '[project.scripts]\nrun-app-scratch = "scratch_app:main"\n\n'
    printf '[dependency-groups]\ndev = ["aeth-devkit", "devkit-templates"]\n\n'
    printf '[tool.docker]\nservices = ["scratch-app"]\nrequired_persisted_dirs = ["persisted_data"]\n'
    [ "$mode" = on ] && printf 'wireguard = true\n'
    printf '\n[tool.uv.sources]\naeth-devkit = [{ index = "SFTPyPI" }]\ndevkit-templates = [{ index = "SFTPyPI" }]\ndevkit-container = { path = "%s" }\n\n' "$wheel"
    printf '[[tool.uv.index]]\nname = "SFTPyPI"\nurl = "https://pypi.sweetfiretobacco.com/jacob.ogden/internal/+simple"\nexplicit = true\n'
  } > "$root/pyproject.toml"
  out="$root/out.txt"
  ( cd "$root" && uv sync 2>&1 | tail -3 && uv run devkit --version && uv run devkit setup-project --dry-run --no-vscode ) | tee "$out"
  grep -q '^docker/Dockerfile: ' "$out" && grep -q '^docker/compose.yaml: ' "$out"
  echo "render ok: mode $mode"
done
