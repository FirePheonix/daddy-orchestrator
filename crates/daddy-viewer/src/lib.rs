use anyhow::Result;
use axum::{
    extract::{Query, State},
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use daddy_storage::load_trajectory;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Clone)]
struct ViewerState;

pub async fn serve(host: String, port: u16) -> Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/api/trajectory", get(load))
        .with_state(ViewerState);
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    let addr = listener.local_addr()?;
    println!("viewer running at http://{}", format_socket(addr));
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

async fn load(
    State(_state): State<ViewerState>,
    Query(params): Query<BTreeMap<String, String>>,
) -> impl IntoResponse {
    let Some(path) = params.get("path") else {
        return (axum::http::StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "missing path"}))).into_response();
    };
    match load_trajectory(PathBuf::from(path)) {
        Ok(trajectory) => Json(serde_json::to_value(trajectory).unwrap_or_default()).into_response(),
        Err(error) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

fn format_socket(addr: SocketAddr) -> String {
    match addr {
        SocketAddr::V4(v4) => format!("{}:{}", v4.ip(), v4.port()),
        SocketAddr::V6(v6) => format!("[{}]:{}", v6.ip(), v6.port()),
    }
}
