//! 静态托管：Embed web/dist 到二进制；FS 托底测试期。

use axum::{
    body::Body,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../web/dist"]
pub struct WebDist;

pub async fn index() -> Response<Body> {
    file_response("index.html").await
}

pub async fn asset(axum::extract::Path(path): axum::extract::Path<String>) -> Response<Body> {
    let key = path.trim_start_matches('/');
    if key.is_empty() {
        return index().await;
    }
    if let Some(f) = WebDist::get(key) {
        return bytes_response(key, f.data.into_owned()).into_response();
    }
    index().await
}

async fn file_response(path: &str) -> Response<Body> {
    if let Some(f) = WebDist::get(path) {
        return bytes_response(path, f.data.into_owned()).into_response();
    }
    let p =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../web/dist/{path}"));
    match tokio::fs::read(&p).await {
        Ok(bytes) => bytes_response(path, bytes).into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            "index.html not built — run `npm --prefix web run build` first",
        )
            .into_response(),
    }
}

fn bytes_response(path: &str, bytes: Vec<u8>) -> Response<Body> {
    let mime = mime_for(path);
    (StatusCode::OK, [(header::CONTENT_TYPE, mime)], bytes).into_response()
}

fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript",
        "css" => "text/css",
        "map" => "application/json",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}
