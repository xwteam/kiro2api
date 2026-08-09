pub mod auth;

use crate::config::Config;
use crate::kiro::credential;
use crate::kiro::pool::{LbMode, Pool};
use crate::protocol::anthropic::handler::{MessagesState, messages_router};
use crate::webui;
use axum::{Json, Router, routing::get};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// 进程启动时刻,首次 `build_router*` 时捕获一次(`OnceLock` 幂等)。
/// admin `system-info` 端点据此算 `uptimeSecs`(已运行秒数)。
/// 用 `Instant`(单调钟)避免系统时钟回拨影响运行时长。
static SERVER_START: OnceLock<Instant> = OnceLock::new();

/// 进程已运行秒数;若从未初始化(理论上不会,build_router 必先跑)回落 0。
pub fn server_uptime_secs() -> u64 {
    SERVER_START
        .get()
        .map(|start| start.elapsed().as_secs())
        .unwrap_or(0)
}

// server::auth 中的鉴权原语(verify/extract_key)与 require_api_key 中间件
// 统一接入下方三条协议路由(messages/openai/gemini);/health、/v1/ping 与
// /admin、/user 挂载的公开 UI 静态资源保持开放,不受鉴权闸影响。
pub fn build_router(cfg: Arc<Config>) -> Router {
    build_router_with_logs(cfg, None)
}

/// 与 [`build_router`] 相同,但注入实时日志捕获器句柄(Phase 6)。
/// `log_capture` 为 `Some` 时,admin 日志端点(stream/snapshot/download)可读历史 + 订阅广播;
/// `None` 时这些端点返回 503。既有测试仍走 [`build_router`](=None),行为不变。
pub fn build_router_with_logs(
    cfg: Arc<Config>,
    log_capture: Option<Arc<crate::logcap::LogCapture>>,
) -> Router {
    build_router_with_handles(cfg, log_capture).0
}

/// 与 [`build_router_with_logs`] 完全相同的路由,但额外交出内部构造的 `StatsManager` 句柄。
/// `serve()` 据此在收到停机信号后触发统计最终刷盘——统计层由 `build_router*` 内部构造,
/// 外部拿不到句柄就无法在退出前落盘。
///
/// 保留此签名只为兼容既有调用方;需要**完整**的停机刷盘(统计 + API-KEY)请用
/// [`build_router_with_persist_handles`],它交出的 [`PersistHandles`] 覆盖全部去抖存储。
pub fn build_router_with_handles(
    cfg: Arc<Config>,
    log_capture: Option<Arc<crate::logcap::LogCapture>>,
) -> (Router, Arc<crate::stats::StatsManager>) {
    let (router, handles) = build_router_with_persist_handles(cfg, log_capture);
    (router, handles.stats)
}

/// 停机 / 管理面重启前必须**显式**落盘的持久化句柄集合。
///
/// 这些存储统一走 `stats::persist` 的「改内存 + 置脏 + 约 5s 去抖批量落盘」模型:置脏通知本身
/// **不写盘**,只有下一拍定时器才写(见 `stats::persist::spawn_flush_loop`)。故进程退出
/// (SIGTERM/Ctrl-C,或管理面重启端点的 `std::process::exit`——它连析构都不跑)时若不显式刷一次,
/// 落在两拍之间的写就只在内存里,重启即回滚。
pub struct PersistHandles {
    /// 用量 / 计费统计(含失败 401/403 与限流 429 事件日志):丢一拍 = 少算这段时间的钱、
    /// 且刚出的报错现场在失败/限流弹窗里查无此事。
    pub stats: Arc<crate::stats::StatsManager>,
    /// 余额缓存:丢一拍最要命的是 `invalidate` —— 删账号时清掉的条目会随旧文件复活,
    /// 复用同一 id 的新账号在 TTL 内(最长 5 分钟)显示已删账号的余额与订阅档位。
    pub balance: Arc<crate::balance::BalanceCache>,
    /// API-KEY 存储:丢一拍的后果更重——**刚吊销的 key 会随旧文件复活**(重启后照样鉴权通过),
    /// 刚建出并交给用户的 key 则凭空消失(用户拿着新 key 直接 401)。
    pub api_keys: Arc<crate::apikey::ApiKeyStore>,
    /// `api_keys.json` 路径:停机落盘前据此判磁盘上那份是否还解析得动(见 [`PersistHandles::flush_before_exit`])。
    pub api_keys_path: PathBuf,
}

impl PersistHandles {
    /// 退出前的最终刷盘:把去抖窗口(约 5s)内尚未落盘的写立刻同步落下去。
    ///
    /// 各项之间无依赖;任一失败只记 error、绝不阻断停机(停机路径宁可少落一项,也不能卡住不退)。
    pub async fn flush_before_exit(&self) {
        // 用量/计费 + 失败/限流事件日志:去抖窗口内的记录只在内存里,不刷就少算钱、
        // 且最近这几秒的 401/403、429 现场直接消失(见 StatsManager::flush_now)。
        self.stats.flush_now().await;
        // 余额缓存:删账号时的 invalidate 同样只置脏,不刷则残留条目随旧文件复活,
        // 复用同一 id 的新账号会显示上一位主人的余额/订阅档位(最长 5 分钟)。
        self.balance.flush_now().await;
        // API-KEY:create/update/delete/disable 都只 `mark_dirty()`,真正写盘要等下一拍定时器。
        if !self.api_keys_overwrite_is_safe() {
            tracing::error!(
                path = %self.api_keys_path.display(),
                "磁盘上的 api_keys.json 存在却解析不动,且内存里一把 key 都没有:跳过停机落盘,以免用空存储把这份仍可人工挽救的文件覆盖掉"
            );
            return;
        }
        let store = self.api_keys.clone();
        // `save_now` 是同步写 + fsync(可长达数十毫秒),不能占着 async 工作线程做。
        match tokio::task::spawn_blocking(move || store.save_now()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!(
                error = %e,
                "API-KEY 停机落盘失败:最近约 5s 内的密钥增删将在重启后回滚(已吊销的 key 可能复活)"
            ),
            Err(e) => tracing::error!(
                error = %e,
                "API-KEY 停机落盘任务执行失败:最近约 5s 内的密钥增删将在重启后回滚(已吊销的 key 可能复活)"
            ),
        }
    }

    /// 停机落盘的自保阀:仅当「内存里一把 key 都没有」**且**「磁盘上的 api_keys.json 存在却
    /// 解析不动」时返回 false。
    ///
    /// 这一组合说明启动时那份文件根本没被读进内存(`ApiKeyStore::load` 对解析失败是静默按空继续的),
    /// 此刻用空存储原子覆写,等于把仅存的、也许还能人工挽救的密钥文件抹掉——正是本轮要堵的那类坑。
    /// 其余情形一律照写:内存里有 key 时,那份内存态才是最新真源。
    fn api_keys_overwrite_is_safe(&self) -> bool {
        if !self.api_keys.is_empty() {
            return true;
        }
        match std::fs::read(&self.api_keys_path) {
            // 文件不存在/读不到:写下去覆盖不掉任何内容。
            Err(_) => true,
            // 空文件(或只有空白):同样没有可挽救的内容。
            Ok(raw) if raw.iter().all(|b| b.is_ascii_whitespace()) => true,
            // 能解析成 JSON = 启动时的空存储是文件真实内容(或无关形状),覆写无损失。
            Ok(raw) => serde_json::from_slice::<serde_json::Value>(&raw).is_ok(),
        }
    }
}

