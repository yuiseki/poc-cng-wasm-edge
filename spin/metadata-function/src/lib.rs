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

// FlatGeobuf v3 magic: bytes 0-7 = "fgb\x03fgb" + padding byte
const FGB_MAGIC: &[u8] = b"fgb\x03fgb";

// ── Remote CNG metadata sources (fixed — not a general-purpose proxy).
const REMOTE_METADATA_URL: &str =
    "https://raw.githubusercontent.com/yuiseki/poc-cng-wasm-edge/main/spin/metadata-function/data/metadata.json";
const REMOTE_TILEJSON_URL: &str =
    "https://raw.githubusercontent.com/yuiseki/poc-cng-wasm-edge/main/spin/metadata-function/data/tilejson.json";

#[http_component]
async fn handle(req: Request) -> anyhow::Result<impl IntoResponse> {
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
            r#"{"service":"metadata-function","runtime":"Spin/Wasm","version":"0.1.0","phase":"0+1+2+4"}"#,
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
        "/capabilities" => Ok(json_response(200, CAPABILITIES_JSON)),

        "/remote-metadata" => fetch_remote(REMOTE_METADATA_URL).await,

        "/remote-tilejson" => fetch_remote(REMOTE_TILEJSON_URL).await,

        // ── Phase 4A: FlatGeobuf range-request header fetch ───────────────
        "/fgb-head" => fgb_head(FGB_COUNTRIES_URL).await,

        "/fgb-head-esa" => fgb_head(FGB_ESA_URL).await,

        // ── Phase 4B: FlatGeobuf FlatBuffers header parse ─────────────────
        // Parse the FlatBuffers-encoded Header table to extract geometry_type,
        // features_count, envelope (bounding box), CRS, and index_node_size.
        // All achieved with two HTTP Range requests and zero external crates.
        "/fgb-parse" => fgb_parse(FGB_COUNTRIES_URL).await,

        "/fgb-parse-esa" => fgb_parse(FGB_ESA_URL).await,

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

/// Phase 4A: Fetch FlatGeobuf magic bytes + raw header size.
async fn fgb_head(url: &str) -> anyhow::Result<Response> {
    let step1 = range_get(url, 0, 11).await?;
    if step1.len() < 12 {
        let msg = format!(
            r#"{{"error":"short_response","got":{},"url":{:?}}}"#,
            step1.len(), url
        );
        return Ok(json_response(502, &msg));
    }
    if &step1[0..7] != FGB_MAGIC {
        let hex: String = step1[0..8].iter().map(|b| format!("{:02x}", b)).collect();
        let msg = format!(
            r#"{{"error":"bad_magic","hex":{:?},"url":{:?}}}"#,
            hex, url
        );
        return Ok(json_response(400, &msg));
    }
    let padding = step1[7];
    let header_size = u32::from_le_bytes([step1[8], step1[9], step1[10], step1[11]]);
    let header_end = 12 + header_size as usize - 1;
    let header_bytes = range_get(url, 12, header_end).await?;
    let fb_root_offset = if header_bytes.len() >= 4 {
        u32::from_le_bytes([header_bytes[0], header_bytes[1], header_bytes[2], header_bytes[3]])
    } else {
        0
    };
    let body = format!(
        r#"{{"magic":"fgb","version":3,"padding":{padding},"header_size":{header_size},"fb_root_offset":{fb_root_offset},"header_bytes_fetched":{fetched},"url":{url:?}}}"#,
        padding = padding,
        header_size = header_size,
        fb_root_offset = fb_root_offset,
        fetched = header_bytes.len(),
        url = url,
    );
    Ok(json_response(200, &body))
}

// ── Minimal FlatBuffers reader ───────────────────────────────────────────────
//
// FlatGeobuf Header FlatBuffers schema (relevant fields):
//   table Header {
//     name: string;           // field 0
//     envelope: [double];     // field 1  — [minX, minY, maxX, maxY]
//     geometry_type: ubyte;   // field 2
//     has_z: bool;            // field 3
//     has_m: bool;            // field 4
//     has_t: bool;            // field 5
//     has_tm: bool;           // field 6
//     columns: [Column];      // field 7
//     features_count: uint64; // field 8
//     index_node_size: uint16;// field 9  — default 16
//     crs: Crs;               // field 10
//     description: string;    // field 11
//     metadata: string;       // field 12
//   }
//   table Crs { org: string; code: int; name: string; ... }
//
// Binary layout:
//   buf[0..3]         = root_table_offset (u32 LE)
//   buf[root_pos..]   = table: first 4 bytes = soffset_t (i32) to vtable
//   vtable[0..1]      = vtable_size (u16); [2..3] = data_size (u16)
//   vtable[4+n*2..]   = field_n offset from table start (u16, 0 = absent)
//   table[field_off..] = inline scalar, or u32 forward-offset to string/vec/table

fn fb_read_u16(buf: &[u8], pos: usize) -> u16 {
    if pos + 2 > buf.len() { return 0; }
    u16::from_le_bytes([buf[pos], buf[pos + 1]])
}

fn fb_read_i32(buf: &[u8], pos: usize) -> i32 {
    if pos + 4 > buf.len() { return 0; }
    i32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]])
}

