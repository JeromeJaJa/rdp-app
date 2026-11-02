//! Local REST API (loopback only, bearer token) — everything a user can do in
//! the window, for scripts and AI agents. Docs: `/docs`, `/openapi.json`, `/llms.txt`.

mod spec;

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::Stream;
use futures_util::StreamExt as _;
use rdp_engine::InputEvent;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::bulk;
use crate::core::{Core, Origin};
use crate::store::{
    ConnectionPatch, ExportOptions, GatewayInput, ImportOptions, ImportSource, NewConnection, OrderEntry,
};

type AppState = Arc<Core>;

// ── Errors ──────────────────────────────────────────────────────────────────

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        let msg = format!("{e:#}");
        let status = if msg.contains("not found") {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        };
        ApiError(status, msg)
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

type ApiResult<T> = Result<T, ApiError>;

fn ok() -> Json<Value> {
    Json(json!({ "ok": true }))
}

// ── Server lifecycle ────────────────────────────────────────────────────────

/// Starts, stops or restarts the server to match the current settings.
pub fn ensure_running(core: &Arc<Core>) {
    let s = core.settings();
    let enabled = s.get("apiEnabled").and_then(Value::as_bool).unwrap_or(true);
    let port = s
        .get("apiPort")
        .and_then(Value::as_u64)
        .unwrap_or(47123)
        .clamp(1024, 65535) as u16;

    let mut rt = core.api.lock();
    let up_to_date = if enabled {
        rt.shutdown.is_some() && rt.port == port
    } else {
        rt.shutdown.is_none()
    };
    if up_to_date {
        return;
    }
    if let Some(stop) = rt.shutdown.take() {
        let _ = stop.send(());
    }
    rt.running = false;
    rt.error = None;
    rt.port = port;
    if !enabled {
        return;
    }

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    rt.shutdown = Some(tx);
    drop(rt);

    let core = Arc::clone(core);
    tauri::async_runtime::spawn(async move {
        // A previous server on the same port may still be shutting down.
        let mut listener = None;
        let mut last_err = None;
        for _ in 0..20 {
            match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                Ok(l) => {
                    listener = Some(l);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        let Some(listener) = listener else {
            let msg = format!(
                "cannot listen on 127.0.0.1:{port}: {}",
                last_err.map(|e| e.to_string()).unwrap_or_default()
            );
            tracing::warn!("{msg}");
            let mut rt = core.api.lock();
            rt.running = false;
            rt.error = Some(msg);
            return;
        };
        {
            let mut rt = core.api.lock();
            rt.running = true;
            rt.error = None;
        }
        tracing::info!(port, "local API listening");
        let app = router(Arc::clone(&core), port);
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, "local API stopped");
        }
        let mut rt = core.api.lock();
        if rt.port == port && rt.shutdown.is_none() {
            rt.running = false;
        }
    });
}