/// 四条协议端点(Anthropic/OpenAI/Responses/Gemini)的请求体上限。
///
/// 取 32MB 与 admin 组同档:多模态请求把图片 base64 内联进 JSON,4/3 的膨胀让 axum 默认的
/// 2MB 只够约 1.5MB 的原图;而上游本身允许的单请求远大于此(Gemini 内联约 20MB、
/// Anthropic PDF 32MB)。上限仍要有:body 由 `Json<T>` 全量缓冲进内存,无上限等于把内存
/// 交给调用方决定。
const PROTOCOL_BODY_LIMIT_BYTES: usize = 32 * 1024 * 1024;

/// 与 [`build_router_with_handles`] 相同的路由,但交出全部需要在退出前落盘的句柄。
pub fn build_router_with_persist_handles(
    cfg: Arc<Config>,
    log_capture: Option<Arc<crate::logcap::LogCapture>>,
) -> (Router, PersistHandles) {
    // 捕获进程启动时刻(幂等):admin system-info 端点据此算 uptimeSecs。
    // 首次调用置入,后续调用(如测试重复建 router)不覆盖,保留最早时刻。
    let _ = SERVER_START.get_or_init(Instant::now);
    // 伪装身份的静态配置灌进进程级默认值:令牌刷新那条链路拿不到 Config,靠它取版本号。
    crate::kiro::provider::init_impersonation_defaults(&cfg);
    // 从配置路径加载凭据;文件不存在回落空池,/v1/messages 遇空池返回 503
    // (不影响 health/webui 路由,默认配置测试保持全绿)。
    // **解析失败不再静默当空池**:先原样备份、再逐条抢救、并大声报错,见 load_credentials_resilient。
    let mut creds = load_credentials_resilient(&cfg.credentials_path);
    // 启动期把账号数写进日志:池为 0 时运营者据此一眼分清「文件没放/里面没账号」与
    // 「文件坏了被抢救成 0」(后者上面已有逐条 error 明细),不必对着面板的 total:0 猜。
    if creds.is_empty() {
        tracing::warn!(
            path = %cfg.credentials_path,
            "账号池为空:四条协议端点将一律返回 503(若刚才有凭据解析报错,请先按其提示修文件,别在管理面重新加账号——写入会用空池整份覆写)"
        );
    } else {
        tracing::info!(path = %cfg.credentials_path, accounts = creds.len(), "已载入账号凭据");
    }
    // 账号 id 高水位:读上次落盘值,使"已删除账号的编号"重启后也不被复用。
    // 旧安装没有该旁挂文件 → `None` → 传 0,由 `with_next_id` 平滑退化为 `max(现有 id)+1`
    // 的原语义。不接这一步的话高水位只写不读,重启即退回复用编号(旧令牌覆盖新账号凭据)。
    let persisted_next_id = credential::load_next_id(&cfg.credentials_path).unwrap_or(0);
    // 机器 ID 一次性冻结并落盘。
    //
    // 此前 machineId 是**每次请求现算**的 `sha256("KotlinNativeAPI/" + 当前 refreshToken)`,
    // 从不落盘。而刷新响应会回带新的 refreshToken(我们照收),于是同一个账号**每刷新一次
    // 就换一台机器**——真实设备绝不会这样,这是最刺眼的一种身份漂移。
    //
    // 冻结只补给"还没有 machineId"的凭据,且仅在配置未全局指定 machineId 时进行
    //(配置级本就是让整池共用一个身份的开关,冻结会把它固化进每条凭据、日后改不动)。
    let frozen = credential::freeze_machine_ids(&mut creds, cfg.machine_id.as_deref());
    if frozen > 0 {
        tracing::info!(
            count = frozen,
            "已为无 machineId 的凭据一次性派生并冻结机器 ID(此后不再随 refreshToken 轮换而漂移)"
        );
        if let Err(e) = credential::save(&cfg.credentials_path, &creds) {
            tracing::warn!(error = ?e, "冻结的 machineId 落盘失败:本次进程内仍稳定,但重启后会重算");
        }
    }
    let mut pool_inner = Pool::with_next_id(creds, LbMode::Priority, persisted_next_id);
    // RPM 限流与负载均衡模式均来自配置(默认无限 RPM/Priority,保持既有行为)。
    pool_inner.set_max_rpm(cfg.max_rpm_per_credential);
    if cfg.load_balancing_mode.eq_ignore_ascii_case("balanced") {
        pool_inner.set_mode(LbMode::Balanced);
    }
    let pool = Arc::new(Mutex::new(pool_inner));
    // 统计持久化层:数据目录由 credentials_path 的父目录推断。
    // relay 数据面记录用量/失败/限流,admin 只读查询,二者共用同一 Arc<StatsManager>。
    let stats = crate::stats::StatsManager::load_from_credentials_path(&cfg.credentials_path);
    // API-KEY 存储:数据目录同 stats,由 credentials_path 父目录推断 api_keys.json。
    // auth 闸 / relay 归属 / admin / user 共用同一 Arc<ApiKeyStore>(后续阶段接入消费方)。
    // 路径单独留一份:停机落盘的自保阀要按它判磁盘文件是否还解析得动(store 自己不外露路径)。
    let api_keys_path = crate::apikey::api_keys_path_from(&cfg.credentials_path);
    let api_keys = crate::apikey::ApiKeyStore::load(api_keys_path.clone());
    // 余额缓存(Phase 4):与 stats 同数据目录,载入 kiro_balance_cache.json 并启动去抖刷盘。
    // admin 余额端点读缓存(5 分钟 TTL)/回填;relay 数据面不消费。
    let balance = crate::balance::BalanceCache::load_from_credentials_path(&cfg.credentials_path);
    // 运行期可变配置:仅承载可改写字段(auth key / RPM / 负载均衡模式),与不可变 cfg 分离。
    // auth 闸(协议 + admin)据此实时读期望 key(轮换即时生效);admin 设置端点写入并原子落盘。
    let runtime_cfg = crate::config::shared_runtime_config(&cfg);
    let messages_state = MessagesState {
        pool,
        // 共享出站客户端:此 state.client 既服务中转数据面(relay/provider::call 的
        // SSE 长流)又服务控制面(登录/余额/模型/刷新的一问一答)。因数据面需容忍长流,
        // 不能加整请求超时(会误杀),故统一取流式客户端——connect_timeout 界定建连、
        // read_timeout 界定读取停顿(每读一段即重置),两面消费者都得到有界超时护栏,
        // 修复上游卡死时连接无限期挂起(HTTP TIMEOUT 发现 #5)。
        client: crate::http::streaming(),
        // 控制面(一问一答)客户端:令牌刷新 / 余额 / 模型清单等短小请求经此发出。
        // 由 http::unary() 构造——connect_timeout + 整请求 timeout 硬顶,使控制面上游
        // 卡死时在上限内失败,不会无限期挂起(HTTP TIMEOUT 发现 #6)。与数据面
        // client(流式、无整请求超时)分离,因数据面需容忍 SSE 长流、加硬顶会误杀。
        control_client: crate::http::unary(),
        cfg: cfg.clone(),
        runtime_cfg: runtime_cfg.clone(),
        endpoint_override: None, // 生产走端点回退
        stats: stats.clone(),
        api_keys: api_keys.clone(),
        balance: balance.clone(),
        // 动态模型清单缓存(FIX 1):进程内新建空缓存,admin /models 按需惰性回填/并集展示。
        models_cache: crate::models_cache::ModelsCache::new(),
        // Phase 3 交互式登录会话(内存中转态,~600s TTL)。admin 登录端点独占,
        // 每次 build_router 起一份新空存储(会话短命、无需持久化)。
        builderid_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
        iam_sso_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
        // Phase 6 实时日志:注入捕获器句柄(main 据 log_capacity 建好后传入),
        // admin 日志端点据此读历史 + 订阅广播;None 时端点 503。
        log_capture,
        // 令牌刷新上下文:per-credential 单飞锁 + credentials.json 路径 + 落盘锁。
        // 三条协议路由 + admin balance/models 共用同一份,使并发刷新同一凭据单飞、
        // 刷新后原子落盘,修复级联 401(Bug A)与重启后旧 token 401(Bug B)。
        refresh_ctx: crate::kiro::ensure_fresh::RefreshCtx::new(cfg.credentials_path.clone()),
    };

    // 活跃账号的令牌提前续期。只对近期真被选中过的账号生效 —— 全池定时续期会给每个账号
    // 造出一条永不停歇的后台心跳,而本项目的账号已被上游以 security precaution 封过。
    // 详见 `kiro::keepalive` 模块头。
    crate::kiro::keepalive::spawn(
        messages_state.pool.clone(),
        messages_state.control_client.clone(),
        messages_state.refresh_ctx.clone(),
    );

    // 三条协议路由(/v1/messages、OpenAI 兼容、Gemini 兼容)共用同一 MessagesState,
    // 先 merge 成一组再统一 .layer() 鉴权闸——axum 的 .layer() 只作用于其之前
    // 已注册到该 Router 的路由,故必须在合并进顶层 Router 之前单独 layer,
    // 否则 health/ping/webui 也会被一并挡住。
    let protocol = messages_router(messages_state.clone())
        // OpenAI 兼容路由(/v1/chat/completions、/v1/models 等)复用同一 MessagesState,
        // 内部通过 relay_core 走同一条中转内核。
        .merge(crate::protocol::openai::openai_router(
            messages_state.clone(),
        ))
        // Gemini 兼容路由(/v1beta/models/{model}:generateContent 等)同样复用 MessagesState,
        // 走同一条 relay_core 中转内核;与 OpenAI 的 /v1/... 路径无重叠。
        .merge(crate::protocol::gemini::gemini_router(
            messages_state.clone(),
        ))
        // OpenAI Responses 兼容路由(/v1/responses 等)同样复用 MessagesState,
        // 走同一条 relay_core 中转内核;与既有路径无重叠。
        .merge(crate::protocol::responses::handler::responses_router(
            messages_state.clone(),
        ))
        .layer(axum::middleware::from_fn_with_state(
            // 协议闸:全局 key 或有效 store key 二选一;store key 命中时把解析出的
            // api-key id 塞进请求扩展供 relay 归属,并按消费上限(查 stats)裁决。
            // 期望全局 key 从 runtime_cfg 实时读取,使 PUT /config/auth-keys 轮换即时生效。
            auth::AuthState::protocol(runtime_cfg.clone(), api_keys.clone(), stats.clone()),
            auth::require_api_key,
        ))
        // 多模态请求体上限:必须显式抬高,否则走 axum 的默认 2MB(`DEFAULT_LIMIT`)。
        // 四条协议端点都用 `Json<T>` 提取器,而多模态请求把图片以 base64 内联在 body 里
        // (Gemini `inlineData.data` / OpenAI `image_url` 的 data: URI / Anthropic image block),
        // base64 把原图放大 4/3 —— 2MB 的上限意味着约 1.5MB 的 JPEG/PNG 就在 handler 之前
        // 被拦下,客户端只看到一句"发图必失败",而三家上游本身允许的单请求都远大于此
        // (Gemini 内联约 20MB、Anthropic PDF 32MB)。故与 admin 组取同一档 32MB。
        .layer(axum::extract::DefaultBodyLimit::max(
            PROTOCOL_BODY_LIMIT_BYTES,
        ));

    // 用户面 REST API 单独一组:**不叠加任何鉴权 .layer()**——这些端点由调用方随
    // 请求携带的 API-KEY 自鉴权(handler 内经 Arc<ApiKeyStore> 校验),故与 admin/协议
    // 两组的鉴权闸互不相干。复用同一 MessagesState(共享 api_keys + stats)。
    let user = crate::user::user_api_router(messages_state.clone());

    // Admin REST API 单独一组:鉴权用 admin_api_key,未配置则回退主 api_key。
    // 与协议组各自独立 layer,互不影响——协议组用 api_key,admin 组用 admin_api_key 优先。
    // Admin 复用 messages_state:其已携带同一个 `Arc<StatsManager>`(上方 line 28-34 构造),
    // 故 admin 只读查询与 relay 记录写入共享同一内存存储,无需再建第二份。
    let admin = crate::admin::admin_api_router(messages_state)
        .layer(axum::middleware::from_fn_with_state(
            // admin 闸:admin_api_key(非空)否则回退主 api_key 的全局 key 校验,不查消费上限。
            // 期望 key 从 runtime_cfg 实时读取,使 PUT /config/auth-keys 改 admin 密码后
            // 无需重启即时生效。store 一并传入但 admin 闸**不据其判定"是否已配置鉴权"**:
            // store key 是用户级凭据、在 admin 闸永不放行,若让它把闸判成已配置,首次部署
            // 建出第一把 key 就会自锁(能开锁的管理员凭据一把都没有)。故 admin 闸只认
            // 管理员级凭据(admin_api_key,缺省回退 api_key)。
            auth::AuthState::admin(runtime_cfg.clone(), api_keys.clone()),
            auth::require_api_key,
        ))
        // 批量导入等 admin 端点可能提交大 JSON(多账号凭据集,单个 token 就上千字符)。
        // 把请求体上限从 axum 默认 2MB 提到 32MB,避免大导入被 413 拦下(体现为前端"发生错误");
        // admin 已鉴权,放宽仅限受信管理面。
        .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024));

    let router = Router::new()
        .route("/health", get(health))
        .route("/v1/ping", get(ping))
        .with_state(cfg)
        // 不用 .nest():axum 0.8 的 nest 对挂载路径末尾斜杠敏感,
        // "/admin" 与 "/admin/" 只能二选一匹配,会导致另一种写法 404。
        // 改为 .merge() 绝对路径路由(webui::admin_router()/user_router()
        // 内部各自注册裸路径、带斜杠、通配子路径三条),使 /admin、/admin/、
        // /admin/{*file} 都能落到 UI 处理器。
        .merge(webui::admin_router())
        .merge(webui::user_router())
        .merge(protocol)
        .merge(admin)
        .merge(user);
    (
        router,
        PersistHandles {
            stats,
            balance,
            api_keys,
            api_keys_path,
        },
    )
}

