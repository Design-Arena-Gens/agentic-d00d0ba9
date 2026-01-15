use std::{net::SocketAddr, sync::Arc};

use anyhow::Result;
use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use serde::Serialize;
use tracing::info;

use crate::engine::TradingBot;

pub async fn run(bot: Arc<TradingBot>, addr: SocketAddr) -> Result<()> {
    let app = Router::new()
        .route("/health", get(health))
        .route("/portfolio", get(portfolio))
        .with_state(AppState { bot });

    info!(%addr, "starting monitoring api");
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app)
        .await
        .map_err(Into::into)
}

#[derive(Clone)]
struct AppState {
    bot: Arc<TradingBot>,
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match state.bot.health_check().await {
        Ok(ok) => Json(HealthResponse { status: ok }).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: err.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn portfolio(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = state.bot.portfolio_snapshot().await;
    Json(snapshot).into_response()
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}
