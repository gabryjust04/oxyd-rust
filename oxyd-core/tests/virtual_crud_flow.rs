// tests/virtual_crud_flow.rs
//! Integration tests for the virtual CRUD layer (RLS + user_id forcing).
//!
//! Assunzioni lato DB/migrations:
//! - esiste la tabella public.posts con almeno: id (PK), user_id, title, content
//! - su posts è abilitata RLS con policy tipo:
//!     USING (user_id = current_setting('app.current_user_id', true)::int)
//!     WITH CHECK (user_id = current_setting('app.current_user_id', true)::int)
//! - _oxyd_tables espone "posts" e richiede autenticazione (require_auth = true)

use sqlx::PgPool;
use axum::http::StatusCode;
use serde_json::json;

mod utils;
use utils::{
    build_full_app, delete_json, get_json, post_json, read_json, AuthResponse,
};

/// Happy path multi-tenant:
/// - user1 e user2 si registrano
/// - ognuno inserisce un post su /api/posts SENZA passare user_id
/// - il backend forza user_id = utente autenticato
/// - SELECT /api/posts mostra solo le proprie righe (RLS)
#[sqlx::test(migrations = "../oxyd-core/migrations")]
async fn virtual_crud_inserts_owned_rows_and_respects_rls(pool: PgPool) {
    let app = build_full_app(pool);

    // 1) Register user1
    let res = post_json(
        &app,
        "/auth/register",
        json!({ "email": "u1@example.com", "password": "verystrong" }),
        None,
    )
    .await;
    assert!(
        res.status().is_success(),
        "register u1 should succeed, got {}",
        res.status()
    );
    let (_, u1): (StatusCode, AuthResponse) = read_json(res).await;

    // 2) Register user2
    let res = post_json(
        &app,
        "/auth/register",
        json!({ "email": "u2@example.com", "password": "verystrong" }),
        None,
    )
    .await;
    assert!(
        res.status().is_success(),
        "register u2 should succeed, got {}",
        res.status()
    );
    let (_, u2): (StatusCode, AuthResponse) = read_json(res).await;

    // 3) user1 inserisce un post senza user_id
    let res = post_json(
        &app,
        "/api/posts",
        json!({
            "title": "post u1",
            "content": "hello from user1"
        }),
        Some(&u1.access_token),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK, "insert u1 failed: {}", res.status());
    let (_, body): (StatusCode, serde_json::Value) = read_json(res).await;
    let arr = body
        .as_array()
        .expect("insert /api/posts must return JSON array");
    assert_eq!(arr.len(), 1, "insert should return exactly one row");
    let p1 = &arr[0];

    let u1_id = u1.user.id;
    let p1_id = p1["id"]
        .as_i64()
        .expect("post1 must have integer id");

    assert_eq!(
        p1["user_id"].as_i64(),
        Some(u1_id),
        "backend must force user_id = u1.id"
    );
    assert_eq!(p1["title"], "post u1");

    // 4) user2 inserisce un post senza user_id
    let res = post_json(
        &app,
        "/api/posts",
        json!({
            "title": "post u2",
            "content": "hello from user2"
        }),
        Some(&u2.access_token),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK, "insert u2 failed: {}", res.status());
    let (_, body): (StatusCode, serde_json::Value) = read_json(res).await;
    let arr = body.as_array().expect("insert /api/posts must return JSON array");
    assert_eq!(arr.len(), 1);
    let p2 = &arr[0];

    let u2_id = u2.user.id;
    let p2_id = p2["id"]
        .as_i64()
        .expect("post2 must have integer id");

    assert_eq!(
        p2["user_id"].as_i64(),
        Some(u2_id),
        "backend must force user_id = u2.id"
    );
    assert_eq!(p2["title"], "post u2");

    // 5) GET /api/posts come user1 → deve vedere solo i propri (user_id = u1.id)
    let res = get_json(&app, "/api/posts", Some(&u1.access_token)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let (_, body): (StatusCode, serde_json::Value) = read_json(res).await;
    let arr = body
        .as_array()
        .expect("GET /api/posts must return array for u1");

    assert!(
        !arr.is_empty(),
        "u1 should see at least its own post"
    );
    assert!(
        arr.iter()
            .all(|row| row["user_id"].as_i64() == Some(u1_id)),
        "RLS must filter posts so that u1 only sees user_id = u1.id"
    );

    // 6) GET /api/posts come user2 → solo i suoi (user_id = u2.id)
    let res = get_json(&app, "/api/posts", Some(&u2.access_token)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let (_, body): (StatusCode, serde_json::Value) = read_json(res).await;
    let arr = body
        .as_array()
        .expect("GET /api/posts must return array for u2");

    assert!(
        !arr.is_empty(),
        "u2 should see at least its own post"
    );
    assert!(
        arr.iter()
            .all(|row| row["user_id"].as_i64() == Some(u2_id)),
        "RLS must filter posts so that u2 only sees user_id = u2.id"
    );

    // giusto per usare le variabili e non farle "unused"
    assert_ne!(p1_id, p2_id, "post ids should differ");
}

/// Il client NON può overridare user_id: se prova a passare un valore diverso
/// dall'utente autenticato, il backend deve rispondere 400.
#[sqlx::test(migrations = "../oxyd-core/migrations")]
async fn virtual_crud_rejects_explicit_user_id_override(pool: PgPool) {
    let app = build_full_app(pool);

    let res = post_json(
        &app,
        "/auth/register",
        json!({ "email": "override@example.com", "password": "verystrong" }),
        None,
    )
    .await;
    assert!(res.status().is_success());
    let (_, auth): (StatusCode, AuthResponse) = read_json(res).await;

    // Provo a passare user_id "farlocco"
    let res = post_json(
        &app,
        "/api/posts",
        json!({
            "title": "malicious",
            "content": "trying to impersonate",
            "user_id": 999999
        }),
        Some(&auth.access_token),
    )
    .await;

    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "backend must reject explicit user_id override"
    );
}

/// DELETE:
/// - senza filtri → 400 (no mass delete)
/// - con ID di un altro utente → 200 + [] (nessuna riga cancellata grazie a RLS)
/// - con il proprio ID → 200 + [ ... ] e la riga sparisce dai risultati
#[sqlx::test(migrations = "../oxyd-core/migrations")]
async fn virtual_crud_delete_respects_rls_and_requires_filters(pool: PgPool) {
    let app = build_full_app(pool);

    // Register u1 & u2
    let res = post_json(
        &app,
        "/auth/register",
        json!({ "email": "del1@example.com", "password": "verystrong" }),
        None,
    )
    .await;
    let (_, u1): (StatusCode, AuthResponse) = read_json(res).await;

    let res = post_json(
        &app,
        "/auth/register",
        json!({ "email": "del2@example.com", "password": "verystrong" }),
        None,
    )
    .await;
    let (_, u2): (StatusCode, AuthResponse) = read_json(res).await;

    // u1 crea un post
    let res = post_json(
        &app,
        "/api/posts",
        json!({ "title": "u1 post", "content": "to be kept" }),
        Some(&u1.access_token),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let (_, body): (StatusCode, serde_json::Value) = read_json(res).await;
    let p1_id = body[0]["id"].as_i64().expect("u1 post id");

    // u2 crea un post
    let res = post_json(
        &app,
        "/api/posts",
        json!({ "title": "u2 post", "content": "to be deleted or not" }),
        Some(&u2.access_token),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let (_, body): (StatusCode, serde_json::Value) = read_json(res).await;
    let p2_id = body[0]["id"].as_i64().expect("u2 post id");

    // 1) DELETE senza filtri → deve dare 400
    let res = delete_json(&app, "/api/posts", Some(&u1.access_token)).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 2) u1 prova a cancellare il post di u2 usando l'id di u2
    let uri = format!("/api/posts?id=eq.{p2_id}");
    let res = delete_json(&app, &uri, Some(&u1.access_token)).await;
    assert_eq!(res.status(), StatusCode::OK);

    let (_, body): (StatusCode, serde_json::Value) = read_json(res).await;
    let arr = body
        .as_array()
        .expect("DELETE /api/posts must return JSON array");
    assert!(
        arr.is_empty(),
        "RLS must prevent u1 from actually deleting a row owned by u2"
    );

    // Conferma che u2 continua a vedere il suo post
    let res = get_json(&app, "/api/posts", Some(&u2.access_token)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let (_, body): (StatusCode, serde_json::Value) = read_json(res).await;
    let arr = body.as_array().unwrap();
    assert!(
        arr.iter()
            .any(|row| row["id"].as_i64() == Some(p2_id)),
        "u2 must still see its own post after u1 attempted delete"
    );

    // 3) u2 cancella il proprio post
    let uri = format!("/api/posts?id=eq.{p2_id}");
    let res = delete_json(&app, &uri, Some(&u2.access_token)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let (_, body): (StatusCode, serde_json::Value) = read_json(res).await;
    let arr = body.as_array().unwrap();
    assert_eq!(
        arr.len(),
        1,
        "u2 deleting its own post should return exactly one deleted row"
    );
    assert_eq!(
        arr[0]["id"].as_i64(),
        Some(p2_id),
        "deleted row should be the one owned by u2"
    );

    // Ora u2 non deve più vedere quel post
    let res = get_json(&app, "/api/posts", Some(&u2.access_token)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let (_, body): (StatusCode, serde_json::Value) = read_json(res).await;
    let arr = body.as_array().unwrap();
    assert!(
        !arr.iter()
            .any(|row| row["id"].as_i64() == Some(p2_id)),
        "u2 should no longer see its deleted post"
    );

    // u1 continua a vedere il suo post
    let res = get_json(&app, "/api/posts", Some(&u1.access_token)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let (_, body): (StatusCode, serde_json::Value) = read_json(res).await;
    let arr = body.as_array().unwrap();
    assert!(
        arr.iter()
            .any(|row| row["id"].as_i64() == Some(p1_id)),
        "u1 should still see its own post"
    );
}