/// 凭据文件逐条抢救时,最多逐条打印多少条坏账号的明细(其余只计入总数)。
/// 上限的理由:生产 credentials.json 有约千条账号,整体格式走样时不能让启动日志被刷爆
/// (日志同时进实时日志面板的环形缓冲,刷爆等于把别的线索一起冲掉)。
const MAX_BAD_CREDENTIAL_LOGS: usize = 20;

/// 载入账号池凭据——**解析失败绝不静默当空池**。
///
/// 为什么不能再写 `credential::load(..).unwrap_or_default()`:生产的 credentials.json 是运营者
/// 手工放置 / drop-in 的大数组(约 1000 个账号、数 MB),而 `load` 是整份文件 all-or-nothing 的
/// serde —— 任何**一条**账号写坏(`expiresAt` 不是 RFC3339、缺 `accessToken`/`refreshToken`、
/// `authMethod` 不是 social/idc、数组里多一个逗号)都会让整份文件解析失败。旧写法把 `Err` 丢掉:
/// 进程带着 0 账号池启动、日志里一个字都没有,运营者只看到 relay 全 503、面板 `total: 0`,于是
/// 「那就重新加一个账号」——admin 侧的原子落盘会用内存里的空池整份覆写 credentials.json,
/// 约千个账号的 refreshToken 当场蒸发且不可逆。
///
/// 故这里做三件事(与 `stats::usage` 对损坏文件的处理同规格):
/// 1. **原样备份**:把当前文件复制成 `{path}.corrupt.{unix秒}.{序号}`,此后任何原子写都覆不掉它;
/// 2. **逐条抢救**:文件本体仍是 JSON 数组时按条反序列化,坏条目跳过、好账号照常入池
///    (1 条坏账号不再拖垮另外 999 个,中转继续可用);
/// 3. **大声报错**:error 日志给出坏条目位置、备份路径与后果说明,让运营者知道该修文件而非重加账号。
fn load_credentials_resilient(path: &str) -> Vec<credential::Credential> {
    let err = match credential::load(path) {
        // 正常路径(含文件不存在 → 空池)原样返回,不做任何额外 IO。
        Ok(creds) => return creds,
        Err(e) => e,
    };
    // 走到这里说明文件存在但读不动/解析不了。原始字节备份与逐条抢救都要用,只读一次。
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(read_err) => {
            tracing::error!(
                path = %path,
                error = %err,
                read_error = %read_err,
                "credentials.json 读取失败,账号池按空启动;修好之前请勿在管理面增删账号——任何写入都会用空池整份覆写该文件"
            );
            return Vec::new();
        }
    };
    let backup = backup_corrupt_credentials(path, &raw);
    let (creds, total, skipped) = salvage_credentials(&raw);
    tracing::error!(
        path = %path,
        error = %err,
        entries_total = total,
        salvaged = creds.len(),
        skipped = skipped,
        backup = backup
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<备份失败>".to_string()),
        "credentials.json 解析失败:已原样备份并逐条抢救可用账号;请按上面的逐条报错修好坏条目后重启,期间在管理面增删账号会用当前(可能不完整的)池覆写该文件"
    );
    creds
}

