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

/// 由内容哈希生成强 ETag。资源在编译期嵌入,同一份构建里内容不变、哈希也不变。
fn etag_of(f: &rust_embed::EmbeddedFile) -> String {
    let h = f.metadata.sha256_hash();
    let mut s = String::with_capacity(2 + h.len() * 2);
    s.push('"');
    for b in h.iter() {
        s.push_str(&format!("{b:02x}"));
    }
    s.push('"');
    s
}

/// 带缓存协商地返回一个嵌入资源。
///
/// 此前一个缓存头都不发。HTTP 规定这种情况下浏览器可以**启发式缓存** —— 自己决定存多久 ——
/// 于是发版后用户看到的仍是旧面板:实际发生过「后端已是 0.7.9,面板却显示 0.7.6」,
/// 而这类现象最难查,因为服务端每个接口查出来都是对的。
///
/// 用 `no-cache` + 强 ETag:`no-cache` 不是"不缓存",而是"可以存但每次必须回源验证";
/// 内容没变就回 304(不带 body),变了才传新内容。既不会再有陈旧面板,也不必每次全量重传。
fn serve_asset(f: rust_embed::EmbeddedFile, mime: &str, inm: Option<&str>) -> Response {
    let etag = etag_of(&f);
    // 客户端手里的副本仍然有效 → 304,不传 body。
    if inm.is_some_and(|v| v.split(',').any(|t| t.trim() == etag)) {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, etag.as_str()),
                (header::CACHE_CONTROL, "no-cache"),
            ],
        )
            .into_response();
    }
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::ETAG, etag.as_str()),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        Body::from(f.data.into_owned()),
    )
        .into_response()
}

fn serve<A: RustEmbed>(path: &str, inm: Option<&str>) -> Response {
    if path.contains("..") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let candidate = if path.is_empty() { "index.html" } else { path };
    match A::get(candidate) {
        Some(f) => {
            let mime = mime_guess::from_path(candidate).first_or_octet_stream();
            serve_asset(f, mime.as_ref(), inm)
        }
        // SPA 回退**只对路由生效**,资源路径找不到就老实 404。
        //
        // 此前是无条件回退:`assets/index-abc123.js` 这种明明是资源、明明不存在的路径,
        // 也会拿到 `200` + 一整篇 HTML。两个坏处:
        // 1. 浏览器把 HTML 当 JS 解析,报的是 `Unexpected token '<'`,而不是一眼可见的 404;
        // 2. **用 curl 核对某个构建产物是否上线时,不存在的文件也返回 200 —— 校验本身在骗人。**
        //
        // 判据用"文件名是否带扩展名":带扩展名的当资源(404),不带的当前端路由(回 index)。
        None if is_asset_path(candidate) => StatusCode::NOT_FOUND.into_response(),
        None => match A::get("index.html") {
            Some(f) => serve_asset(f, "text/html", inm),
            None => StatusCode::NOT_FOUND.into_response(),
        },
    }
}

/// 路径是否指向一个静态资源(而非前端路由):取最后一段,含 `.` 即认为带扩展名。
fn is_asset_path(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|last| last.contains('.'))
}

/// 从请求头取 `If-None-Match`(非 UTF-8 视为未提供)。
fn if_none_match(h: &axum::http::HeaderMap) -> Option<String> {
    h.get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
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
        .route(
            &base,
            get(|h: axum::http::HeaderMap| async move {
                serve::<A>("", if_none_match(&h).as_deref())
            }),
        )
        .route(
            &base_slash,
            get(|h: axum::http::HeaderMap| async move {
                serve::<A>("", if_none_match(&h).as_deref())
            }),
        )
        .route(
            &wildcard,
            get(
                |Path(file): Path<String>, h: axum::http::HeaderMap| async move {
                    serve::<A>(&file, if_none_match(&h).as_deref())
                },
            ),
        )
}