fn fb_read_u32(buf: &[u8], pos: usize) -> u32 {
    if pos + 4 > buf.len() { return 0; }
    u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]])
}

fn fb_read_u64(buf: &[u8], pos: usize) -> u64 {
    if pos + 8 > buf.len() { return 0; }
    u64::from_le_bytes([
        buf[pos], buf[pos+1], buf[pos+2], buf[pos+3],
        buf[pos+4], buf[pos+5], buf[pos+6], buf[pos+7],
    ])
}

fn fb_read_f64(buf: &[u8], pos: usize) -> f64 {
    if pos + 8 > buf.len() { return 0.0; }
    f64::from_le_bytes([
        buf[pos], buf[pos+1], buf[pos+2], buf[pos+3],
        buf[pos+4], buf[pos+5], buf[pos+6], buf[pos+7],
    ])
}

/// Return the byte offset (relative to table_pos) for field `field_index`.
/// Returns 0 if the field is absent or the vtable is too small.
fn fb_field_offset(buf: &[u8], table_pos: usize, field_index: usize) -> usize {
    if table_pos + 4 > buf.len() { return 0; }
    let soffset = fb_read_i32(buf, table_pos);
    // vtable is at table_pos - soffset (soffset is positive when vtable precedes table)
    let vtable_pos = (table_pos as i64 - soffset as i64) as usize;
    if vtable_pos + 4 > buf.len() { return 0; }
    let vtable_size = fb_read_u16(buf, vtable_pos) as usize;
    let slot_pos = vtable_pos + 4 + field_index * 2;
    if slot_pos + 2 > vtable_pos + vtable_size { return 0; }
    fb_read_u16(buf, slot_pos) as usize
}

/// Read a u8 scalar from a table field (e.g. geometry_type).
fn fb_u8_field(buf: &[u8], table_pos: usize, field_index: usize) -> u8 {
    let off = fb_field_offset(buf, table_pos, field_index);
    if off == 0 { return 0; }
    let pos = table_pos + off;
    if pos >= buf.len() { return 0; }
    buf[pos]
}

/// Read a u16 scalar from a table field.
fn fb_u16_field(buf: &[u8], table_pos: usize, field_index: usize, default: u16) -> u16 {
    let off = fb_field_offset(buf, table_pos, field_index);
    if off == 0 { return default; }
    fb_read_u16(buf, table_pos + off)
}

/// Read a u64 scalar from a table field.
fn fb_u64_field(buf: &[u8], table_pos: usize, field_index: usize) -> u64 {
    let off = fb_field_offset(buf, table_pos, field_index);
    if off == 0 { return 0; }
    fb_read_u64(buf, table_pos + off)
}

/// Read an i32 scalar from a table field (e.g. CRS code).
fn fb_i32_field(buf: &[u8], table_pos: usize, field_index: usize) -> i32 {
    let off = fb_field_offset(buf, table_pos, field_index);
    if off == 0 { return 0; }
    fb_read_i32(buf, table_pos + off)
}

/// Read a UTF-8 string field. Returns "" if absent or invalid.
fn fb_string_field<'a>(buf: &'a [u8], table_pos: usize, field_index: usize) -> &'a str {
    let off = fb_field_offset(buf, table_pos, field_index);
    if off == 0 { return ""; }
    let ref_pos = table_pos + off;
    let relative = fb_read_u32(buf, ref_pos) as usize;
    let str_pos = ref_pos + relative;
    if str_pos + 4 > buf.len() { return ""; }
    let str_len = fb_read_u32(buf, str_pos) as usize;
    let data_pos = str_pos + 4;
    if data_pos + str_len > buf.len() { return ""; }
    std::str::from_utf8(&buf[data_pos..data_pos + str_len]).unwrap_or("")
}

/// Read a vector of f64 from a table field (e.g. envelope).
fn fb_f64_vec_field(buf: &[u8], table_pos: usize, field_index: usize) -> Vec<f64> {
    let off = fb_field_offset(buf, table_pos, field_index);
    if off == 0 { return vec![]; }
    let ref_pos = table_pos + off;
    let relative = fb_read_u32(buf, ref_pos) as usize;
    let vec_pos = ref_pos + relative;
    if vec_pos + 4 > buf.len() { return vec![]; }
    let count = fb_read_u32(buf, vec_pos) as usize;
    let max = count.min(8); // envelope has at most 4 elements, cap at 8 for safety
    let mut result = Vec::with_capacity(max);
    for i in 0..max {
        result.push(fb_read_f64(buf, vec_pos + 4 + i * 8));
    }
    result
}