/// 逐条抢救:文件本体仍是 JSON 数组时按条反序列化,坏条目跳过并报错。
/// 返回 `(可用凭据, 条目总数, 跳过条数)`;整份文件连数组都解析不出(语法错/被截断)时全空。
fn salvage_credentials(raw: &[u8]) -> (Vec<credential::Credential>, usize, usize) {
    let entries: Vec<serde_json::Value> = match serde_json::from_slice(raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                error = %e,
                "credentials.json 连 JSON 数组都解析不出(语法错误/文件被截断),无法逐条抢救;原文件已备份,请人工修复"
            );
            return (Vec::new(), 0, 0);
        }
    };
    let total = entries.len();
    let mut out = Vec::with_capacity(total);
    let mut skipped = 0usize;
    for (index, entry) in entries.into_iter().enumerate() {
        // 定位信息只取 id(和数组下标),**绝不打印 token/邮箱**——这些日志会进实时日志面板与文件。
        let id_hint = entry
            .get("id")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "<无 id>".to_string());
        match serde_json::from_value::<credential::Credential>(entry) {
            Ok(c) => out.push(c),
            Err(e) => {
                skipped += 1;
                if skipped <= MAX_BAD_CREDENTIAL_LOGS {
                    tracing::error!(
                        index = index,
                        id = %id_hint,
                        error = %e,
                        "该账号条目解析失败,已跳过(其余账号照常入池)"
                    );
                }
            }
        }
    }
    (out, total, skipped)
}

/// 把当前(解析不了的)凭据文件**原样复制**一份到 `{path}.corrupt.{unix秒}.{序号}`。
///
/// 为什么是复制而不是像 `stats::usage` 那样改名:credentials.json 是账号池的唯一真源。改名走人,
/// 下次重启连「逐条抢救」都没得救(文件已不在原处),且管理面第一次落盘就会把仅剩的内存池写成
/// 新文件。复制则两头都保住:原文件留在原地供人工修复,备份档任凭后续原子写也覆不掉。
///
/// 幂等:同一份坏内容已经备份过(存在字节完全相同的 `.corrupt.*`)就不再重复备份,
/// 否则容器反复重启会在数据盘上堆出一摞数 MB 的副本。
fn backup_corrupt_credentials(path: &str, raw: &[u8]) -> Option<PathBuf> {
    if let Some(existing) = existing_identical_backup(path, raw) {
        return Some(existing);
    }
    let backup = corrupt_backup_path(Path::new(path));
    let res = (|| -> std::io::Result<()> {
        // 文件里是全部账号的明文 access/refresh token:与 `credential::save` 同规,按 0600 建,
        // 不能用默认权限(0644)把凭据副本变成全局可读。`create_new` 保证绝不顶掉已有备份。
        #[cfg(unix)]
        let f = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&backup)?
        };
        #[cfg(not(unix))]
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)?;
        {
            use std::io::Write;
            let mut w = std::io::BufWriter::new(&f);
            w.write_all(raw)?;
            w.flush()?;
        }
        f.sync_all() // fsync:备份的意义就在于机器随后掉电也还在
    })();
    match res {
        Ok(()) => Some(backup),
        Err(e) => {
            tracing::error!(
                path = %path,
                backup = %backup.display(),
                error = %e,
                "损坏的 credentials.json 备份失败:原文件仍在原处,但管理面的下一次写入会覆盖它,请先手工另存一份"
            );
            None
        }
    }
}

/// 损坏文件的备份路径:`{path}.corrupt.{unix秒}.{序号}`,取第一个尚不存在的候选
/// (与 `stats::usage::corrupt_backup_path` 同规格),保证同秒内多次备份互不覆盖。
fn corrupt_backup_path(path: &Path) -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let with_suffix = |n: u32| {
        let mut s = path.as_os_str().to_os_string();
        s.push(format!(".corrupt.{secs}.{n}"));
        PathBuf::from(s)
    };
    for n in 0..100 {
        let cand = with_suffix(n);
        if !cand.exists() {
            return cand;
        }
    }
    with_suffix(99)
}

/// 找同目录下内容与 `raw` 逐字节相同的既有 `{basename}.corrupt.*` 备份(有则本次不必再备份)。
/// 先比长度再比内容:长度不同直接跳过,避免为每个候选都读一遍数 MB 的文件。
fn existing_identical_backup(path: &str, raw: &[u8]) -> Option<PathBuf> {
    let p = Path::new(path);
    let (dir, file_name) = (p.parent()?, p.file_name()?.to_string_lossy().into_owned());
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let prefix = format!("{file_name}.corrupt.");
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let same_len = entry
            .metadata()
            .map(|m| m.len() == raw.len() as u64)
            .unwrap_or(false);
        if same_len
            && std::fs::read(entry.path())
                .map(|b| b == raw)
                .unwrap_or(false)
        {
            return Some(entry.path());
        }
    }
    None
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "kiro2api",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn ping() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "pong": true }))
}

/// 启动期鉴权配置体检:返回需要提示的告警文案(空 = 无隐患)。
///
/// 抽成纯函数以便单测钉住各场景的覆盖面;`serve()` 只负责逐条 warn。
/// 判据只看两个全局 key。注意两道闸的口径不同:**协议闸**认 store key(建了 API-KEY 即收口,
/// 故协议那条文案带"未创建任何 API-KEY 前"限定);**admin 闸**只认管理员级凭据,建 API-KEY
/// 不会收口,故管理面那条**不得**带该限定,否则低估暴露面。
fn auth_startup_warnings(cfg: &Config) -> Vec<String> {
    let api_key_set = !cfg.api_key.as_deref().unwrap_or("").trim().is_empty();
    let admin_key_set = !cfg.admin_api_key.as_deref().unwrap_or("").trim().is_empty();
    let mut warnings = Vec::new();
    if !api_key_set {
        warnings.push(
            "未设置 api_key:在未创建任何 API-KEY 前,四条协议端点(Anthropic/OpenAI/Responses/Gemini)开放访问"
                .to_string(),
        );
    }
    if !admin_key_set {
        if api_key_set {
            // 只有主 key:管理闸回退用它,数据面 key 持有者即管理员。
            warnings.push(
                "未设置 admin_api_key:/admin 管理面回退用主 api_key 校验,数据面 key 持有者同时具备完整管理员权限;建议设置环境变量 ADMIN_API_KEY=<独立强口令> 与数据面分离"
                    .to_string(),
            );
        } else {
            // 两个 key 都没有:管理面完全开放——可读写账号凭据、密钥与统计,危害远大于协议端点。
            // 措辞不能带"在未创建任何 API-KEY 前"这类限定:admin 闸只认管理员级凭据,建 API-KEY
            // **不会**自动收口(那样会把首次部署的运营者锁在门外,见 admin_stats_open_when_only_store_keys_exist)。
            warnings.push(
                "未设置 admin_api_key/api_key:/admin 管理面当前完全开放(任何人可读写账号凭据、API-KEY 与统计),创建 API-KEY 也不会自动收口;请在管理面设置管理员密钥,或设置环境变量 ADMIN_API_KEY=<强口令> 后重启"
                    .to_string(),
            );
        }
    }
    warnings
}

