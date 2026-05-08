SPIN := `which spin 2>/dev/null || echo "$HOME/bin/spin"`
METADATA_DIR := "spin/metadata-function"

# Build metadata-function
build:
    cd {{METADATA_DIR}} && {{SPIN}} build

# Run metadata-function locally on port 3000
run: build
    cd {{METADATA_DIR}} && {{SPIN}} up

# Run on a specific port
run-port port="3000": build
    cd {{METADATA_DIR}} && {{SPIN}} up --listen 127.0.0.1:{{port}}

# Quick smoke test (requires running instance on port 3000)
test port="3000":
    @echo "--- / ---"
    curl -sf http://127.0.0.1:{{port}}/
    @echo ""
    @echo "--- /healthz ---"
    curl -sf http://127.0.0.1:{{port}}/healthz
    @echo ""
    @echo "--- /metadata ---"
    curl -sf http://127.0.0.1:{{port}}/metadata
    @echo ""
    @echo "--- /tilejson ---"
    curl -sf http://127.0.0.1:{{port}}/tilejson
    @echo ""

# Wasm binary size
wasm-size:
    ls -lh {{METADATA_DIR}}/target/wasm32-wasip1/release/metadata_function.wasm