fn router(core: AppState, port: u16) -> Router {
    let api = Router::new()
        .route("/state", get(state))
        .route("/events", get(events))
        .route("/connections", get(list_connections).post(add_connections))
        .route("/connections/bulk", post(bulk_add))
        .route("/connections/delete", post(delete_many))
        .route("/connections/reorder", post(reorder_connections))
        .route(
            "/connections/{id}",
            get(get_connection).patch(patch_connection).delete(delete_connection),
        )
        .route("/connections/{id}/duplicate", post(duplicate_connection))
        .route("/groups", get(list_groups).post(create_group))
        .route("/groups/reorder", post(reorder_groups))
        .route("/groups/{id}", axum::routing::patch(patch_group).delete(delete_group))
        .route("/groups/{id}/replace", post(replace_group))
        .route("/groups/{id}/connect", post(connect_group))
        .route("/groups/{id}/disconnect", post(disconnect_group))
        .route("/groups/{id}/export", get(export_group))
        .route("/sessions", get(list_sessions))
        .route("/sessions/connect", post(connect_many))
        .route("/sessions/disconnect", post(disconnect_many))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/connect", post(connect_one))
        .route("/sessions/{id}/disconnect", post(disconnect_one))
        .route("/sessions/{id}/reconnect", post(reconnect_one))
        .route("/sessions/{id}/screenshot", get(screenshot))
        .route("/sessions/{id}/keys", post(keys))
        .route("/sessions/{id}/type", post(type_text))
        .route("/sessions/{id}/mouse", post(mouse))
        .route("/sessions/{id}/input", post(raw_input))
        .route("/sessions/{id}/ctrl-alt-del", post(ctrl_alt_del))
        .route("/sessions/{id}/stats", get(session_stats))
        .route("/stats", get(all_stats))
        .route("/files/send", post(send_files))
        .route("/run", post(run_command))
        .route("/transfers", get(list_transfers))
        .route("/transfers/{id}/cancel", post(cancel_transfer))
        .route("/certificates", get(list_certificates))
        .route("/certificates/trust", post(trust_certificate))
        .route("/certificates/{endpoint}", axum::routing::delete(forget_certificate))
        .route("/settings", get(get_settings).patch(patch_settings))
        .route("/export", get(export_get).post(export_post))
        .route("/import/preview", post(import_preview))
        .route("/import", post(import))
        .route("/ui", get(get_ui).post(post_ui))
        .route_layer(middleware::from_fn_with_state(Arc::clone(&core), auth))
        .route("/health", get(health));

    Router::new()
        .route("/", get(|| async { Redirect::temporary("/docs") }))
        .route("/docs", get(docs))
        .route("/openapi.json", get(openapi))
        .route("/llms.txt", get(llms))
        .nest("/api/v1", api)
        .layer(middleware::from_fn(move |req: Request, next: Next| {
            host_guard(req, next, port)
        }))
        .with_state(core)
}

/// Rejects requests whose Host isn't loopback (DNS-rebinding protection).
async fn host_guard(req: Request, next: Next, port: u16) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let allowed = [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
        "127.0.0.1".to_owned(),
        "localhost".to_owned(),
    ];
    if !allowed.iter().any(|a| a.eq_ignore_ascii_case(host)) {
        return ApiError(StatusCode::FORBIDDEN, "requests must target 127.0.0.1".into()).into_response();
    }
    next.run(req).await
}

async fn auth(State(core): State<AppState>, req: Request, next: Next) -> Response {
    let expected = core.api_token();
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer ")))
        .map(str::trim)
        .map(str::to_owned);
    let query = req
        .uri()
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")).map(str::to_owned));
    let given = bearer.or(query).unwrap_or_default();
    if expected.is_empty() || !constant_time_eq(given.as_bytes(), expected.as_bytes()) {
        return ApiError(
            StatusCode::UNAUTHORIZED,
            "missing or wrong token (Authorization: Bearer <token>; see Settings → Automation)".into(),
        )
        .into_response();
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

fn port_of(core: &Core) -> u16 {
    core.api.lock().port
}

// ── Docs ────────────────────────────────────────────────────────────────────

async fn docs() -> Html<&'static str> {
    Html(include_str!("docs.html"))
}

async fn openapi(State(core): State<AppState>) -> Json<Value> {
    Json(spec::openapi(&base_url(port_of(&core))))
}

async fn llms(State(core): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        spec::llms_txt(&base_url(port_of(&core))),
    )
}