/// 停机信号:SIGTERM(`docker stop`/编排器回收)或 SIGINT(Ctrl-C)任一到达即返回。
/// 非 unix 平台只等 Ctrl-C。
///
/// 容器里本进程常是 PID 1:不处理 SIGTERM 就会一直等到宽限期结束被 SIGKILL,
/// 在途 SSE 长流被硬掐、未刷盘的统计丢失。
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                let _ = sig.recv().await;
            }
            Err(e) => {
                // 注册失败(极罕见)退化为只响应 Ctrl-C,不让停机路径 panic。
                tracing::warn!(error = %e, "SIGTERM 处理器注册失败,停机仅响应 Ctrl-C");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("收到停机信号,停止接受新连接,等待在途请求结束");
}

/// 停机排空窗口:收到 SIGTERM/SIGINT 后最多再给在途请求这么久收尾,到点无论是否
/// 还有连接挂着都继续走最终刷盘并退出。
///
/// 为什么必须有上界:admin 实时日志 SSE(`/api/admin/logs/stream`)是**无限流**——它只在
/// 广播发送端释放时才结束,而发送端由路由 state 持有,服务器活着就不会释放。axum 的
/// `with_graceful_shutdown` 要等所有在途响应结束才返回,故只要有一个管理页开着日志流,
/// 停机就会永远卡住,最后被容器宽限期结束时的 SIGKILL 硬掐——在途中转流照样被切断,
/// 统计最终刷盘反而彻底跳过,和不做优雅停机相比只坏不好。
///
/// 取 8s 的理由:必须**短于**容器宽限期(docker stop 默认 10s、本仓 compose 未改;
/// 编排器一般 30s),给窗口到期后的刷盘 + 收尾留出余量;同时长于绝大多数在途一问一答
/// 请求(控制面有整请求超时护栏),正常停机仍能干净排空。
const SHUTDOWN_DRAIN_WINDOW: Duration = Duration::from_secs(8);

/// 等服务器结束,但只给"停机信号已到"之后的排空阶段有界的等待时长。
///
/// - `server`:`axum::serve(..).with_graceful_shutdown(..)` 的 future。
/// - `shutdown_started`:停机信号已触发的通知(由信号 future 顺手发出)。
/// - 返回 `Some(结果)` = 服务器自行结束(干净排空);`None` = 窗口到期仍有连接未结束,
///   调用方应放弃等待、直接收尾。
///
/// 抽成泛型函数是为了能用假 future 单测边界行为,不必真开监听 socket。
/// 取 `IntoFuture` 而非 `Future`:axum 的 `WithGracefulShutdown` 只实现前者(普通 future
/// 经空实现自动满足),这样调用点直接传 `axum::serve(..)..` 的返回值即可。
/// `biased`:优先轮询服务器,使"信号与结束同时就绪"时确定性地报干净结束。
async fn drain_with_deadline<F, S>(
    server: F,
    shutdown_started: S,
    window: Duration,
) -> Option<F::Output>
where
    F: std::future::IntoFuture,
    S: std::future::Future<Output = ()>,
{
    let server = server.into_future();
    tokio::pin!(server, shutdown_started);
    tokio::select! {
        biased;
        // 服务器先结束:要么在途请求已全部跑完,要么 accept 出错提前返回。
        out = &mut server => return Some(out),
        // 停机信号到达:进入下面的有界排空窗口。
        _ = &mut shutdown_started => {}
    }
    tokio::time::timeout(window, &mut server).await.ok()
}

