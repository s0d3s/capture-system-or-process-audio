set -euo pipefail
uv python install "$HOST_PYTHON"
HOST_PY="$(uv python find --no-project --managed-python "$HOST_PYTHON")"

PYO3_PYTHON="$HOST_PY" cargo build --locked --manifest-path crates/capture-python/Cargo.toml
PYO3_PYTHON="$HOST_PY" cargo test --locked --manifest-path crates/capture-python/Cargo.toml --lib

mkdir -p dist/sdist
uv tool run --from "maturin==$MATURIN_VERSION" maturin sdist -m crates/capture-python/Cargo.toml -o dist/sdist

SDIST_FILE="$(ls dist/sdist/capture_system_or_process_audio-*.tar.gz)"
TMP_WHL_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_WHL_DIR"' EXIT
uv build --wheel "$SDIST_FILE" --python "$HOST_PY" -o "$TMP_WHL_DIR"
SDIST_WHL="$(ls "$TMP_WHL_DIR"/*.whl)"
uv run --no-project --python "$HOST_PY" --with "$SDIST_WHL" python tools/release/smoke_sdist.py