/// Get the absolute position of a nested table from a field.
fn fb_table_field_pos(buf: &[u8], table_pos: usize, field_index: usize) -> Option<usize> {
    let off = fb_field_offset(buf, table_pos, field_index);
    if off == 0 { return None; }
    let ref_pos = table_pos + off;
    let relative = fb_read_u32(buf, ref_pos) as usize;
    Some(ref_pos + relative)
}

// ── Phase 4B: FlatGeobuf header parse ────────────────────────────────────────

/// Parse the FlatBuffers-encoded FlatGeobuf Header via two HTTP Range requests.
/// Returns a JSON summary with geometry_type, features_count, envelope, crs.
async fn fgb_parse(url: &str) -> anyhow::Result<Response> {
    // Range request 1: bytes 0-11 — FlatGeobuf magic (8 bytes) + header_size (4 bytes).
    let preamble = range_get(url, 0, 11).await?;
    if preamble.len() < 12 {
        return Ok(json_response(502, &format!(
            r#"{{"error":"short_preamble","got":{}}}"#, preamble.len()
        )));
    }
    if &preamble[0..7] != FGB_MAGIC {
        let hex: String = preamble[0..8].iter().map(|b| format!("{:02x}", b)).collect();
        return Ok(json_response(400, &format!(
            r#"{{"error":"bad_magic","hex":{:?}}}"#, hex
        )));
    }
    let header_size = u32::from_le_bytes([preamble[8], preamble[9], preamble[10], preamble[11]]) as usize;

    // Range request 2: bytes 12..(12+header_size-1) — FlatBuffers Header table.
    let hbuf = range_get(url, 12, 12 + header_size - 1).await?;
    if hbuf.len() < header_size {
        return Ok(json_response(502, &format!(
            r#"{{"error":"incomplete_header","expected":{},"got":{}}}"#, header_size, hbuf.len()
        )));
    }

    // FlatBuffers root: buf[0..3] = u32 offset from buf start to root table.
    let root_pos = fb_read_u32(&hbuf, 0) as usize;

    // ── Extract Header fields ──────────────────────────────────────────────
    let name         = fb_string_field(&hbuf, root_pos, 0);
    let envelope     = fb_f64_vec_field(&hbuf, root_pos, 1);
    let geom_type    = fb_u8_field(&hbuf, root_pos, 2);
    let feat_count   = fb_u64_field(&hbuf, root_pos, 8);
    let idx_node_sz  = fb_u16_field(&hbuf, root_pos, 9, 16);

    // ── Extract CRS sub-table (field 10) ──────────────────────────────────
    let (crs_org, crs_code, crs_name) = if let Some(crs_pos) = fb_table_field_pos(&hbuf, root_pos, 10) {
        (
            fb_string_field(&hbuf, crs_pos, 0).to_string(),
            fb_i32_field(&hbuf, crs_pos, 1),
            fb_string_field(&hbuf, crs_pos, 2).to_string(),
        )
    } else {
        (String::new(), 0i32, String::new())
    };

    let geom_type_name = match geom_type {
        0 => "Unknown",        1 => "Point",           2 => "LineString",
        3 => "Polygon",        4 => "MultiPoint",      5 => "MultiLineString",
        6 => "MultiPolygon",   7 => "GeometryCollection",
        _ => "Other",
    };

    // Format envelope as JSON array, truncating f64 to 6 decimal places.
    let envelope_json = if envelope.len() >= 4 {
        format!(
            "[{:.6},{:.6},{:.6},{:.6}]",
            envelope[0], envelope[1], envelope[2], envelope[3]
        )
    } else {
        "null".to_string()
    };

    // Estimated index byte size: each R-tree node = 4×f64 (bbox) + u64 (offset) = 40 bytes.
    // Total nodes for packed Hilbert R-tree = ceil(features / idx_node_sz) nodes per level,
    // summed across all levels. Simplified approximation:
    let index_bytes_approx = if idx_node_sz > 0 {
        let n = idx_node_sz as u64;
        let mut nodes: u64 = 1;
        let mut level_items = feat_count;
        while level_items > 1 {
            level_items = (level_items + n - 1) / n;
            nodes += level_items;
        }
        nodes * 40
    } else {
        0
    };

    let body = format!(
        r#"{{"name":{name:?},"geometry_type":{geom_type},"geometry_type_name":{geom_type_name:?},"features_count":{feat_count},"index_node_size":{idx_node_sz},"index_bytes_approx":{index_bytes_approx},"envelope":{envelope_json},"crs":{{"org":{crs_org:?},"code":{crs_code},"name":{crs_name:?}}},"header_size":{header_size},"url":{url:?}}}"#,
        name = name,
        geom_type = geom_type,
        geom_type_name = geom_type_name,
        feat_count = feat_count,
        idx_node_sz = idx_node_sz,
        index_bytes_approx = index_bytes_approx,
        envelope_json = envelope_json,
        crs_org = crs_org,
        crs_code = crs_code,
        crs_name = crs_name,
        header_size = header_size,
        url = url,
    );
    Ok(json_response(200, &body))
}

/// Issue a single HTTP Range request: `Range: bytes={start}-{end}`.
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
