use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use server::{api::AppState, config::Config, router};
use tower::ServiceExt;

#[tokio::test]
async fn health_returns_ok() {
    let app = router(Arc::new(AppState::from_config(Config::load())));
    let res = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}