async fn health() -> Json<Value> {
    Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

// ── State & events ──────────────────────────────────────────────────────────

async fn state(State(core): State<AppState>) -> Json<Value> {
    let mut library = serde_json::to_value(core.library()).unwrap_or_default();
    library["settings"] = Value::Object(core.settings());
    Json(json!({
        "library": library,
        "sessions": core.sessions(),
        "ui": core.ui_state(),
    }))
}

async fn events(State(core): State<AppState>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = core.subscribe();
    let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|item| async move {
        match item {
            Ok(value) => {
                // Cursor bitmaps are noise for API clients.
                if value.get("type").and_then(Value::as_str) == Some("cursor") {
                    return None;
                }
                Some(Ok(Event::default().json_data(value).unwrap_or_default()))
            }
            Err(_) => None, // lagged: skip
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ── Connections ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ListQuery {
    group: Option<String>,
    q: Option<String>,
    status: Option<String>,
}

async fn list_connections(State(core): State<AppState>, Query(q): Query<ListQuery>) -> Json<Value> {
    let lib = core.library();
    let sessions = core.sessions();
    let needle = q.q.as_deref().map(str::to_lowercase);
    let list: Vec<&crate::store::Connection> = lib
        .connections
        .iter()
        .filter(|c| q.group.as_deref().is_none_or(|g| c.group_id == g))
        .filter(|c| {
            needle.as_deref().is_none_or(|n| {
                c.name.to_lowercase().contains(n)
                    || c.host.to_lowercase().contains(n)
                    || c.username.to_lowercase().contains(n)
            })
        })
        .filter(|c| {
            q.status
                .as_deref()
                .is_none_or(|st| sessions.iter().any(|s| s.id == c.id && s.status == st))
        })
        .collect();
    Json(serde_json::to_value(list).unwrap_or_default())
}

async fn add_connections(State(core): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let items: Vec<NewConnection> = if body.is_array() {
        serde_json::from_value(body).map_err(|e| bad(e.to_string()))?
    } else {
        vec![serde_json::from_value(body).map_err(|e| bad(e.to_string()))?]
    };
    if items.iter().any(|i| i.host.trim().is_empty()) {
        return Err(bad("host is required"));
    }
    let added = core.add_connections(items, Origin::Api)?;
    Ok(Json(serde_json::to_value(added).unwrap_or_default()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BulkBody {
    text: String,
    #[serde(default)]
    group_id: Option<String>,
    #[serde(default)]
    mode: Option<String>,
}

fn parse_bulk(text: &str, group: Option<String>) -> (Vec<NewConnection>, Vec<bulk::LineError>) {
    let parsed = bulk::parse(text);
    let items = parsed
        .entries
        .into_iter()
        .map(|(_, e)| NewConnection::from_entry(e, group.clone()))
        .collect();
    (items, parsed.errors)
}

async fn bulk_add(State(core): State<AppState>, Json(body): Json<BulkBody>) -> ApiResult<Json<Value>> {
    let group = body.group_id.unwrap_or_else(|| crate::store::DEFAULT_GROUP.into());
    let (items, errors) = parse_bulk(&body.text, Some(group.clone()));
    if body.mode.as_deref() == Some("replace") {
        let r = core.replace_group(&group, items, Origin::Api)?;
        return Ok(Json(
            json!({ "added": r.added, "kept": r.kept, "removed": r.removed, "errors": errors }),
        ));
    }
    let added = core.add_connections(items, Origin::Api)?;
    Ok(Json(
        json!({ "added": added.len(), "kept": 0, "removed": 0, "errors": errors, "connections": added }),
    ))
}

async fn get_connection(State(core): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(serde_json::to_value(core.connection(&id)?).unwrap_or_default()))
}

async fn patch_connection(
    State(core): State<AppState>,
    Path(id): Path<String>,
    Json(patch): Json<ConnectionPatch>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        serde_json::to_value(core.update_connection(&id, patch, Origin::Api)?).unwrap_or_default(),
    ))
}

async fn delete_connection(State(core): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    core.connection(&id)?;
    core.delete_connections(&[id], Origin::Api)?;
    Ok(ok())
}

#[derive(Deserialize)]
struct IdsBody {
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    all: bool,
}

async fn delete_many(State(core): State<AppState>, Json(body): Json<IdsBody>) -> ApiResult<Json<Value>> {
    core.delete_connections(&body.ids, Origin::Api)?;
    Ok(ok())
}

async fn duplicate_connection(State(core): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let c = core.connection(&id)?;
    let item = NewConnection {
        name: Some(if c.name.is_empty() {
            format!("{} copy", c.host)
        } else {
            format!("{} copy", c.name)
        }),
        host: c.host,
        port: Some(c.port),
        username: c.username,
        domain: Some(c.domain),
        group_id: Some(c.group_id),
        notes: Some(c.notes),
        copy_password_from: Some(id),
        ..Default::default()
    };
    let added = core.add_connections(vec![item], Origin::Api)?;
    Ok(Json(serde_json::to_value(&added[0]).unwrap_or_default()))
}

#[derive(Deserialize)]
struct ReorderBody {
    order: Vec<OrderEntry>,
}

async fn reorder_connections(State(core): State<AppState>, Json(body): Json<ReorderBody>) -> ApiResult<Json<Value>> {
    core.reorder(body.order, Origin::Api)?;
    Ok(ok())
}

// ── Groups ──────────────────────────────────────────────────────────────────

async fn list_groups(State(core): State<AppState>) -> Json<Value> {
    let lib = core.library();
    let list: Vec<Value> = lib
        .groups
        .iter()
        .map(|g| {
            let mut v = serde_json::to_value(g).unwrap_or_default();
            v["connections"] = json!(lib.connections.iter().filter(|c| c.group_id == g.id).count());
            v
        })
        .collect();
    Json(Value::Array(list))
}

#[derive(Deserialize)]
struct CreateGroupBody {
    name: String,
    #[serde(default)]
    gateway: Option<GatewayInput>,
}

async fn create_group(State(core): State<AppState>, Json(body): Json<CreateGroupBody>) -> ApiResult<Json<Value>> {
    Ok(Json(
        serde_json::to_value(core.create_group(&body.name, body.gateway, Origin::Api)?).unwrap_or_default(),
    ))
}

#[derive(Deserialize)]
struct PatchGroupBody {
    name: Option<String>,
    collapsed: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    gateway: Option<Option<GatewayInput>>,
    /// The group's complete set of connection-setting overrides.
    settings: Option<serde_json::Map<String, Value>>,
}

fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

async fn patch_group(
    State(core): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<PatchGroupBody>,
) -> ApiResult<Json<Value>> {
    let mut g = core.update_group(&id, body.name, body.collapsed, body.gateway, Origin::Api)?;
    if let Some(settings) = body.settings {
        g = core.set_group_settings(&id, settings, Origin::Api)?;
    }
    Ok(Json(serde_json::to_value(g).unwrap_or_default()))
}

#[derive(Deserialize)]
struct DeleteGroupQuery {
    connections: Option<String>,
}

async fn delete_group(
    State(core): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DeleteGroupQuery>,
) -> ApiResult<Json<Value>> {
    core.delete_group(&id, q.connections.as_deref() == Some("delete"), Origin::Api)?;
    Ok(ok())
}

async fn reorder_groups(State(core): State<AppState>, Json(body): Json<IdsBody>) -> ApiResult<Json<Value>> {
    core.reorder_groups(&body.ids, Origin::Api)?;
    Ok(ok())
}

#[derive(Deserialize)]
struct ReplaceBody {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    connections: Option<Vec<NewConnection>>,
}

async fn replace_group(
    State(core): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ReplaceBody>,
) -> ApiResult<Json<Value>> {
    core.group(&id)?;
    let (items, errors) = match (body.text, body.connections) {
        (Some(text), _) => parse_bulk(&text, Some(id.clone())),
        (None, Some(list)) => (list, Vec::new()),
        (None, None) => return Err(bad("send \"text\" (bulk lines) or \"connections\"")),
    };
    let r = core.replace_group(&id, items, Origin::Api)?;
    Ok(Json(
        json!({ "kept": r.kept, "added": r.added, "removed": r.removed, "errors": errors }),
    ))
}

#[derive(Deserialize)]
struct WaitQuery {
    #[serde(default)]
    wait: Option<bool>,
    #[serde(default)]
    timeout: Option<f64>,
}

impl WaitQuery {
    fn timeout(&self) -> Duration {
        Duration::from_secs_f64(self.timeout.unwrap_or(30.0).clamp(1.0, 300.0))
    }
}

async fn connect_ids(core: &Core, ids: Vec<String>, q: &WaitQuery) -> Json<Value> {
    core.connect(&ids);
    if q.wait.unwrap_or(false) {
        // Give the engine a moment to register the sessions before polling.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let sessions = core.wait_settled(&ids, q.timeout()).await;
        return Json(json!({ "sessions": sessions }));
    }
    Json(json!({ "sessions": core.sessions().into_iter().filter(|s| ids.contains(&s.id)).collect::<Vec<_>>() }))
}

async fn connect_group(
    State(core): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<WaitQuery>,
) -> ApiResult<Json<Value>> {
    let ids = core.connection_ids_in(&id)?;
    Ok(connect_ids(&core, ids, &q).await)
}

async fn disconnect_group(State(core): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let ids = core.connection_ids_in(&id)?;
    core.disconnect(&ids);
    Ok(ok())
}

// ── Sessions ────────────────────────────────────────────────────────────────

async fn list_sessions(State(core): State<AppState>) -> Json<Value> {
    Json(serde_json::to_value(core.sessions()).unwrap_or_default())
}

async fn get_session(State(core): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(serde_json::to_value(core.session(&id)?).unwrap_or_default()))
}

async fn connect_many(
    State(core): State<AppState>,
    Query(q): Query<WaitQuery>,
    Json(body): Json<IdsBody>,
) -> ApiResult<Json<Value>> {
    let ids = if body.all { core.all_connection_ids() } else { body.ids };
    Ok(connect_ids(&core, ids, &q).await)
}

async fn disconnect_many(State(core): State<AppState>, Json(body): Json<IdsBody>) -> ApiResult<Json<Value>> {
    if body.all {
        core.engine.disconnect_all();
    } else {
        core.disconnect(&body.ids);
    }
    Ok(ok())
}

async fn connect_one(
    State(core): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<WaitQuery>,
) -> ApiResult<Json<Value>> {
    core.connection(&id)?;
    let Json(v) = connect_ids(&core, vec![id], &q).await;
    Ok(Json(v["sessions"].get(0).cloned().unwrap_or(Value::Null)))
}

async fn disconnect_one(State(core): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    core.connection(&id)?;
    core.disconnect(&[id]);
    Ok(ok())
}

async fn reconnect_one(
    State(core): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<WaitQuery>,
) -> ApiResult<Json<Value>> {
    core.connection(&id)?;
    let before = core.engine.generation(&id);
    core.reconnect(std::slice::from_ref(&id));
    if q.wait.unwrap_or(false) {
        // Wait for the new session to replace the old one before waiting for it to settle.
        for _ in 0..100 {
            let now = core.engine.generation(&id);
            if now.is_some() && now != before {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let s = core.wait_settled(std::slice::from_ref(&id), q.timeout()).await;
        return Ok(Json(
            s.into_iter()
                .next()
                .map(|s| serde_json::to_value(s).unwrap_or_default())
                .unwrap_or(Value::Null),
        ));
    }
    Ok(Json(serde_json::to_value(core.session(&id)?).unwrap_or_default()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScreenshotQuery {
    max_width: Option<u32>,
}

async fn screenshot(
    State(core): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<ScreenshotQuery>,
) -> ApiResult<Response> {
    core.connection(&id)?;
    let shot = core
        .engine
        .screenshot(&id)
        .await
        .map_err(|e| ApiError(StatusCode::CONFLICT, format!("{e:#}")))?;
    let png = tokio::task::spawn_blocking(move || encode_png(shot, q.max_width))
        .await
        .map_err(|e| bad(e.to_string()))??;
    Ok((
        [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "no-store")],
        png,
    )
        .into_response())
}

fn encode_png(shot: rdp_engine::Screenshot, max_width: Option<u32>) -> anyhow::Result<Vec<u8>> {
    let (mut w, mut h, mut rgba) = (shot.width, shot.height, shot.rgba);
    if let Some(max) = max_width.filter(|m| *m >= 16 && *m < w) {
        let (nw, nh) = (max, ((u64::from(h) * u64::from(max)) / u64::from(w)).max(1) as u32);
        let mut out = vec![0u8; (nw * nh * 4) as usize];
        // Area-average 2×2 samples per output pixel.
        for y in 0..nh {
            for x in 0..nw {
                let mut acc = [0u32; 3];
                for (sx, sy) in [(1u64, 1u64), (3, 1), (1, 3), (3, 3)] {
                    let px = ((u64::from(x) * 4 + sx) * u64::from(w) / (u64::from(nw) * 4)).min(u64::from(w) - 1);
                    let py = ((u64::from(y) * 4 + sy) * u64::from(h) / (u64::from(nh) * 4)).min(u64::from(h) - 1);
                    let i = ((py * u64::from(w) + px) * 4) as usize;
                    for c in 0..3 {
                        acc[c] += u32::from(rgba[i + c]);
                    }
                }
                let o = ((y * nw + x) * 4) as usize;
                for c in 0..3 {
                    out[o + c] = ((acc[c] + 2) / 4) as u8;
                }
                out[o + 3] = 255;
            }
        }
        (w, h, rgba) = (nw, nh, out);
    }
    let mut bytes = Vec::with_capacity(rgba.len() / 3);
    {
        let mut enc = png::Encoder::new(&mut bytes, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header()?;
        writer.write_image_data(&rgba)?;
    }
    Ok(bytes)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeysBody {
    keys: Value,
    #[serde(default)]
    delay_ms: Option<u64>,
}

async fn keys(
    State(core): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<KeysBody>,
) -> ApiResult<Json<Value>> {
    core.connection(&id)?;
    let combos: Vec<String> = match body.keys {
        Value::String(s) => vec![s],
        Value::Array(list) => list.into_iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
        _ => return Err(bad("keys must be a string or an array of strings")),
    };
    let mut batches = Vec::new();
    for combo in &combos {
        batches.push(crate::keys::combo(combo).map_err(|e| bad(e.to_string()))?);
    }
    for (i, events) in batches.into_iter().enumerate() {
        if i > 0
            && let Some(ms) = body.delay_ms.filter(|ms| *ms > 0)
        {
            tokio::time::sleep(Duration::from_millis(ms.min(10_000))).await;
        }
        core.engine.input(&id, events);
    }
    Ok(ok())
}

#[derive(Deserialize)]
struct TypeBody {
    text: String,
}

async fn type_text(
    State(core): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<TypeBody>,
) -> ApiResult<Json<Value>> {
    core.connection(&id)?;
    core.engine.input(&id, crate::keys::text(&body.text));
    Ok(ok())
}

#[derive(Deserialize)]
struct MouseBody {
    action: String,
    #[serde(default)]
    x: Option<u16>,
    #[serde(default)]
    y: Option<u16>,
    #[serde(default)]
    button: Option<String>,
    #[serde(default)]
    delta: Option<i32>,
    #[serde(default)]
    horizontal: bool,
}

async fn mouse(
    State(core): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<MouseBody>,
) -> ApiResult<Json<Value>> {
    core.connection(&id)?;
    let button = match body.button.as_deref().unwrap_or("left") {
        "left" => 0,
        "middle" => 1,
        "right" => 2,
        "back" => 3,
        "forward" => 4,
        other => return Err(bad(format!("unknown button {other:?}"))),
    };
    let mut events = Vec::new();
    if let (Some(x), Some(y)) = (body.x, body.y) {
        events.push(InputEvent::Move { x, y });
    } else if body.action != "scroll" && body.action != "down" && body.action != "up" {
        return Err(bad("x and y are required"));
    }
    let press = |events: &mut Vec<InputEvent>| {
        events.push(InputEvent::Button { button, down: true });
        events.push(InputEvent::Button { button, down: false });
    };
    match body.action.as_str() {
        "move" => {}
        "click" => press(&mut events),
        "double_click" | "doubleclick" | "dblclick" => {
            press(&mut events);
            press(&mut events);
        }
        "down" => events.push(InputEvent::Button { button, down: true }),
        "up" => events.push(InputEvent::Button { button, down: false }),
        "scroll" => {
            let mut remaining = body.delta.unwrap_or(-120);
            while remaining != 0 {
                let step = remaining.clamp(-255, 255);
                events.push(InputEvent::Wheel {
                    delta: step as i16,
                    horizontal: body.horizontal,
                });
                remaining -= step;
            }
        }
        other => return Err(bad(format!("unknown action {other:?}"))),
    }
    core.engine.input(&id, events);
    Ok(ok())
}

#[derive(Deserialize)]
struct RawInputBody {
    events: Vec<InputEvent>,
}

async fn raw_input(
    State(core): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RawInputBody>,
) -> ApiResult<Json<Value>> {
    core.connection(&id)?;
    core.engine.input(&id, body.events);
    Ok(ok())
}

async fn ctrl_alt_del(State(core): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    core.connection(&id)?;
    core.engine.ctrl_alt_del(&id);
    Ok(ok())
}

// ── Companion: host stats and file sends ───────────────────────────────────

async fn all_stats(State(core): State<AppState>) -> Json<Value> {
    Json(serde_json::to_value(core.companions.stats()).unwrap_or_default())
}

async fn session_stats(State(core): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    core.connection(&id)?;
    let stats = core.companions.stats().remove(&id).ok_or_else(|| {
        ApiError(
            StatusCode::NOT_FOUND,
            "no host stats for this session (no companion attached, or host stats are off)".into(),
        )
    })?;
    Ok(Json(serde_json::to_value(stats).unwrap_or_default()))
}

#[derive(Deserialize)]
struct SendBody {
    ids: Vec<String>,
    paths: Vec<String>,
    #[serde(default)]
    folder: String,
}

async fn send_files(State(core): State<AppState>, Json(body): Json<SendBody>) -> ApiResult<Json<Value>> {
    if body.ids.is_empty() || body.paths.is_empty() {
        return Err(bad("ids and paths are required"));
    }
    let paths: Vec<std::path::PathBuf> = body.paths.into_iter().map(Into::into).collect();
    let (ids, folder) = (body.ids, body.folder);
    let transfers = tokio::task::spawn_blocking(move || core.send_files(&ids, &paths, &folder))
        .await
        .map_err(|e| bad(e.to_string()))?
        .map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(json!({ "transfers": transfers })))
}

/// Longest a command may be waited for: a day.
const RUN_TIMEOUT_MAX: u32 = 24 * 60 * 60;

fn default_run_timeout() -> u32 {
    300
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunBody {
    ids: Vec<String>,
    command: String,
    #[serde(default)]
    shell: rdp_companion_proto::Shell,
    #[serde(default)]
    visible: bool,
    #[serde(default = "yes")]
    wait: bool,
    #[serde(default = "default_run_timeout")]
    timeout_secs: u32,
}

async fn run_command(State(core): State<AppState>, Json(body): Json<RunBody>) -> ApiResult<Json<Value>> {
    if body.ids.is_empty() || body.command.trim().is_empty() {
        return Err(bad("ids and command are required"));
    }
    if !(1..=RUN_TIMEOUT_MAX).contains(&body.timeout_secs) {
        return Err(bad(format!("timeoutSecs must be 1 to {RUN_TIMEOUT_MAX}")));
    }
    let spec = rdp_companion_proto::RunSpec {
        command: body.command,
        shell: body.shell,
        visible: body.visible,
        wait: body.wait,
        timeout_secs: body.timeout_secs,
    };
    let results = core.run_command(body.ids, spec).await;
    Ok(Json(json!({ "results": results })))
}

async fn list_transfers(State(core): State<AppState>) -> Json<Value> {
    Json(serde_json::to_value(core.companions.transfers()).unwrap_or_default())
}

async fn cancel_transfer(State(core): State<AppState>, Path(id): Path<u64>) -> ApiResult<Json<Value>> {
    if core.companions.state(id).is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "transfer not found".into()));
    }
    core.companions.cancel(id);
    Ok(ok())
}

// ── Certificates ────────────────────────────────────────────────────────────

async fn list_certificates(State(core): State<AppState>) -> Json<Value> {
    Json(serde_json::to_value(core.certs.list()).unwrap_or_default())
}

#[derive(Deserialize)]
struct TrustBody {
    endpoint: String,
    fingerprint: String,
}

async fn trust_certificate(State(core): State<AppState>, Json(body): Json<TrustBody>) -> ApiResult<Json<Value>> {
    if body.endpoint.trim().is_empty() || body.fingerprint.trim().is_empty() {
        return Err(bad("endpoint and fingerprint are required"));
    }
    core.trust_certificate(&body.endpoint, &body.fingerprint);
    Ok(ok())
}

async fn forget_certificate(State(core): State<AppState>, Path(endpoint): Path<String>) -> ApiResult<Json<Value>> {
    if !core.certs.forget(&endpoint) {
        return Err(ApiError(StatusCode::NOT_FOUND, "certificate not found".into()));
    }
    Ok(ok())
}

// ── Settings ────────────────────────────────────────────────────────────────

async fn get_settings(State(core): State<AppState>) -> Json<Value> {
    Json(Value::Object(core.settings()))
}

async fn patch_settings(State(core): State<AppState>, Json(patch): Json<Map<String, Value>>) -> ApiResult<Json<Value>> {
    // Restarting the API from inside a request would cut the response: defer it.
    let restarts = patch.contains_key("apiPort") || patch.contains_key("apiEnabled");
    if restarts {
        let next = core.merged_settings(patch)?;
        let settings = next.clone();
        let core2 = Arc::clone(&core);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = core2.save_settings(settings, Origin::Api);
        });
        return Ok(Json(Value::Object(next)));
    }
    Ok(Json(Value::Object(core.patch_settings(patch, Origin::Api)?)))
}

// ── Import & export ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportQuery {
    groups: Option<String>,
    format: Option<String>,
}

fn export_options(q: ExportQuery, groups: Option<Vec<String>>) -> ApiResult<ExportOptions> {
    let v = json!({
        "groupIds": groups.or_else(|| q.groups.map(|g| g.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect())),
        "format": q.format.unwrap_or_else(|| "json".into()),
    });
    serde_json::from_value(v).map_err(|e| bad(e.to_string()))
}

fn export_response(content: String, _options: &ExportOptions) -> Response {
    let ct = if content.trim_start().starts_with('{') {
        "application/json"
    } else {
        "text/plain; charset=utf-8"
    };
    ([(header::CONTENT_TYPE, ct)], content).into_response()
}

async fn export_get(State(core): State<AppState>, Query(q): Query<ExportQuery>) -> ApiResult<Response> {
    let options = export_options(q, None)?;
    let content = core.export(&options)?;
    Ok(export_response(content, &options))
}

async fn export_post(State(core): State<AppState>, Json(options): Json<ExportOptions>) -> ApiResult<Response> {
    let content = core.export(&options)?;
    Ok(export_response(content, &options))
}

async fn export_group(
    State(core): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<ExportQuery>,
) -> ApiResult<Response> {
    core.group(&id)?;
    let options = export_options(q, Some(vec![id]))?;
    let content = core.export(&options)?;
    Ok(export_response(content, &options))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportBody {
    text: String,
    #[serde(default = "default_mode")]
    mode: String,
    #[serde(default = "default_target")]
    target: String,
}

fn default_mode() -> String {
    "merge".into()
}

fn default_target() -> String {
    "byName".into()
}

fn import_args(body: ImportBody) -> ApiResult<(ImportSource, ImportOptions)> {
    let options: ImportOptions =
        serde_json::from_value(json!({ "mode": body.mode, "target": body.target })).map_err(|e| bad(e.to_string()))?;
    Ok((ImportSource { text: body.text }, options))
}

async fn import_preview(State(core): State<AppState>, Json(body): Json<ImportBody>) -> ApiResult<Json<Value>> {
    let (source, options) = import_args(body)?;
    Ok(Json(
        serde_json::to_value(core.preview_import(&source, &options)?).unwrap_or_default(),
    ))
}

async fn import(State(core): State<AppState>, Json(body): Json<ImportBody>) -> ApiResult<Json<Value>> {
    let (source, options) = import_args(body)?;
    let r = core.apply_import(&source, &options, Origin::Api)?;
    Ok(Json(json!({
        "added": r.added,
        "updated": r.updated,
        "removed": r.removed,
        "groupsCreated": r.groups_created,
    })))
}

// ── UI ──────────────────────────────────────────────────────────────────────

async fn get_ui(State(core): State<AppState>) -> Json<Value> {
    Json(serde_json::to_value(core.ui_state()).unwrap_or_default())
}

#[derive(Deserialize)]
struct UiBody {
    select: Option<String>,
    view: Option<String>,
    fullscreen: Option<bool>,
    #[serde(default)]
    focus: bool,
}

async fn post_ui(State(core): State<AppState>, headers: HeaderMap, Json(body): Json<UiBody>) -> ApiResult<Json<Value>> {
    let _ = headers;
    if let Some(id) = body.select {
        core.connection(&id)?;
        core.ui_command(json!({ "type": "select", "id": id }));
    }
    if let Some(view) = body.view {
        if view != "session" && view != "grid" {
            return Err(bad("view must be \"session\" or \"grid\""));
        }
        core.ui_command(json!({ "type": "view", "view": view }));
    }
    if let Some(on) = body.fullscreen {
        core.ui_command(json!({ "type": "fullscreen", "on": on }));
    }
    if body.focus {
        core.ui_command(json!({ "type": "focus" }));
    }
    Ok(ok())
}
