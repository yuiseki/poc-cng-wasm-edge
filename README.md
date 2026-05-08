# poc-cng-wasm-edge

This repository explores WebAssembly-based edge functions for Cloud-Native Geospatial workflows.

This is not an attempt to replace Martin, DuckDB, GDAL, or container-based CNG functions.

The initial hypothesis is narrower:

> Wasm edge functions may be useful for lightweight CNG metadata, policy, routing, validation, and archive introspection.
> Heavy geospatial processing — raster tiling, reprojection, dynamic MVT generation, large GeoParquet queries — may still be better handled by containerized engines.
> The goal is to identify the boundary between Wasm edge functions and conventional CNG function containers.

## The question

```
Can WebAssembly edge functions cover the lightweight control plane of CNG,
while containers and specialized servers handle the heavy data plane?
```

## Runtime

| Component | Choice |
|---|---|
| Runtime | [Spin](https://spinframework.dev/) |
| Language | Rust |
| Target | `wasm32-wasip1` |
| Trigger | HTTP |

## Roadmap

| Phase | Goal | Status |
|---|---|---|
| 0 | Spin HTTP function up, `/healthz` + `/metadata` responding | **DONE** |
| 1 | Bundled file access — serve `data/metadata.json` + `tilejson.json` from component | DONE (via `include_str!`) |
| 2 | Outbound HTTP — fetch remote metadata via `allowed_outbound_hosts` | planned |
| 3 | PMTiles header reader — introspect a small `.pmtiles` archive | planned |
| 4 | COG info / policy endpoint — routing logic, not pixel reads | planned |

## Functions

### metadata-function (`spin/metadata-function/`)

Phase 0+1 function. Serves static metadata from JSON bundled at compile time via `include_str!`.

```
GET /            service info
GET /healthz     health check
GET /metadata    bundled data/metadata.json
GET /tilejson    bundled data/tilejson.json
GET /env         selected runtime environment info
```

## Quick start

```bash
# Install Spin (one-time)
curl -fsSL https://github.com/fermyon/spin/releases/download/v4.0.0/spin-v4.0.0-linux-amd64.tar.gz \
  | tar -xz -C ~/bin spin

# Install wasm target (one-time)
rustup target add wasm32-wasip1

# Build and run
just run
# or manually:
cd spin/metadata-function
spin build && spin up
```

## Comparison

| Runtime | Use case | Expected role |
|---|---|---|
| Martin / Caddy | COG/PMTiles tile serving | best for heavy static delivery |
| FastAPI container | DuckDB/GDAL-heavy processing | best for heavy geospatial |
| Knative on k3s | portable container FaaS | good but operationally heavy |
| **Spin local** | **lightweight edge function** | **promising for metadata/policy** |
| WasmEdge direct | ultra-light custom runtime | compare after Spin |
| SpinKube | Kubernetes-native Wasm | compare after Spin local |
| wasmCloud | distributed capability model | later, CNG swarm candidate |
