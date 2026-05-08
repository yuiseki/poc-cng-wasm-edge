use spin_sdk::http::{send, IntoResponse, Method, Request, Response};
use spin_sdk::http_component;

const METADATA_JSON: &str = include_str!("../data/metadata.json");
const TILEJSON: &str = include_str!("../data/tilejson.json");
const CAPABILITIES_JSON: &str = include_str!("../data/capabilities.json");

// Remote CNG metadata sources (fixed — not a general-purpose proxy).
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

    if *req.method() != Method::Get {
        return Ok(Response::builder()
            .status(405)
            .header("allow", "GET")
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

        _ => Ok(Response::builder()
            .status(404)
            .header("content-type", "application/json")
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

fn json_response(status: u16, body: &str) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("access-control-allow-origin", "*")
        .body(body.to_owned())
        .build()
}
