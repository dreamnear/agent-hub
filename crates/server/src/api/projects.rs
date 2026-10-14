use axum::{extract::State, Json, Router};
use serde::Deserialize;

use crate::{
    api::SharedState,
    error::AppError,
    projects::{validate_paths, ProjectsStore},
};

pub fn router() -> Router<SharedState> {
    Router::new().route(
        "/api/projects",
        axum::routing::get(list_projects).put(put_projects),
    )
}

async fn list_projects(State(state): State<SharedState>) -> Json<Vec<String>> {
    let store = ProjectsStore::new(&state.cfg);
    Json(store.load().await)
}

#[derive(Debug, Deserialize)]
pub struct Body {
    pub paths: Vec<String>,
}

async fn put_projects(
    State(state): State<SharedState>,
    Json(body): Json<Body>,
) -> Result<Json<Vec<String>>, AppError> {
    validate_paths(&body.paths).await?;
    let store = ProjectsStore::new(&state.cfg);
    store.save(&body.paths).await?;
    Ok(Json(body.paths))
}
