use spin_sdk::http::{send, IntoResponse, Method, Request, Response};
use spin_sdk::http_component;

const METADATA_JSON: &str = include_str!("../data/metadata.json");
const TILEJSON: &str = include_str!("../data/tilejson.json");
const CAPABILITIES_JSON: &str = include_str!("../data/capabilities.json");

const FGB_COUNTRIES_URL: &str =
    "https://raw.githubusercontent.com/flatgeobuf/flatgeobuf/master/test/data/countries.fgb";
const FGB_ESA_URL: &str =
    "https://esa-worldcover.s3.eu-central-1.amazonaws.com/esa_worldcover_grid_composites.fgb";
const FGB_MAGIC: &[u8] = b"fgb\x03fgb";

// COG area bounds (Abidjan, Côte d'Ivoire) — default spatial query bbox
const COG_BBOX: [f64; 4] = [-4.179823, 5.174968, -3.824387, 5.531216];

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
    let qi = path_with_query.find('?');
    let path = path_with_query[..qi.unwrap_or(path_with_query.len())]
        .trim_end_matches('/');
    let query = qi.map(|i| &path_with_query[i + 1..]);

    if *req.method() == Method::Options {
        return Ok(Response::builder()
            .status(204)
            .header("access-control-allow-origin", "*")
            .header("access-control-allow-methods", "GET, OPTIONS")
            .header("access-control-allow-headers", "Content-Type")
            .header("timing-allow-origin", "*")
            .body("").build());
    }
    if *req.method() != Method::Get {
        return Ok(Response::builder()
            .status(405).header("allow", "GET, OPTIONS").body("").build());
    }

    match path {
        "" | "/" => Ok(json_response(200,
            r#"{"service":"metadata-function","runtime":"Spin/Wasm","version":"0.1.0","phase":"0+1+2+4"}"#)),
        "/healthz" => Ok(json_response(200, r#"{"status":"ok"}"#)),
        "/metadata" => Ok(json_response(200, METADATA_JSON)),
        "/tilejson" | "/tilejson.json" => Ok(json_response(200, TILEJSON)),
        "/env" => {
            let spin_full_url = req.header("spin-full-url")
                .and_then(|v| v.as_str()).unwrap_or("").to_string();
            Ok(json_response(200, &format!(
                r#"{{"spin_full_url":{:?},"wasm_target":"wasm32-wasip1"}}"#, spin_full_url)))
        }
        "/capabilities" => Ok(json_response(200, CAPABILITIES_JSON)),
        "/remote-metadata" => fetch_remote(REMOTE_METADATA_URL).await,
        "/remote-tilejson" => fetch_remote(REMOTE_TILEJSON_URL).await,
        "/fgb-head" => fgb_head(FGB_COUNTRIES_URL).await,
        "/fgb-head-esa" => fgb_head(FGB_ESA_URL).await,
        "/fgb-parse" => fgb_parse(FGB_COUNTRIES_URL).await,
        "/fgb-parse-esa" => fgb_parse(FGB_ESA_URL).await,
        // Phase 4C: spatial query + GeoJSON decode
        // ?bbox=w,s,e,n  (default: COG/Abidjan area)
        "/fgb-geojson" => {
            let bbox = parse_bbox(query).unwrap_or(COG_BBOX);
            fgb_geojson(FGB_COUNTRIES_URL, bbox).await
        }
        _ => Ok(Response::builder()
            .status(404)
            .header("content-type", "application/json")
            .header("access-control-allow-origin", "*")
            .header("timing-allow-origin", "*")
            .body(r#"{"error":"not found"}"#).build()),
    }
}

fn parse_bbox(query: Option<&str>) -> Option<[f64; 4]> {
    let q = query?;
    let v: Vec<f64> = q.split('&')
        .find(|p| p.starts_with("bbox="))?["bbox=".len()..]
        .split(',')
        .filter_map(|s| s.parse().ok())
        .collect();
    if v.len() == 4 { Some([v[0], v[1], v[2], v[3]]) } else { None }
}

// ── FlatGeobuf header struct ──────────────────────────────────────────────

struct FgbHeader {
    header_size: usize,
    features_count: u64,
    geom_type: u8,
    index_node_size: u16,
    num_nodes: u64,
    columns: Vec<(String, u8)>,  // (name, ColumnType)
}

fn calc_num_nodes(num_items: u64, node_size: u16) -> u64 {
    if node_size <= 1 || num_items == 0 { return num_items; }
    let n = node_size as u64;
    let mut items = num_items;
    let mut total = items;
    loop {
        items = (items + n - 1) / n;
        total += items;
        if items == 1 { break; }
    }
    total
}

async fn parse_fgb_header(url: &str) -> anyhow::Result<FgbHeader> {
    let pre = range_get(url, 0, 11).await?;
    if pre.len() < 12 { anyhow::bail!("short preamble"); }
    if &pre[0..7] != FGB_MAGIC { anyhow::bail!("bad magic"); }
    let header_size = u32::from_le_bytes([pre[8], pre[9], pre[10], pre[11]]) as usize;

    let hbuf = range_get(url, 12, 12 + header_size - 1).await?;
    let root_pos = fb_read_u32(&hbuf, 0) as usize;

    let geom_type      = fb_u8_field(&hbuf, root_pos, 2);
    let features_count = fb_u64_field(&hbuf, root_pos, 8);
    let index_node_sz  = fb_u16_field(&hbuf, root_pos, 9, 16);

    let col_positions  = fb_table_vec_elements(&hbuf, root_pos, 7);
    let columns: Vec<(String, u8)> = col_positions.iter().map(|&cp| {
        let name = fb_string_field(&hbuf, cp, 0).to_string();
        let ctype = fb_u8_field(&hbuf, cp, 1);
        (name, ctype)
    }).collect();

    let num_nodes = calc_num_nodes(features_count, index_node_sz);

    Ok(FgbHeader { header_size, features_count, geom_type, index_node_size: index_node_sz, num_nodes, columns })
}

// ── Phase 4A ──────────────────────────────────────────────────────────────

async fn fgb_head(url: &str) -> anyhow::Result<Response> {
    let step1 = range_get(url, 0, 11).await?;
    if step1.len() < 12 {
        return Ok(json_response(502, &format!(r#"{{"error":"short","got":{}}}"#, step1.len())));
    }
    if &step1[0..7] != FGB_MAGIC {
        return Ok(json_response(400, r#"{"error":"bad_magic"}"#));
    }
    let padding = step1[7];
    let header_size = u32::from_le_bytes([step1[8], step1[9], step1[10], step1[11]]);
    let header_bytes = range_get(url, 12, 12 + header_size as usize - 1).await?;
    let fb_root_offset = fb_read_u32(&header_bytes, 0);
    Ok(json_response(200, &format!(
        r#"{{"magic":"fgb","version":3,"padding":{padding},"header_size":{header_size},"fb_root_offset":{fb_root_offset},"header_bytes_fetched":{fetched},"url":{url:?}}}"#,
        padding=padding, header_size=header_size, fb_root_offset=fb_root_offset,
        fetched=header_bytes.len(), url=url)))
}

// ── Phase 4B ──────────────────────────────────────────────────────────────

async fn fgb_parse(url: &str) -> anyhow::Result<Response> {
    let h = match parse_fgb_header(url).await {
        Ok(h) => h,
        Err(e) => return Ok(json_response(502, &format!(r#"{{"error":{:?}}}"#, e.to_string()))),
    };

    let geom_type_name = geom_type_str(h.geom_type);
    let index_bytes = h.num_nodes * 40;

    let col_names: Vec<String> = h.columns.iter()
        .map(|(n, t)| format!(r#"{{"name":{:?},"type":{}}}"#, n, t))
        .collect();

    let body = format!(
        r#"{{"geometry_type":{gt},"geometry_type_name":{gtn:?},"features_count":{fc},"index_node_size":{ins},"num_nodes":{nn},"index_bytes":{ib},"header_size":{hs},"columns":[{cols}],"url":{url:?}}}"#,
        gt=h.geom_type, gtn=geom_type_name, fc=h.features_count,
        ins=h.index_node_size, nn=h.num_nodes, ib=index_bytes,
        hs=h.header_size, cols=col_names.join(","), url=url);
    Ok(json_response(200, &body))
}

// ── Phase 4C: spatial query + GeoJSON decode ──────────────────────────────

async fn fgb_geojson(url: &str, bbox: [f64; 4]) -> anyhow::Result<Response> {
    let h = match parse_fgb_header(url).await {
        Ok(h) => h,
        Err(e) => return Ok(json_response(502, &format!(r#"{{"error":{:?}}}"#, e.to_string()))),
    };

    // Leaf nodes: first h.features_count nodes, each 40 bytes.
    let index_start  = 12 + h.header_size;
    let leaf_bytes   = h.features_count as usize * 40;
    let leaves       = range_get(url, index_start, index_start + leaf_bytes - 1).await?;

    let feat_data_start = 12 + h.header_size + h.num_nodes as usize * 40;
    let [qw, qs, qe, qn] = bbox;

    let mut feature_jsons: Vec<String> = Vec::new();

    for i in 0..h.features_count as usize {
        let b = i * 40;
        if b + 40 > leaves.len() { break; }
        let min_x = fb_read_f64(&leaves, b);
        let min_y = fb_read_f64(&leaves, b + 8);
        let max_x = fb_read_f64(&leaves, b + 16);
        let max_y = fb_read_f64(&leaves, b + 24);
        let offset = fb_read_u64(&leaves, b + 32);

        // Bbox intersection test
        if max_x < qw || min_x > qe || max_y < qs || min_y > qn { continue; }

        let abs = feat_data_start + offset as usize;
        // Fetch 128 KB from this feature's start position; covers most country polygons
        let chunk = range_get(url, abs, abs + 131071).await?;
        if chunk.len() < 4 { continue; }

        let feat_size = fb_read_u32(&chunk, 0) as usize;
        let feat_buf: Vec<u8> = if feat_size + 4 <= chunk.len() {
            chunk[4..4 + feat_size].to_vec()
        } else {
            range_get(url, abs + 4, abs + 4 + feat_size - 1).await?
        };
        if feat_buf.len() < 4 { continue; }

        let feat_pos = fb_read_u32(&feat_buf, 0) as usize;
        if let Some(fj) = decode_feature(&feat_buf, feat_pos, h.geom_type, &h.columns) {
            feature_jsons.push(fj);
        }
    }

    let body = format!(
        r#"{{"type":"FeatureCollection","features":[{features}],"metadata":{{"matched":{matched},"bbox":[{bw:.6},{bs:.6},{be:.6},{bn:.6}],"source":{url:?}}}}}"#,
        features = feature_jsons.join(","),
        matched  = feature_jsons.len(),
        bw=bbox[0], bs=bbox[1], be=bbox[2], bn=bbox[3],
        url=url);
    Ok(json_response(200, &body))
}

// ── Feature / Geometry / Properties decoders ─────────────────────────────

fn decode_feature(buf: &[u8], feat_pos: usize, header_geom_type: u8, cols: &[(String, u8)]) -> Option<String> {
    let geom_pos = fb_table_field_pos(buf, feat_pos, 0)?;

    // Geometry type: use the Geometry table's own type field (6) if set, else header type.
    let geom_type = {
        let t = fb_u8_field(buf, geom_pos, 6);
        if t == 0 { header_geom_type } else { t }
    };

    let geom_json = match geom_type {
        3 => polygon_geom_json(buf, geom_pos),
        6 => multipolygon_geom_json(buf, geom_pos),
        _ => return None,
    };

    let props_bytes = fb_bytes_field(buf, feat_pos, 1);
    let props_json  = decode_props(props_bytes, cols);

    Some(format!(r#"{{"type":"Feature","geometry":{},"properties":{}}}"#, geom_json, props_json))
}

fn polygon_geom_json(buf: &[u8], geom_pos: usize) -> String {
    let xy   = fb_f64_vec_all(buf, geom_pos, 1);
    let ends = fb_u32_vec(buf, geom_pos, 0);
    let ring_ends: Vec<usize> = if ends.is_empty() { vec![xy.len()] }
                                else { ends.iter().map(|&e| e as usize).collect() };
    let rings = rings_json(&xy, &ring_ends);
    format!(r#"{{"type":"Polygon","coordinates":[{}]}}"#, rings.join(","))
}

fn multipolygon_geom_json(buf: &[u8], geom_pos: usize) -> String {
    let parts = fb_table_vec_elements(buf, geom_pos, 7);
    if parts.is_empty() {
        return polygon_geom_json(buf, geom_pos);
    }
    let polys: Vec<String> = parts.iter().map(|&pp| {
        let xy   = fb_f64_vec_all(buf, pp, 1);
        let ends = fb_u32_vec(buf, pp, 0);
        let ring_ends: Vec<usize> = if ends.is_empty() { vec![xy.len()] }
                                    else { ends.iter().map(|&e| e as usize).collect() };
        format!("[{}]", rings_json(&xy, &ring_ends).join(","))
    }).collect();
    format!(r#"{{"type":"MultiPolygon","coordinates":[{}]}}"#, polys.join(","))
}

fn rings_json(xy: &[f64], ring_ends: &[usize]) -> Vec<String> {
    let mut rings = Vec::with_capacity(ring_ends.len());
    let mut prev = 0usize;
    for &end in ring_ends {
        let end = end.min(xy.len());
        let coords: Vec<String> = xy[prev..end].chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| format!("[{:.6},{:.6}]", c[0], c[1]))
            .collect();
        rings.push(format!("[{}]", coords.join(",")));
        prev = end;
    }
    rings
}

fn decode_props(props: &[u8], cols: &[(String, u8)]) -> String {
    if props.is_empty() { return "{}".to_string(); }
    let mut parts = Vec::new();
    let mut pos = 0usize;
    while pos + 2 <= props.len() {
        let idx = u16::from_le_bytes([props[pos], props[pos + 1]]) as usize;
        pos += 2;
        if idx >= cols.len() { break; }
        let (col_name, col_type) = &cols[idx];
        let val = match col_type {
            0  => { if pos >= props.len() { break; } let v = props[pos] as i8; pos += 1; format!("{}", v) }
            1  => { if pos >= props.len() { break; } let v = props[pos]; pos += 1; format!("{}", v) }
            2  => { if pos >= props.len() { break; } let v = props[pos] != 0; pos += 1; format!("{}", v) }
            3  => { if pos + 2 > props.len() { break; } let v = i16::from_le_bytes([props[pos],props[pos+1]]); pos += 2; format!("{}", v) }
            4  => { if pos + 2 > props.len() { break; } let v = u16::from_le_bytes([props[pos],props[pos+1]]); pos += 2; format!("{}", v) }
            5  => { if pos + 4 > props.len() { break; } let mut b = [0u8;4]; b.copy_from_slice(&props[pos..pos+4]); let v = i32::from_le_bytes(b); pos += 4; format!("{}", v) }
            6  => { if pos + 4 > props.len() { break; } let mut b = [0u8;4]; b.copy_from_slice(&props[pos..pos+4]); let v = u32::from_le_bytes(b); pos += 4; format!("{}", v) }
            7  => { if pos + 8 > props.len() { break; } let mut b = [0u8;8]; b.copy_from_slice(&props[pos..pos+8]); let v = i64::from_le_bytes(b); pos += 8; format!("{}", v) }
            8  => { if pos + 8 > props.len() { break; } let mut b = [0u8;8]; b.copy_from_slice(&props[pos..pos+8]); let v = u64::from_le_bytes(b); pos += 8; format!("{}", v) }
            9  => { if pos + 4 > props.len() { break; } let mut b = [0u8;4]; b.copy_from_slice(&props[pos..pos+4]); let v = f32::from_le_bytes(b); pos += 4; format!("{}", v) }
            10 => { if pos + 8 > props.len() { break; } let mut b = [0u8;8]; b.copy_from_slice(&props[pos..pos+8]); let v = f64::from_le_bytes(b); pos += 8; format!("{}", v) }
            11 | 12 | 13 => {
                if pos + 4 > props.len() { break; }
                let mut b = [0u8;4]; b.copy_from_slice(&props[pos..pos+4]);
                let len = u32::from_le_bytes(b) as usize; pos += 4;
                if pos + len > props.len() { break; }
                let s = std::str::from_utf8(&props[pos..pos+len]).unwrap_or("?");
                pos += len;
                format!("{:?}", s)  // Rust debug fmt = JSON-quoted string
            }
            14 => {
                if pos + 4 > props.len() { break; }
                let mut b = [0u8;4]; b.copy_from_slice(&props[pos..pos+4]);
                let len = u32::from_le_bytes(b) as usize; pos += 4 + len;
                "null".to_string()
            }
            _ => break,
        };
        parts.push(format!("{:?}:{}", col_name, val));
    }
    format!("{{{}}}", parts.join(","))
}

fn geom_type_str(t: u8) -> &'static str {
    match t {
        1 => "Point", 2 => "LineString", 3 => "Polygon",
        4 => "MultiPoint", 5 => "MultiLineString", 6 => "MultiPolygon",
        7 => "GeometryCollection", _ => "Unknown",
    }
}

// ── Minimal FlatBuffers reader ────────────────────────────────────────────

fn fb_read_u16(buf: &[u8], pos: usize) -> u16 {
    if pos + 2 > buf.len() { return 0; }
    u16::from_le_bytes([buf[pos], buf[pos + 1]])
}
fn fb_read_i32(buf: &[u8], pos: usize) -> i32 {
    if pos + 4 > buf.len() { return 0; }
    i32::from_le_bytes([buf[pos], buf[pos+1], buf[pos+2], buf[pos+3]])
}
fn fb_read_u32(buf: &[u8], pos: usize) -> u32 {
    if pos + 4 > buf.len() { return 0; }
    u32::from_le_bytes([buf[pos], buf[pos+1], buf[pos+2], buf[pos+3]])
}
fn fb_read_u64(buf: &[u8], pos: usize) -> u64 {
    if pos + 8 > buf.len() { return 0; }
    u64::from_le_bytes([buf[pos],buf[pos+1],buf[pos+2],buf[pos+3],buf[pos+4],buf[pos+5],buf[pos+6],buf[pos+7]])
}
fn fb_read_f64(buf: &[u8], pos: usize) -> f64 {
    if pos + 8 > buf.len() { return 0.0; }
    f64::from_le_bytes([buf[pos],buf[pos+1],buf[pos+2],buf[pos+3],buf[pos+4],buf[pos+5],buf[pos+6],buf[pos+7]])
}

/// Byte offset from table_pos to field slot; 0 = absent.
fn fb_field_offset(buf: &[u8], table_pos: usize, field_index: usize) -> usize {
    if table_pos + 4 > buf.len() { return 0; }
    let soffset  = fb_read_i32(buf, table_pos);
    // vtable_pos = table_pos - soffset  (soffset is positive when vtable precedes table)
    let vtable_pos = (table_pos as i64 - soffset as i64) as usize;
    if vtable_pos + 4 > buf.len() { return 0; }
    let vt_size  = fb_read_u16(buf, vtable_pos) as usize;
    let slot_pos = vtable_pos + 4 + field_index * 2;
    if slot_pos + 2 > vtable_pos + vt_size { return 0; }
    fb_read_u16(buf, slot_pos) as usize
}

fn fb_u8_field(buf: &[u8], tp: usize, fi: usize) -> u8 {
    let off = fb_field_offset(buf, tp, fi);
    if off == 0 || tp + off >= buf.len() { return 0; }
    buf[tp + off]
}
fn fb_u16_field(buf: &[u8], tp: usize, fi: usize, default: u16) -> u16 {
    let off = fb_field_offset(buf, tp, fi);
    if off == 0 { return default; }
    fb_read_u16(buf, tp + off)
}
fn fb_u64_field(buf: &[u8], tp: usize, fi: usize) -> u64 {
    let off = fb_field_offset(buf, tp, fi);
    if off == 0 { return 0; }
    fb_read_u64(buf, tp + off)
}

fn fb_string_field<'a>(buf: &'a [u8], tp: usize, fi: usize) -> &'a str {
    let off = fb_field_offset(buf, tp, fi);
    if off == 0 { return ""; }
    let ref_pos  = tp + off;
    let rel      = fb_read_u32(buf, ref_pos) as usize;
    let str_pos  = ref_pos + rel;
    if str_pos + 4 > buf.len() { return ""; }
    let len      = fb_read_u32(buf, str_pos) as usize;
    let data_pos = str_pos + 4;
    if data_pos + len > buf.len() { return ""; }
    std::str::from_utf8(&buf[data_pos..data_pos + len]).unwrap_or("")
}

/// f64 vector, uncapped (for polygon coordinates).
fn fb_f64_vec_all(buf: &[u8], tp: usize, fi: usize) -> Vec<f64> {
    fb_f64_vec_raw(buf, tp, fi, usize::MAX)
}

fn fb_f64_vec_raw(buf: &[u8], tp: usize, fi: usize, cap: usize) -> Vec<f64> {
    let off = fb_field_offset(buf, tp, fi);
    if off == 0 { return vec![]; }
    let ref_pos = tp + off;
    let rel     = fb_read_u32(buf, ref_pos) as usize;
    let vec_pos = ref_pos + rel;
    if vec_pos + 4 > buf.len() { return vec![]; }
    let count = fb_read_u32(buf, vec_pos) as usize;
    let max   = count.min(cap).min((buf.len() - vec_pos - 4) / 8);
    (0..max).map(|i| fb_read_f64(buf, vec_pos + 4 + i * 8)).collect()
}

/// u32 vector (for ring ends).
fn fb_u32_vec(buf: &[u8], tp: usize, fi: usize) -> Vec<u32> {
    let off = fb_field_offset(buf, tp, fi);
    if off == 0 { return vec![]; }
    let ref_pos = tp + off;
    let rel     = fb_read_u32(buf, ref_pos) as usize;
    let vec_pos = ref_pos + rel;
    if vec_pos + 4 > buf.len() { return vec![]; }
    let count = fb_read_u32(buf, vec_pos) as usize;
    let max   = count.min((buf.len() - vec_pos - 4) / 4);
    (0..max).map(|i| fb_read_u32(buf, vec_pos + 4 + i * 4)).collect()
}

/// Raw bytes field (for Feature properties).
fn fb_bytes_field<'a>(buf: &'a [u8], tp: usize, fi: usize) -> &'a [u8] {
    let off = fb_field_offset(buf, tp, fi);
    if off == 0 { return &[]; }
    let ref_pos = tp + off;
    let rel     = fb_read_u32(buf, ref_pos) as usize;
    let vec_pos = ref_pos + rel;
    if vec_pos + 4 > buf.len() { return &[]; }
    let len     = fb_read_u32(buf, vec_pos) as usize;
    let dp      = vec_pos + 4;
    if dp + len > buf.len() { return &[]; }
    &buf[dp..dp + len]
}

/// Absolute positions of nested table elements in a [Table] vector field.
fn fb_table_vec_elements(buf: &[u8], tp: usize, fi: usize) -> Vec<usize> {
    let off = fb_field_offset(buf, tp, fi);
    if off == 0 { return vec![]; }
    let ref_pos = tp + off;
    let rel     = fb_read_u32(buf, ref_pos) as usize;
    let vec_pos = ref_pos + rel;
    if vec_pos + 4 > buf.len() { return vec![]; }
    let count = fb_read_u32(buf, vec_pos) as usize;
    let max   = count.min(10_000);
    let mut result = Vec::with_capacity(max);
    for i in 0..max {
        let slot_pos = vec_pos + 4 + i * 4;
        if slot_pos + 4 > buf.len() { break; }
        let sub = fb_read_u32(buf, slot_pos) as usize;
        result.push(slot_pos + sub);
    }
    result
}

/// Absolute position of a single nested table field.
fn fb_table_field_pos(buf: &[u8], tp: usize, fi: usize) -> Option<usize> {
    let off = fb_field_offset(buf, tp, fi);
    if off == 0 { return None; }
    let ref_pos = tp + off;
    let rel     = fb_read_u32(buf, ref_pos) as usize;
    Some(ref_pos + rel)
}

// ── Outbound HTTP helpers ─────────────────────────────────────────────────

async fn fetch_remote(url: &str) -> anyhow::Result<Response> {
    match send::<_, Response>(Request::get(url)).await {
        Ok(resp) => {
            let status = *resp.status();
            let body   = resp.into_body();
            Ok(Response::builder().status(status)
                .header("content-type", "application/json")
                .header("access-control-allow-origin", "*")
                .header("access-control-allow-methods", "GET, OPTIONS")
                .header("access-control-allow-headers", "Content-Type")
                .header("timing-allow-origin", "*")
                .header("x-fetched-from", url)
                .body(body).build())
        }
        Err(e) => Ok(Response::builder().status(403)
            .header("content-type", "application/json")
            .body(format!(r#"{{"error":"outbound_denied","detail":{:?}}}"#, e.to_string()))
            .build()),
    }
}

async fn range_get(url: &str, start: usize, end: usize) -> anyhow::Result<Vec<u8>> {
    let range_val = format!("bytes={}-{}", start, end);
    let mut req = Request::get(url);
    req.header("range", &range_val);
    let resp: Response = send(req).await?;
    Ok(resp.into_body().to_vec())
}

fn json_response(status: u16, body: &str) -> Response {
    Response::builder().status(status)
        .header("content-type", "application/json")
        .header("access-control-allow-origin", "*")
        .header("access-control-allow-methods", "GET, OPTIONS")
        .header("access-control-allow-headers", "Content-Type")
        .header("timing-allow-origin", "*")
        .body(body.to_owned()).build()
}
