mod auth;
mod general;
mod virtual_crud;
use axum::Router;
use dotenvy::dotenv;
use sqlx::postgres::PgPoolOptions;
use std::{time::Duration};
use tokio::net::TcpListener;

use crate::general::types::AppState;


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();

    let database_url = std::env::var("DATABASE_URL")?;
    let jwt_secret = std::env::var("JWT_SECRET")?.into_bytes().into();

    let access_ttl = std::env::var("ACCESS_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(15 * 60));

    let refresh_ttl = std::env::var("REFRESH_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(30 * 24 * 3600));

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await?;

    let state = AppState::new(pool, jwt_secret, access_ttl, refresh_ttl);

    // Monta  il router di auth (login/register/refresh/me) e virtual crud
    // Se la tua funzione costruisce già il Router con lo state, tieni così:
    let app: Router = auth::routes::router(state.clone())
        .merge(virtual_crud::routes::router(state.clone()));


    // In alternativa, se la tua `router()` non prende lo state:
    // let app: Router = auth::routes::router().with_state(state);

    // Bind del listener TCP (niente SocketAddr necessario)
    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    println!("listening on {}", listener.local_addr()?);

    // Avvio server moderno: axum::serve(listener, app)
    axum::serve(listener, app).await?;

    Ok(())
}
