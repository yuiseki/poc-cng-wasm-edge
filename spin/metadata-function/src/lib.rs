use spin_sdk::http::{send, IntoResponse, Method, Request, Response};
use spin_sdk::http_component;

const METADATA_JSON: &str = include_str!("../data/metadata.json");
const TILEJSON: &str = include_str!("../data/tilejson.json");
const CAPABILITIES_JSON: &str = include_str!("../data/capabilities.json");

// ── Phase 4: FlatGeobuf range-request introspection ─────────────────────
// Smoke test: countries.fgb from GitHub (raw.githubusercontent.com, already allowed).
// Full test: ESA WorldCover grid composites from S3.
const FGB_COUNTRIES_URL: &str =
    "https://raw.githubusercontent.com/flatgeobuf/flatgeobuf/master/test/data/countries.fgb";
const FGB_ESA_URL: &str =
    "https://esa-worldcover.s3.eu-central-1.amazonaws.com/esa_worldcover_grid_composites.fgb";

// FlatGeobuf v3 magic: bytes 0-2 = "fgb", byte 3 = 0x03 (version), bytes 4-7 = "fgb\x00"
const FGB_MAGIC: &[u8] = b"fgb\x03fgb";

// ── Remote CNG metadata sources (fixed — not a general-purpose proxy).
// Outbound access to these hosts must be declared in spin.toml via
// `allowed_outbound_hosts`. Requests to any other host will be denied
// by the Spin runtime regardless of what this code attempts.
const REMOTE_METADATA_URL: &str =
    "https://raw.githubusercontent.com/yuiseki/poc-cng-wasm-edge/main/spin/metadata-function/data/metadata.json";
const REMOTE_TILEJSON_URL: &str =
    "https://raw.githubusercontent.com/yuiseki/poc-cng-wasm-edge/main/spin/metadata-function/data/tilejson.json";

