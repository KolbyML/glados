use std::sync::Arc;
use std::{net::SocketAddr, path::Path};

use anyhow::{bail, Result};
use axum::http::StatusCode;
use axum::{
    extract::Extension,
    routing::{get, get_service},
    Router,
};
use tower_http::services::ServeDir;
use tracing::info;

use ethereum_types::U256;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

pub mod cli;
pub mod routes;
pub mod state;
pub mod templates;

use crate::state::State;

const SOCKET: &str = "0.0.0.0:3001";

pub async fn run_glados_web(config: Arc<State>) -> Result<()> {
    let Some(parent) = Path::new(std::file!()).parent() else {bail!("No parent of config file")};
    let Some(grandparent) = parent.parent() else {bail!("No grandparent of config file")};
    let assets_path = grandparent.join("assets");

    let serve_dir = get_service(ServeDir::new(assets_path)).handle_error(routes::handle_error);

    let nodes_with_zero_high_bits = entity::node::Entity::find()
        .filter(entity::node::Column::NodeIdHigh.eq(0))
        .all(&config.database_connection)
        .await
        .unwrap();

    info!(rows=?nodes_with_zero_high_bits.len(), "One time migration: setting high bits for node model");

    for node_model in nodes_with_zero_high_bits {
        let raw_node_id = U256::from_big_endian(node_model.get_node_id().raw().as_slice());
        let node_id_high: i64 = (raw_node_id >> 193).as_u64().try_into().unwrap();

        let mut node: entity::node::ActiveModel = node_model.into();
        let previous_value = node.node_id_high;
        node.node_id_high = Set(node_id_high);
        let updated = node.update(&config.database_connection).await?;
        info!(row.id=?updated.id, old=?previous_value, new=?updated.node_id_high, "Setting high bits");
    }

    // setup router
    let app = Router::new()
        .route("/", get(routes::root))
        .route("/network/", get(routes::network_dashboard))
        .route("/network/node/:node_id_hex/", get(routes::node_detail))
        .route(
            "/network/node/:node_id_hex/enr/:enr_seq/",
            get(routes::enr_detail),
        )
        .route("/content/", get(routes::content_dashboard))
        .route("/content/id/", get(routes::contentid_list))
        .route(
            "/content/id/:content_id_hex/",
            get(routes::contentid_detail),
        )
        .route("/content/key/", get(routes::contentkey_list))
        .route(
            "/content/key/:content_key_hex/",
            get(routes::contentkey_detail),
        )
        .route("/audit/id/:audit_id", get(routes::contentaudit_detail))
        .nest_service("/static/", serve_dir.clone())
        .fallback_service(serve_dir)
        .layer(Extension(config));

    let app = app.fallback(handler_404);

    let socket: SocketAddr = SOCKET.parse()?;
    info!("Serving glados-web at {}", socket);
    Ok(axum::Server::bind(&socket)
        .serve(app.into_make_service())
        .await?)
}

/// Global routing error handler to prevent panics.
async fn handler_404() -> StatusCode {
    tracing::error!("404: Non-existent page visited");
    StatusCode::NOT_FOUND
}
