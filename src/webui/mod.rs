use axum::body::Body;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Router, routing::get};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "admin-ui-v2"]
struct AdminAssets;

#[derive(RustEmbed)]
#[folder = "user-ui/dist"]
struct UserAssets;

fn serve<A: RustEmbed>(path: &str) -> Response {
    if path.contains("..") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let candidate = if path.is_empty() { "index.html" } else { path };
    match A::get(candidate) {
        Some(f) => {
            let mime = mime_guess::from_path(candidate).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], f.data.into_owned()).into_response()
        }
        // SPA 回退:非静态路径回 index.html
        None => match A::get("index.html") {
            Some(f) => (
                [(header::CONTENT_TYPE, "text/html")],
                Body::from(f.data.into_owned()),
            )
                .into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
    }
}

/// 以绝对路径挂载一套 UI:同时注册裸路径(`/admin`)、带斜杠(`/admin/`)与
/// 通配子路径(`/admin/{*file}`),三者都落入同一个 `serve::<A>()`。
/// 之所以不用 `.nest()`:axum 0.8 的 nest 对挂载路径末尾斜杠敏感
/// (`nest("/admin", ..)` 不匹配 `/admin/`,反之亦然),导致 `GET /admin`
/// (无斜杠)404。改为在 `prefix` 上直接注册三条绝对路径可同时覆盖两种写法。
fn ui_router<A: RustEmbed>(prefix: &str) -> Router {
    let base = prefix.to_string();
    let base_slash = format!("{prefix}/");
    let wildcard = format!("{prefix}/{{*file}}");
    Router::new()
        .route(&base, get(|| async { serve::<A>("") }))
        .route(&base_slash, get(|| async { serve::<A>("") }))
        .route(
            &wildcard,
            get(|Path(file): Path<String>| async move { serve::<A>(&file) }),
        )
}

pub fn admin_router() -> Router {
    ui_router::<AdminAssets>("/admin")
}

pub fn user_router() -> Router {
    ui_router::<UserAssets>("/user")
}
