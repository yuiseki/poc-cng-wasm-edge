use spin_sdk::http::{IntoResponse, Method, Request, Response};
use spin_sdk::http_component;

const METADATA_JSON: &str = include_str!("../data/metadata.json");
const TILEJSON: &str = include_str!("../data/tilejson.json");

#[http_component]
fn handle(req: Request) -> anyhow::Result<impl IntoResponse> {
    // uri() returns the full URL string in spin-sdk v5 (e.g. "http://host/path?q").
    // Extract just the path portion.
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
    let method = req.method();

    // Only allow GET
    if *method != Method::Get {
        return Ok(Response::builder()
            .status(405)
            .header("allow", "GET")
            .body("")
            .build());
    }

    match path {
        "" | "/" => Ok(json_response(
            200,
            r#"{"service":"metadata-function","runtime":"Spin/Wasm","version":"0.1.0"}"#,
        )),

        "/healthz" => Ok(json_response(200, r#"{"status":"ok"}"#)),

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

        _ => Ok(Response::builder()
            .status(404)
            .header("content-type", "application/json")
            .body(r#"{"error":"not found"}"#)
            .build()),
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
