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
| 2 | Outbound HTTP — fetch remote metadata via `allowed_outbound_hosts` | **DONE** |
| 3A | GitHub Pages visual dashboard — MapLibre map + PerformanceResourceTiming panel | **DONE** |
| 3B | TileJSON raster overlay — connect Wasm control-plane to COG tile data-plane | **DONE** |
| 4A | FlatGeobuf header introspection — HTTP Range requests to remote FGB files | **DONE** |
| 4B | FlatBuffers binary parse — pure Rust, zero external crates, feature count + schema | **DONE** |
| 4C | Spatial query + GeoJSON decode — R-tree leaf scan, bbox filter, vector overlay on COG | **DONE** |

## Functions

### metadata-function (`spin/metadata-function/`)

Phase 0+1+2+4C function. Serves static metadata from JSON bundled at compile time via `include_str!`,
fetches remote metadata via Spin's capability-scoped outbound HTTP, and performs FlatGeobuf spatial
queries against remote FGB files using HTTP Range requests — all in pure Rust with zero external crates
beyond `spin-sdk` and `anyhow`.
Exposed via Cloudflare Tunnel at `https://oceania.yuiseki.net`.

```
GET /                service info
GET /healthz         health check
GET /metadata        bundled data/metadata.json  (includes bounds + endpoint list)
GET /tilejson        bundled TileJSON linking to cog-tile data-plane
GET /env             selected runtime environment info
GET /capabilities    outbound HTTP capability declaration (Phase 2)
GET /remote-metadata fetch metadata.json from GitHub raw (Phase 2)
GET /remote-tilejson fetch tilejson.json from GitHub raw (Phase 2)
GET /fgb-parse       FlatGeobuf header parse: countries.fgb (179 features, MultiPolygon, EPSG:4326)
GET /fgb-parse-esa   FlatGeobuf header parse: ESA WorldCover grid (19363 tiles, Polygon, EPSG:4326)
GET /fgb-geojson     spatial query on countries.fgb for COG bbox: returns Ivory Coast GeoJSON
OPTIONS /*           CORS preflight — Timing-Allow-Origin: * for PerformanceResourceTiming
```

#### Phase 4C: FlatGeobuf spatial query + GeoJSON decode + vector overlay

The `/fgb-geojson` endpoint demonstrates the key result of Phase 4:

1. Two HTTP Range requests fetch the FlatGeobuf header from a remote FGB file
2. A pure-Rust FlatBuffers parser (no external crates) extracts feature count, geometry type, column schema, and R-tree index size
3. A single Range request fetches all R-tree leaf nodes (features_count × 40 bytes)
4. Each leaf is tested against a query bbox — matching features are fetched by offset
5. MultiPolygon + property data is decoded to a GeoJSON `FeatureCollection`
6. The GitHub Pages dashboard overlays the result on COG satellite imagery via MapLibre

```
GitHub Pages → GET /fgb-geojson?bbox=... (Spin Wasm)
                 ↓ GeoJSON FeatureCollection (Ivory Coast polygon)
             → MapLibre adds GeoJSON source + fill/line layers on top of COG raster
```

This demonstrates that Wasm edge functions can execute lightweight spatial queries — R-tree
traversal, bbox intersection, binary format decode — without containers or GDAL.

```
allowed_outbound_hosts = [
  "https://raw.githubusercontent.com",               # FlatGeobuf countries.fgb
  "https://esa-worldcover.s3.eu-central-1.amazonaws.com",  # ESA WorldCover FGB
]
```

#### Phase 4A/4B: FlatGeobuf and FlatBuffers binary parse

The `/fgb-parse` and `/fgb-parse-esa` endpoints parse the FlatGeobuf binary format
using only two HTTP Range requests:

- Request 1: bytes 0-11 — magic header (`8 bytes`) + `header_size` (`u32`)
- Request 2: bytes 12-(12+header_size-1) — FlatBuffers-encoded `Header` table

The FlatBuffers reader is implemented from scratch (no `flatbuffers` crate):
vtable offsets, field reads, vector reads, string reads, nested table reads.

Key lesson: `vtable_pos = table_pos - soffset` (soffset is **positive** when vtable precedes table).

ESA WorldCover FGB schema discovered via `/fgb-parse-esa`:
`tile`, `s1_vvvhratio_2020`, `s1_vvvhratio_2021`, `s2_rgbnir_2020/2021`, `s2_ndvi_2020/2021`, `s2_swir_2020/2021` (all String).

#### Phase 3: GitHub Pages visual and performance dashboard

`docs/index.html` — static dashboard published on GitHub Pages.

```
Left panel:   Spin endpoint selector + per-endpoint status / latency badges
Center map:   MapLibre GL JS — COG satellite raster + FlatGeobuf vector overlay
Right panel:  PerformanceResourceTiming table (total, TTFB, transferSize, nextHopProtocol)
```

The `tilejson` endpoint links the Wasm control-plane to the COG tile data-plane:

```
GitHub Pages → GET /tilejson (Spin Wasm)
                 ↓ tiles URL
             → MapLibre loads tiles from https://cog-tile.yuiseki.com
```

Wasm returns TileJSON — tile pixels are served by a separate COG function (FastAPI + rio-tiler on Knative).

#### Phase 2: capability-scoped outbound HTTP

Spin denies outbound HTTP by default. Access is granted per-host in `spin.toml`:

```toml
[component.metadata-function]
allowed_outbound_hosts = [
  "https://raw.githubusercontent.com",
  "https://esa-worldcover.s3.eu-central-1.amazonaws.com",
]
```

This makes the external dependencies of a Wasm function **inspectable from the manifest** —
a key property for CNG policy/audit use cases. Any request to a host not listed here
is denied by the Spin runtime before reaching the network, regardless of what the code does.

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