pub async fn serve(
    cfg: Config,
    log_capture: Option<Arc<crate::logcap::LogCapture>>,
) -> anyhow::Result<()> {
    for w in auth_startup_warnings(&cfg) {
        tracing::warn!("{w}");
    }
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let (app, persist) = build_router_with_persist_handles(Arc::new(cfg), log_capture);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("kiro2api listening on {addr}");
    // `into_make_service_with_connect_info` 使各 handler 可经 `ConnectInfo<SocketAddr>`
    // 拿到 socket 对端地址(用于提取客户端 IP;有反代时优先 X-Forwarded-For/X-Real-IP)。
    //
    // `with_graceful_shutdown`:收到 SIGTERM/SIGINT 后停止 accept,已建立的连接(含在途
    // SSE 长流)跑完才返回,而不是被 SIGKILL 硬掐。
    //
    // 但这份等待必须封顶(见 SHUTDOWN_DRAIN_WINDOW):admin 日志 SSE 是无限流,单靠
    // graceful shutdown 会永远等下去。故信号 future 在放行 axum 的同时经 oneshot 通知
    // 本函数开始计时,由 drain_with_deadline 给排空阶段设上界。
    let (drain_tx, drain_rx) = tokio::sync::oneshot::channel::<()>();
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        // 接收端在 serve 结束前一直活着;即便已被丢弃也无需处理(那说明服务器已自行结束)。
        let _ = drain_tx.send(());
    });

    let outcome = drain_with_deadline(
        server,
        async {
            let _ = drain_rx.await;
        },
        SHUTDOWN_DRAIN_WINDOW,
    )
    .await;
    if outcome.is_none() {
        tracing::warn!(
            drain_window_secs = SHUTDOWN_DRAIN_WINDOW.as_secs(),
            "排空窗口到期仍有连接未结束(常见于 admin 实时日志 SSE 这类长连接),不再等待,直接刷盘退出"
        );
    }
    // 无论是干净排空还是窗口到期,最终刷盘都必须跑且只跑一次:把去抖窗口内(约 5s)
    // 未落盘的写全部落下去,否则每次停机都静默丢这一段。
    //
    // 范围不能只有统计:API-KEY 的增删同样是「置脏 + 等下一拍」,而它丢一拍是安全问题——
    // 管理员刚吊销的 key 会随旧 api_keys.json 复活、重启后继续鉴权通过(见 PersistHandles)。
    // 后台刷盘任务本身指望不上:进程退出时 tokio 运行时随之销毁,那一拍根本轮不到。
    persist.flush_before_exit().await;
    tracing::info!("kiro2api 已停止");
    // 服务器自身的错误(accept 失败等,极罕见)放到刷盘之后再上抛,
    // 避免用 `?` 提前返回把统计吞掉。
    if let Some(res) = outcome {
        res?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::sync::Arc;
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_returns_ok_json() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["service"], "kiro2api");
    }

    #[tokio::test]
    async fn admin_index_served() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let html = String::from_utf8_lossy(&bytes);
        assert!(html.contains("kiro2api") && html.contains("js/app.js"));
    }

    #[tokio::test]
    async fn admin_index_served_without_trailing_slash() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let html = String::from_utf8_lossy(&bytes);
        assert!(html.contains("kiro2api") && html.contains("js/app.js"));
    }

    #[tokio::test]
    async fn user_index_served_without_trailing_slash() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(Request::builder().uri("/user").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let html = String::from_utf8_lossy(&bytes);
        assert!(html.contains("kiro2api") && html.contains("/user/assets/"));
    }

    #[tokio::test]
    async fn user_index_served_with_trailing_slash() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/user/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let html = String::from_utf8_lossy(&bytes);
        assert!(html.contains("kiro2api") && html.contains("/user/assets/"));
    }

    /// /v1/messages 已挂载:默认配置(凭据文件不存在 → 空池)下返回 503,
    /// 证明路由接线成功且默认配置服务器测试不受影响。
    #[tokio::test]
    async fn messages_route_mounted_returns_503_on_empty_pool() {
        let app = build_router(Arc::new(Config::default()));
        let body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// 关键回归:多模态请求(base64 内联图片)的 body 轻易超过 axum 默认的 2MB 上限。
    /// 协议组不显式抬高上限时,body 在 handler 之前就被 `Json` 提取器拒掉——`/v1/messages`
    /// 会把这次拒绝映射成 400,表现为"发个稍大的图就报参数错误",而账号池为空时本该是 503。
    /// 故断言 503:没有 DefaultBodyLimit 这一层,它会退化成 400。
    #[tokio::test]
    async fn protocol_routes_accept_bodies_larger_than_axum_default_limit() {
        // 3MB 文本 > axum 默认 2MB,< 本仓 32MB 上限。
        let big = "a".repeat(3 * 1024 * 1024);
        let body = serde_json::json!({
            "model": "sonnet",
            "messages": [{ "role": "user", "content": big }],
        })
        .to_string();
        assert!(
            body.len() > 2 * 1024 * 1024,
            "前提:body 须超过 axum 默认上限"
        );

        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "超过 2MB 的多模态请求必须能进到 handler(空池 503),而不是被默认体积上限拒在门外"
        );
    }

    /// 上限不能取消:body 由 `Json<T>` 全量缓冲进内存,32MB 之外必须挡下。
    #[tokio::test]
    async fn protocol_routes_still_reject_absurdly_large_bodies() {
        let big = "a".repeat(PROTOCOL_BODY_LIMIT_BYTES + 1024);
        let body = serde_json::json!({
            "model": "sonnet",
            "messages": [{ "role": "user", "content": big }],
        })
        .to_string();
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "超过上限的 body 必须被拒,不能一路缓冲进内存"
        );
    }

    const MESSAGES_BODY: &str = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;

    fn messages_request(uri: &str, auth_header: Option<(&str, &str)>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some((name, value)) = auth_header {
            builder = builder.header(name, value);
        }
        builder.body(Body::from(MESSAGES_BODY)).unwrap()
    }

    /// 鉴权闸开启(api_key = Some("secret"))时,/v1/messages 无 Authorization → 401。
    #[tokio::test]
    async fn protected_messages_without_auth_returns_401() {
        let cfg = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(messages_request("/v1/messages", None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// 鉴权闸开启时,错误的 Bearer key → 401。
    #[tokio::test]
    async fn protected_messages_with_wrong_bearer_returns_401() {
        let cfg = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(messages_request(
                "/v1/messages",
                Some(("authorization", "Bearer wrong")),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// 鉴权闸开启时,正确的 Bearer key → 通过鉴权(空池 503,但不是 401)。
    #[tokio::test]
    async fn protected_messages_with_correct_bearer_passes_auth() {
        let cfg = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(messages_request(
                "/v1/messages",
                Some(("authorization", "Bearer secret")),
            ))
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// 鉴权闸开启时,正确的 x-api-key → 通过鉴权。
    #[tokio::test]
    async fn protected_messages_with_correct_x_api_key_passes_auth() {
        let cfg = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(messages_request(
                "/v1/messages",
                Some(("x-api-key", "secret")),
            ))
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// 鉴权闸开启时,正确的 ?token= query 参数 → 通过鉴权。
    #[tokio::test]
    async fn protected_messages_with_correct_query_token_passes_auth() {
        let cfg = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(messages_request("/v1/messages?token=secret", None))
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// 鉴权闸开启时,/health 仍然公开,不受影响。
    #[tokio::test]
    async fn health_stays_open_when_api_key_set() {
        let cfg = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// api_key 未配置(None)时为开放模式:/v1/messages 无 auth 头 → 非 401(空池 503)。
    #[tokio::test]
    async fn open_mode_without_api_key_allows_unauthenticated_messages() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(messages_request("/v1/messages", None))
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// admin_api_key 设置时,/admin/api/stats 无 key → 401。
    #[tokio::test]
    async fn admin_stats_without_key_returns_401_when_admin_key_set() {
        let cfg = Config {
            admin_api_key: Some("adm".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// admin_api_key 设置时,正确的 Bearer key → 通过鉴权(非 401)。
    #[tokio::test]
    async fn admin_stats_with_correct_admin_key_passes_auth() {
        let cfg = Config {
            admin_api_key: Some("adm".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/api/stats")
                    .header("authorization", "Bearer adm")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// admin_api_key 未设置但 api_key 设置时,admin 鉴权回退到 api_key。
    #[tokio::test]
    async fn admin_auth_falls_back_to_api_key_when_admin_key_unset() {
        let cfg = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let cfg2 = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app2 = build_router(Arc::new(cfg2));
        let resp2 = app2
            .oneshot(
                Request::builder()
                    .uri("/admin/api/stats")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(resp2.status(), StatusCode::UNAUTHORIZED);
    }

    /// 仅用 store key 部署(config 里 api_key / admin_api_key 都没配)时,/admin API **仍开放**。
    ///
    /// 这是防"首次部署自锁"的回归测试,与直觉相反故写明:store key 是**用户级**凭据,在 admin
    /// 闸永远不放行。若让它把 admin 闸判成"已配置鉴权",首次部署的运营者一在开放面板里建出第一把
    /// API-KEY,下一次 /api/admin/* 就 401——而能开锁的管理员凭据一把都不存在,产品内再无自救途径。
    /// 故 admin 闸的"是否已配置"只认管理员级凭据(admin_api_key,缺省回退 api_key);运营者据此
    /// 仍能在开放面板里调 PUT /config/auth-keys 设好管理员密钥,当场收口(见 auth.rs 同名回归测试)。
    #[tokio::test]
    async fn admin_stats_open_when_only_store_keys_exist() {
        let dir = crate::test_tmp::stable_dir("srv_adminguard");
        let creds = dir.join("credentials.json").to_string_lossy().into_owned();
        let store_path = crate::apikey::api_keys_path_from(&creds);
        let _ = std::fs::remove_file(&store_path);
        {
            let store = crate::apikey::ApiKeyStore::load(&store_path);
            let _ = store.create(
                "0001".into(),
                None,
                None,
                None,
                None,
                None,
                chrono::Utc::now(),
            );
            store.save_now().unwrap();
        }

        let cfg = Config {
            credentials_path: creds,
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "store key 不得关上 admin 闸,否则首次部署建完第一把 key 即自锁"
        );
        let _ = std::fs::remove_file(&store_path);
    }

    /// 启动告警必须显式点名管理面(而非只提协议端点),并给出可操作的补救指令。
    #[test]
    fn auth_startup_warnings_name_admin_panel_and_shared_key_risk() {
        // 一个 key 都没配:协议端点 + 管理面各一条,管理面那条须点名 /admin 与 ADMIN_API_KEY。
        let none = auth_startup_warnings(&Config::default());
        assert_eq!(none.len(), 2, "{none:?}");
        assert!(none.iter().any(|w| w.contains("协议端点")));
        let admin_warn = none
            .iter()
            .find(|w| w.contains("/admin"))
            .expect("必须点名管理面同样开放");
        assert!(admin_warn.contains("ADMIN_API_KEY"), "{admin_warn}");

        // 只设主 key:管理闸回退用它,须提示数据面 key 同时具备管理员权限。
        let only_api = auth_startup_warnings(&Config {
            api_key: Some("sk-main".into()),
            ..Config::default()
        });
        assert_eq!(only_api.len(), 1, "{only_api:?}");
        assert!(only_api[0].contains("管理员权限"), "{}", only_api[0]);
        assert!(only_api[0].contains("ADMIN_API_KEY"), "{}", only_api[0]);

        // 只设 admin key:协议端点仍开放,管理面无隐患。
        let only_admin = auth_startup_warnings(&Config {
            admin_api_key: Some("adm".into()),
            ..Config::default()
        });
        assert_eq!(only_admin.len(), 1, "{only_admin:?}");
        assert!(only_admin[0].contains("协议端点"));

        // 两个都设 → 无告警;空串/纯空白视同未设置。
        assert!(
            auth_startup_warnings(&Config {
                api_key: Some("sk-main".into()),
                admin_api_key: Some("adm".into()),
                ..Config::default()
            })
            .is_empty()
        );
        assert_eq!(
            auth_startup_warnings(&Config {
                api_key: Some("   ".into()),
                admin_api_key: Some(String::new()),
                ..Config::default()
            })
            .len(),
            2
        );
    }

    /// admin_api_key 与 api_key 均未设置、且 store 里一条 key 都没有时,admin API 开放访问
    /// (首次运行体验;只要出现任一鉴权材料就不再开放,见上一条测试)。
    #[tokio::test]
    async fn admin_stats_open_when_no_keys_configured() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// Phase 6:日志流路由挂在 admin 组下,`?api_key=<admin key>` 经 admin 闸放行,
    /// 且日志捕获已接线时返回 200 + SSE content-type(EventSource 无法设头,只能 query 携带 key)。
    #[tokio::test]
    async fn log_stream_authenticates_via_query_api_key_and_streams() {
        let cfg = Config {
            admin_api_key: Some("adm".into()),
            ..Config::default()
        };
        let capture = Arc::new(crate::logcap::LogCapture::new(16));
        let app = build_router_with_logs(Arc::new(cfg), Some(capture));

        // 无 key → 401(admin 闸拦截)。
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/admin/logs/stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // 错误 key → 401。
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/admin/logs/stream?api_key=wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // 正确 key via ?api_key= → 200 + SSE。
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/logs/stream?api_key=adm")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.starts_with("text/event-stream"), "content-type = {ct}");
    }

    /// 排空窗口必须有界,且短于容器默认宽限期(docker stop 10s),
    /// 否则窗口一到就被 SIGKILL,最终刷盘照样跑不到。
    #[test]
    fn shutdown_drain_window_is_bounded_below_container_grace() {
        assert!(SHUTDOWN_DRAIN_WINDOW >= Duration::from_secs(1));
        assert!(
            SHUTDOWN_DRAIN_WINDOW <= Duration::from_secs(9),
            "排空窗口须给刷盘留余量:{SHUTDOWN_DRAIN_WINDOW:?}"
        );
    }

    /// 服务器在停机信号之前就自行结束(如 accept 出错):原样交回结果,不进排空窗口。
    #[tokio::test]
    async fn drain_returns_server_result_when_server_finishes_first() {
        let out = drain_with_deadline(
            async { 7u32 },
            std::future::pending::<()>(),
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(out, Some(7));
    }

    /// 信号已到但在途请求在窗口内跑完:等到干净结束,拿到服务器结果(正常停机路径)。
    #[tokio::test]
    async fn drain_waits_for_inflight_requests_within_window() {
        let out = drain_with_deadline(
            async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                1u32
            },
            async {},
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(out, Some(1));
    }

    /// 关键回归:连接永不结束(admin 日志 SSE 就是这种无限流)时,等待必须在窗口到期后
    /// 放弃并返回 None,由调用方继续刷盘退出——而不是永远挂着等 SIGKILL。
    #[tokio::test]
    async fn drain_gives_up_when_connection_never_ends() {
        let started = Instant::now();
        let window = Duration::from_millis(80);
        let out = drain_with_deadline(std::future::pending::<()>(), async {}, window).await;
        assert!(out.is_none(), "窗口到期必须放弃等待");
        // 确实等满了窗口(留调度余量,只校验下界)。
        assert!(started.elapsed() >= Duration::from_millis(60));
    }

    /// 每个测试独占的临时目录(pid + 纳秒,互不串扰)。
    fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
        crate::test_tmp::dir(&format!("srv_{tag}"))
    }

    /// 目录下的损坏备份文件列表(`*.corrupt.*`)。
    fn corrupt_backups(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut v: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().contains(".corrupt."))
            .collect();
        v.sort();
        v
    }

    /// 两条好账号中间夹一条坏账号(`expiresAt` 不是 RFC3339)——生产最常见的坏法:
    /// 整份 credentials.json 因**一条**账号写坏而 serde 全盘失败。
    const MIXED_CREDENTIALS_JSON: &str = r#"[
  {"id": 1, "accessToken": "at-1", "refreshToken": "rt-1", "expiresAt": "2099-01-01T00:00:00Z", "authMethod": "social"},
  {"id": 2, "accessToken": "at-2", "refreshToken": "rt-2", "expiresAt": "never", "authMethod": "social"},
  {"id": 3, "accessToken": "at-3", "refreshToken": "rt-3", "expiresAt": "2099-01-01T00:00:00Z", "authMethod": "idc"}
]"#;

    /// 关键回归:一条坏账号不得把整个池吃成 0,且坏文件必须先被原样备份。
    /// 旧写法 `credential::load(..).unwrap_or_default()` 会静默返回空池 —— 运营者据此
    /// "重新加账号",admin 的原子落盘随即用空池整份覆写掉约千个账号的凭据。
    #[test]
    fn corrupt_credentials_file_is_backed_up_and_salvaged_entry_by_entry() {
        let dir = unique_temp_dir("credsalvage");
        let path = dir.join("credentials.json");
        std::fs::write(&path, MIXED_CREDENTIALS_JSON).unwrap();
        let p = path.to_str().unwrap();

        // 前提:整份文件确实解析不了(证明"一条拖垮全部"这个前提成立)。
        assert!(credential::load(p).is_err());

        let creds = load_credentials_resilient(p);
        assert_eq!(creds.len(), 2, "坏条目只能吃掉它自己,其余账号必须照常入池");
        assert_eq!(creds[0].id, "1");
        assert_eq!(creds[1].id, "3");

        // 原文件仍留在原处:供人工修复,且下次重启还能再抢救一次。
        assert!(path.exists(), "凭据本体不得被改名带走");
        // 备份存在且与原文件逐字节相同——此后管理面的任何原子写都覆不掉它。
        let backups = corrupt_backups(&dir);
        assert_eq!(backups.len(), 1, "{backups:?}");
        assert_eq!(
            std::fs::read(&backups[0]).unwrap(),
            MIXED_CREDENTIALS_JSON.as_bytes()
        );
        // 备份里是明文 token,权限必须仅属主可读写。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&backups[0]).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "凭据备份必须 0600,实际 {mode:o}");
        }

        // 再来一次(模拟容器反复重启):同一份坏内容不重复堆备份,不把数据盘撑爆。
        let again = load_credentials_resilient(p);
        assert_eq!(again.len(), 2);
        assert_eq!(corrupt_backups(&dir).len(), 1, "同一份坏内容不应重复备份");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 语法层面就坏了(数组多一个逗号)时逐条抢救无从下手,但**备份必须照做** ——
    /// 这正是"管理面下一次写入会把整份账号文件覆盖掉"的高危场景。
    #[test]
    fn syntactically_broken_credentials_are_backed_up_even_when_nothing_can_be_salvaged() {
        let dir = unique_temp_dir("credsyntax");
        let path = dir.join("credentials.json");
        let broken = r#"[
  {"id": 1, "accessToken": "at-1", "refreshToken": "rt-1", "expiresAt": "2099-01-01T00:00:00Z", "authMethod": "social"},
]"#;
        std::fs::write(&path, broken).unwrap();
        let p = path.to_str().unwrap();

        let creds = load_credentials_resilient(p);
        assert!(
            creds.is_empty(),
            "语法错时抢救不出账号(合理),但不能悄无声息"
        );
        let backups = corrupt_backups(&dir);
        assert_eq!(backups.len(), 1, "坏文件必须先备份再继续:{backups:?}");
        assert_eq!(std::fs::read(&backups[0]).unwrap(), broken.as_bytes());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 接线回归:坏凭据文件下 `build_router` 起来的进程,管理面看到的账号数必须是抢救出的 2,
    /// 而不是 0(0 会把运营者引向"重新加账号",从而覆写掉整份账号文件)。
    #[tokio::test]
    async fn router_boots_with_salvaged_pool_when_credentials_file_is_corrupt() {
        let dir = unique_temp_dir("credrouter");
        let path = dir.join("credentials.json");
        std::fs::write(&path, MIXED_CREDENTIALS_JSON).unwrap();

        let cfg = Config {
            credentials_path: path.to_string_lossy().into_owned(),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/credentials")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["total"], 2, "坏一条不该让整池归零:{v}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 关键回归:API-KEY 的增删只置脏、要等下一拍(约 5s)才落盘,故停机前必须显式刷。
    /// 不刷的后果:管理员刚吊销的 key 会随旧 api_keys.json 复活(重启后继续鉴权通过),
    /// 刚建出交给用户的 key 则凭空消失(用户拿着新 key 直接 401)。
    #[tokio::test]
    async fn shutdown_flush_persists_api_key_create_and_revocation() {
        let dir = unique_temp_dir("keyflush");
        let creds_path = dir.join("credentials.json").to_string_lossy().into_owned();
        let cfg = Config {
            credentials_path: creds_path,
            ..Config::default()
        };
        let (_app, persist) = build_router_with_persist_handles(Arc::new(cfg), None);

        let key = persist.api_keys.create(
            "k1".into(),
            None,
            None,
            None,
            None,
            None,
            chrono::Utc::now(),
        );
        // 去抖窗口内确实还没落盘(证明这一拍真的丢得掉;此处到断言之间无 await,
        // 后台刷盘任务不可能插进来,断言是确定性的)。
        assert!(
            !persist.api_keys_path.exists(),
            "新建 key 本就该等下一拍才落盘,这里若已存在说明前提变了"
        );

        persist.flush_before_exit().await;
        let after_create = std::fs::read_to_string(&persist.api_keys_path).unwrap();
        assert!(
            after_create.contains(&key.key),
            "停机刷盘必须落下新建的 key,否则管理员刚发出的 key 重启后 401"
        );

        // 吊销后同样必须落盘,否则重启即复活 —— 这是安全问题,不只是丢数据。
        assert!(persist.api_keys.delete(key.id));
        persist.flush_before_exit().await;
        let after_delete = std::fs::read_to_string(&persist.api_keys_path).unwrap();
        assert!(
            !after_delete.contains(&key.key),
            "已吊销的 key 不得在重启后复活"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 关键回归:停机刷盘的句柄集合必须**覆盖余额缓存与失败/限流事件日志**。
    ///
    /// 这两样和 API-KEY 一样是「改内存 + 置脏 + 等约 5s 的下一拍」,而 SIGTERM 收尾里那一拍
    /// 根本轮不到(进程退出,运行时随之销毁)。此前 `build_router_with_persist_handles` 连余额
    /// 句柄都不交出来,`serve()` 结构上就没法刷它:删账号时清掉的条目随旧文件复活,复用同一
    /// id 的新账号在 TTL 内(最长 5 分钟)显示上一位主人的余额;刚出的 401/403、429 现场则在
    /// 重启后从失败/限流弹窗里消失。
    #[tokio::test]
    async fn shutdown_flush_persists_balance_cache_and_event_logs() {
        let dir = unique_temp_dir("balevflush");
        let creds_path = dir.join("credentials.json").to_string_lossy().into_owned();
        let cfg = Config {
            credentials_path: creds_path,
            ..Config::default()
        };
        let (_app, persist) = build_router_with_persist_handles(Arc::new(cfg), None);
        // 先让各后台刷盘循环把 interval 的立即首拍空跑掉(此刻还没有脏数据,它什么都不写)。
        // 不等的话首拍可能与下面的置脏撞上,把文件写出去,断言就会因为错误的理由变绿。
        tokio::time::sleep(Duration::from_millis(80)).await;

        persist
            .balance
            .put(
                "7",
                crate::balance::BalanceSnapshot {
                    subscription_title: Some("KIRO PRO+".into()),
                    current_usage: 1.0,
                    usage_limit: 100.0,
                    remaining: 99.0,
                    usage_percentage: 1.0,
                    next_reset_at: None,
                    fetched_at_unix: 1_700_000_000,
                },
            )
            .await;
        persist
            .stats
            .record_failure(7, "api", 403, "forbidden-body", 1_700_000_000)
            .await;
        persist
            .stats
            .record_throttle(7, "api", "too-many-requests", 1_700_000_001)
            .await;
        // 去抖窗口内确实还没落盘(证明这一拍真的丢得掉)。
        assert!(
            !dir.join(crate::balance::BALANCE_CACHE_FILE).exists()
                && !dir.join("failure_log.json").exists()
                && !dir.join("throttle_log.json").exists(),
            "前提:这三样都只置脏,尚未落盘"
        );

        persist.flush_before_exit().await;

        // 重启:新进程从同一数据目录读回来,三样都必须在。
        let balance = crate::balance::BalanceCache::load_from_dir(&dir);
        assert!(
            balance.get_fresh("7", 1_700_000_100).await.is_some(),
            "停机刷盘必须落下余额缓存,否则重启后这一拍里的写(含删账号时的 invalidate)整段回滚"
        );
        let stats = crate::stats::StatsManager::load_from_dir(&dir);
        assert_eq!(
            stats
                .failure_log
                .records_for_credential(7, 1, 10)
                .await
                .total,
            1,
            "停机刷盘必须落下失败日志,否则重启后查不到停机前刚出的 401/403"
        );
        assert_eq!(
            stats
                .throttle_log
                .records_for_credential(7, 1, 10)
                .await
                .total,
            1,
            "停机刷盘必须落下限流日志,否则重启后查不到停机前刚出的 429"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 自保阀:磁盘上的 api_keys.json 解析不动(启动时被静默按空载入)且内存里一把 key 都没有时,
    /// 停机刷盘必须**跳过**写入 —— 否则空存储会把这份仍可人工挽救的密钥文件当场抹掉。
    #[tokio::test]
    async fn shutdown_flush_does_not_clobber_unparseable_api_keys_file() {
        let dir = unique_temp_dir("keyguard");
        let creds_path = dir.join("credentials.json").to_string_lossy().into_owned();
        let broken = b"{ this is not json";
        std::fs::write(dir.join("api_keys.json"), broken).unwrap();

        let cfg = Config {
            credentials_path: creds_path,
            ..Config::default()
        };
        let (_app, persist) = build_router_with_persist_handles(Arc::new(cfg), None);
        assert!(persist.api_keys.is_empty(), "损坏文件会被静默载成空 store");

        persist.flush_before_exit().await;
        assert_eq!(
            std::fs::read(&persist.api_keys_path).unwrap(),
            broken,
            "损坏的密钥文件不得被空存储覆盖"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 日志捕获未接线(build_router 默认 None)时,日志端点在通过鉴权后返回 503。
    #[tokio::test]
    async fn log_snapshot_503_when_capture_not_wired() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/logs/snapshot")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