#[http_component]
async fn handle(req: Request) -> anyhow::Result<impl IntoResponse> {
    // uri() returns the full URL string in spin-sdk v5 (e.g. "http://host/path?q").
    let uri = req.uri();
    let path_with_query = if let Some(after_host) = uri
        .split("://")
        .nth(1)
        .and_then(|s| s.find('/').map(|i| &s[i..]))
    {
        after_host
    } else {
        uri
    };
    let path = path_with_query.split('?').next().unwrap_or("/").trim_end_matches('/');

    // OPTIONS preflight — required for cross-origin requests from GitHub Pages.
    if *req.method() == Method::Options {
        return Ok(Response::builder()
            .status(204)
            .header("access-control-allow-origin", "*")
            .header("access-control-allow-methods", "GET, OPTIONS")
            .header("access-control-allow-headers", "Content-Type")
            .header("timing-allow-origin", "*")
            .body("")
            .build());
    }

    if *req.method() != Method::Get {
        return Ok(Response::builder()
            .status(405)
            .header("allow", "GET, OPTIONS")
            .body("")
            .build());
    }

    match path {
        // ── Phase 0: basic health / service info ────────────────────────
        "" | "/" => Ok(json_response(
            200,
            r#"{"service":"metadata-function","runtime":"Spin/Wasm","version":"0.1.0","phase":"0+1+2"}"#,
        )),

        "/healthz" => Ok(json_response(200, r#"{"status":"ok"}"#)),

        // ── Phase 1: bundled file access (include_str! — zero file I/O) ──
        "/metadata" => Ok(json_response(200, METADATA_JSON)),

        "/tilejson" | "/tilejson.json" => Ok(json_response(200, TILEJSON)),

        "/env" => {
            let spin_full_url = req
                .header("spin-full-url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let body = format!(
                r#"{{"spin_full_url":{:?},"wasm_target":"wasm32-wasip1"}}"#,
                spin_full_url
            );
            Ok(json_response(200, &body))
        }

        // ── Phase 2: capability-scoped outbound HTTP ─────────────────────
        //
        // Spin denies outbound HTTP by default. Access requires explicit
        // `allowed_outbound_hosts` in spin.toml. The allowed hosts are
        // declared in the manifest — not in this code — making the
        // external dependencies of this function inspectable.

        "/capabilities" => Ok(json_response(200, CAPABILITIES_JSON)),

        "/remote-metadata" => fetch_remote(REMOTE_METADATA_URL).await,

        "/remote-tilejson" => fetch_remote(REMOTE_TILEJSON_URL).await,

        // ── Phase 4: FlatGeobuf range-request introspection ───────────────
        // Prove that Spin can issue HTTP Range requests to read only the
        // header of a FlatGeobuf file — the first step toward dynamic
        // vector tile generation without reading the whole file.

        "/fgb-head" => fgb_head(FGB_COUNTRIES_URL).await,

        "/fgb-head-esa" => fgb_head(FGB_ESA_URL).await,

        _ => Ok(Response::builder()
            .status(404)
            .header("content-type", "application/json")
            .header("access-control-allow-origin", "*")
            .header("timing-allow-origin", "*")
            .body(r#"{"error":"not found"}"#)
            .build()),
    }
}

/// Fetch a remote URL using Spin's capability-scoped outbound HTTP.
/// The host must be listed in spin.toml `allowed_outbound_hosts` or
/// the Spin runtime will deny the connection before it reaches the network.
async fn fetch_remote(url: &str) -> anyhow::Result<Response> {
    match send::<_, Response>(Request::get(url)).await {
        Ok(resp) => {
            let status = *resp.status();
            let body = resp.into_body();
            Ok(Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .header("access-control-allow-origin", "*")
                .header("access-control-allow-methods", "GET, OPTIONS")
                .header("access-control-allow-headers", "Content-Type")
                .header("timing-allow-origin", "*")
                .header("x-fetched-from", url)
                .body(body)
                .build())
        }
        Err(e) => {
            // Outbound denied (not in allowed_outbound_hosts) or network error.
            let msg = format!(
                r#"{{"error":"outbound_denied","detail":{:?},"url":{:?}}}"#,
                e.to_string(),
                url
            );
            Ok(Response::builder()
                .status(403)
                .header("content-type", "application/json")
                .body(msg)
                .build())
        }
    }
}

/// Phase 4: FlatGeobuf header introspection via HTTP Range request.
///
/// FlatGeobuf layout:
///   [0..7]   magic: "fgb\x03fgb\x00" (or \x01)
///   [8..11]  header_size: u32 le — size of the FlatBuffers-encoded header
///   [12..]   FlatBuffers header (contains geometry type, CRS, columns, feature count, …)
///
/// We fetch bytes 0..(12 + header_size - 1) in a single Range request,
/// validate the magic, decode header_size, and return a summary.
/// The index (R-tree) and feature data are NOT fetched — this is the
/// minimal footprint needed to prove range-request capability from Wasm.
async fn fgb_head(url: &str) -> anyhow::Result<Response> {
    // Step 1: fetch first 12 bytes to get magic + header_size.
    let step1 = range_get(url, 0, 11).await?;
    if step1.len() < 12 {
        let msg = format!(
            r#"{{"error":"short_response","got":{},"url":{:?}}}"#,
            step1.len(), url
        );
        return Ok(json_response(502, &msg));
    }

    // Validate magic bytes.
    if &step1[0..7] != FGB_MAGIC {
        let hex: String = step1[0..8].iter().map(|b| format!("{:02x}", b)).collect();
        let msg = format!(
            r#"{{"error":"bad_magic","hex":{:?},"url":{:?}}}"#,
            hex, url
        );
        return Ok(json_response(400, &msg));
    }
    let padding = step1[7]; // 0x00 or 0x01

    // header_size is u32 little-endian at bytes 8..11.
    let header_size = u32::from_le_bytes([step1[8], step1[9], step1[10], step1[11]]);

    // Step 2: fetch the FlatBuffers header itself.
    let header_end = 12 + header_size as usize - 1;
    let header_bytes = range_get(url, 12, header_end).await?;

    // Basic FlatBuffers root offset check (first 4 bytes = offset to root table).
    let fb_root_offset = if header_bytes.len() >= 4 {
        u32::from_le_bytes([header_bytes[0], header_bytes[1], header_bytes[2], header_bytes[3]])
    } else {
        0
    };

    let body = format!(
        r#"{{"magic":"fgb","version":3,"padding":{padding},"header_size":{header_size},"fb_root_offset":{fb_root_offset},"header_bytes_fetched":{fetched},"url":{url:?},"note":"Range requests work. Next: parse FlatBuffers header for geometry_type, crs, feature_count."}}"#,
        padding = padding,
        header_size = header_size,
        fb_root_offset = fb_root_offset,
        fetched = header_bytes.len(),
        url = url,
    );
    Ok(json_response(200, &body))
}

/// Issue a single HTTP Range request: `Range: bytes={start}-{end}`.
/// Returns the response body as Vec<u8>.
///
/// spin-sdk v5: `Request::get(url)` returns an owned `RequestBuilder`.
/// `.header()` mutates it in-place (returns `&mut Self` for chaining convenience,
/// but the owned `req` remains valid). Pass the owned builder directly to `send()`.
async fn range_get(url: &str, start: usize, end: usize) -> anyhow::Result<Vec<u8>> {
    let range_value = format!("bytes={}-{}", start, end);
    let mut req = Request::get(url);
    req.header("range", &range_value);
    let resp: Response = send(req).await?;
    Ok(resp.into_body().to_vec())
}

fn json_response(status: u16, body: &str) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("access-control-allow-origin", "*")
        .header("access-control-allow-methods", "GET, OPTIONS")
        .header("access-control-allow-headers", "Content-Type")
        .header("timing-allow-origin", "*")
        .body(body.to_owned())
        .build()
}