pub fn admin_router() -> Router {
    ui_router::<AdminAssets>("/admin")
}

pub fn user_router() -> Router {
    ui_router::<UserAssets>("/user")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::ServiceExt;

    /// 不存在的**资源**必须 404,不能回 200 + index.html。
    ///
    /// 回归:SPA 回退此前是无条件的,于是 `js/does-not-exist.js` 也拿到 `200` + 一整篇 HTML。
    /// 浏览器把 HTML 当 JS 解析,报的是 `Unexpected token '<'`;更坏的是,**用 curl 核对
    /// 某个构建产物是否上线时,不存在的文件也返回 200 —— 校验本身在骗人。**
    #[tokio::test]
    async fn missing_assets_404_while_routes_still_fall_back_to_index() {
        // 带扩展名 → 资源 → 404
        for p in [
            "/admin/js/does-not-exist.js",
            "/admin/assets/index-deadbeef.css",
        ] {
            let resp = admin_router()
                .oneshot(Request::builder().uri(p).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{p} 应 404");
        }
        // 不带扩展名 → 前端路由 → 回 index.html
        let resp = admin_router()
            .oneshot(
                Request::builder()
                    .uri("/admin/some/spa/route")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "前端路由仍应回 index");
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert!(
            String::from_utf8_lossy(&body).contains("<"),
            "回退内容应是 HTML"
        );
    }

    #[test]
    fn is_asset_path_only_matches_names_with_an_extension() {
        assert!(is_asset_path("js/app.js"));
        assert!(is_asset_path("assets/index-abc.css"));
        assert!(is_asset_path("favicon.ico"));
        assert!(!is_asset_path("dashboard"));
        assert!(!is_asset_path("some/spa/route"));
        // 目录名里有点、文件名没有 → 仍算路由
        assert!(!is_asset_path("v1.0/settings"));
    }

    /// 面板资源必须带缓存协商头。一个头都不发时,HTTP 允许浏览器**启发式缓存**——
    /// 自己决定存多久——发版后用户看到的仍是旧面板。实际发生过:后端已是 0.7.9,
    /// 面板却显示 0.7.6,而服务端每个接口查出来都是对的,这类现象最难查。
    #[tokio::test]
    async fn assets_carry_etag_and_must_revalidate() {
        let app = admin_router();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/js/sec-accounts.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache",
            "缺 no-cache 浏览器就会按自己的心情缓存,发版后仍显示旧面板"
        );
        assert!(
            resp.headers().get(header::ETAG).is_some(),
            "没有 ETag 就无从判断副本是否仍然有效,只能整份重传或用陈旧副本"
        );
    }

    /// 内容未变时回 304 且不带 body:既保证不陈旧,也不必每次全量重传。
    #[tokio::test]
    async fn unchanged_asset_answers_304_without_body() {
        let first = admin_router()
            .oneshot(
                Request::builder()
                    .uri("/admin/js/sec-accounts.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let etag = first.headers().get(header::ETAG).unwrap().clone();

        let second = admin_router()
            .oneshot(
                Request::builder()
                    .uri("/admin/js/sec-accounts.js")
                    .header(header::IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
        let body = to_bytes(second.into_body(), usize::MAX).await.unwrap();
        assert!(body.is_empty(), "304 不得带 body");
    }

    /// ETag 取自内容哈希:内容不同的两个资源不得共用同一个 ETag,
    /// 否则浏览器会拿着 A 的副本当 B 用。
    #[tokio::test]
    async fn different_assets_have_different_etags() {
        async fn etag_for(uri: &str) -> String {
            let r = admin_router()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            r.headers()
                .get(header::ETAG)
                .unwrap()
                .to_str()
                .unwrap()
                .to_string()
        }
        let a = etag_for("/admin/js/sec-accounts.js").await;
        let b = etag_for("/admin/js/sec-dashboard.js").await;
        assert_ne!(a, b);
    }
}
