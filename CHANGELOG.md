# Changelog

本文件记录项目的所有重要变更。格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [Unreleased]

## [0.2.0] - 2026-07-26

一轮覆盖事件流、账号池、统计、登录、四协议、鉴权与部署的全链路审计与修复。改动面较大且含行为变更,故跳次版本号。

### Security

- **修复消费上限绕过**:给 API-KEY 设的消费上限此前**只在 Anthropic 端点生效**——OpenAI / OpenAI-Responses / Gemini 三条协议记账时把归属硬编码为 0,换用这些端点即可无限消费且永不 402,面板上这些流量的用量与花费还全部显示为零。现四协议一律按鉴权闸解析出的 key 归属并按真实计量结算。
- **修复管理面无鉴权开放**:仅配置了用户级 API-KEY(未配 `apiKey`/`adminApiKey`)时,`/api/admin/*` 此前对匿名放行——可读出全部用户 key 明文、抢改鉴权密钥、任意增删凭据、反复重启造成停服。现管理面只认管理员级凭据(`adminApiKey`,缺省回退 `apiKey`);同时**不**把用户级 key 计入"已配置鉴权",否则首次部署的运营者一建出第一把 key 就会把自己永久锁在门外。
- `credentials.json` 等含密文件的原子写临时文件改为 **0600** 创建(此前继承 umask 通常为 0644,而文件里是全部账号的明文令牌)。
- 空环境变量不再覆盖 `config.json`:`.env.example` 自带的 `API_KEY=` 经 compose 注入后会静默关掉鉴权。
- store key 校验改常量时间比较;API-KEY 编号改单调递增并持久化(编号复用会让新 key 继承前任的用量、真实 IP 与累计消费)。

### Fixed

- **上游错误不再被伪装成正常完成**:①上游在 HTTP 200 事件流里下发的 `exception` 帧此前被整体忽略,客户端拿到一个"内容为空/半截却正常结束"的响应,重试逻辑永不触发,该次失败还被计入成功用量;②**传输层中断**(连接重置 / 读超时 / chunked 体未收尾)同样落进正常收尾分支。现四条协议一律以各自规范的错误事件收束(Anthropic `error` 事件、OpenAI 错误 chunk 且不补 `[DONE]`、Responses `response.failed`、Gemini 错误块且绝不报 `STOP`),超时映射 504、其余 502。
- **截断不再被报成正常结束**:命中 `max_tokens` 或上下文耗尽时,流式路径此前一律报 `end_turn`/`stop`/`STOP`/`completed`;现与非流式同口径(`max_tokens` / `length` / `MAX_TOKENS` / `incomplete`)。
- **账号池**:刷新失败此前完全不反馈给池,`refresh_token` 已吊销的账号永远显示健康、每次被选中都白打一轮注定失败的刷新;现按 token 端点语义分类(仅 400/401/403 + `invalid_grant` 才判永久失效,其余一律降级为可自愈冷却,避免一次 5xx 或网关错误页永久禁用健康账号)。启停状态在内存与落盘两处同步;强制刷新以"调用方实际失败的那枚令牌"为基线,迟到者不再把别人刚换好的令牌再轮换一遍。
- **统计**:后台刷盘任务与脏标记构成 `Arc` 自引用环,导致"关闭时兜底刷盘"是死代码,每次重启静默丢最近约 5 秒的用量与计费;每日汇总与原始记录同源同被淘汰,使 7d/30d 汇总静默偏低;载入时用 `.ok()` 吞掉解析错误,损坏文件被当空库并在数秒后原子覆写。均已修复,并新增停机刷盘入口。
- **登录**:一次网络抖动或代理塞回的 HTML 错误页此前会销毁整个登录会话(用户即使已在浏览器点过授权也得从头再来);现区分瞬态与终态,设备码轮询按 OAuth 错误码判定,SSO 令牌兑换改为带上限的退避轮询。
- **事件流解码**:帧内 headers 段自截断会让解码器永久停滞并丢弃其后全部合法帧(非流式路径表现为响应截断 + 计费丢失);resync 预算耗尽时会连噪声之后已到达的合法帧一起清空。均已修复,并给 headers 段加上条数上限。
- **部署**:`--credentials` 此前解析了却从未使用,照文档部署后容器显示 healthy 但四协议全部 503,凭据与统计还写在容器可写层、升级即丢;健康检查只读 `config.json` 的端口,按文档改 `PORT` 后容器永远 unhealthy。均已修复。
- 修复纯 HTTP 访问下更新弹窗复制按钮无反应(非安全上下文回退 `document.execCommand('copy')`)。
- Gemini:`functionCall`/`functionResponse` 的 id 配对改为"回带 id 精确配对、缺 id 配最近一轮未被答的同名调用",消除并行同名调用被并成一条、以及工具结果成孤儿导致模型反复重发同一调用的问题;`streamGenerateContent` 支持 `alt=sse` 与默认 JSON 数组两种线格式。
- OpenAI:`system`/`tool` 角色的数组形 `content` 此前被替换成空字符串(工具结果整体丢失);`developer` 角色未映射为 `system`;Responses 的 `input` 条目此前把 `type` 设为必填,官方 SDK 的常见写法与多轮回灌都会 422。均已修复,并补上流式 `usage` 与 `stream_options.include_usage`。

### Changed

- **优雅停机加排空上限**:管理面实时日志 SSE 是无界长连接,此前会让停机一直等到容器宽限期结束被强杀(在途流被掐断、最终刷盘跳过);现最多等待 8 秒即刷盘退出。
- **统计磁盘格式保持可回滚**:`usage_records.json` 恒为裸数组(旧版本可原样解析),新增的每日汇总落在旁挂的 `usage_records.daily.json`。
- **凭据路径的内置默认改为就近解析**到 `-c` 所指配置文件的目录(容器因此仍默认落在挂载卷内),镜像不再烘焙 `CREDENTIALS_PATH` 环境变量——环境变量层优先级高于 `config.json`,烘焙会把用户自定义的路径静默改道。
- 账号 id 改单调递增并持久化,重启后不再复用已删除账号的编号。
- 文档修正:`/api/admin/*` 只有配置了 `adminApiKey`(缺省回退 `apiKey`)之后才受保护,`/api/user/*` 不走该闸、始终要求调用方自带有效 API-KEY;`logCapacity` 默认值、配置优先级补上命令行层。

## [0.1.4] - 2026-07-26

### Fixed

- **Anthropic `system` 字段兼容内容块数组**:真实客户端(Claude Code、带 prompt 缓存的 SDK)会把 `system` 发成 `[{"type":"text","text":"…","cache_control":{…}}]` 数组,之前只接受字符串会返回 422(`invalid type: sequence, expected a string`)。现在 `system` 同时接受字符串与内容块数组两种形态,转发到 Kiro 后端与 `count_tokens` 时拍平为纯文本(OpenAI/Gemini/Responses 前端不受影响)。

## [0.1.3] - 2026-07-26

批量导入 JSON 的实时化改版。

### Changed

- **批量导入 JSON 改为实时逐条验活显示**:导入对话框现在逐个账号处理并实时展示——进度条 +「正在处理账号 i/N」+ 成功/重复/失败实时统计 + 每个账号一行的状态列表(等待中 → 检查重复 → 验活中 → 验活成功[带用量] / 重复账号 / 验活失败[已排除])。验活通过的账号即时落库,中途中断也不丢失;导入进行中不可关闭对话框。(细化 v0.1.2 的「逐条验活 + 去重」导入。)

## [0.1.2] - 2026-07-25

检查更新弹窗改版,以及文档补全。

### Changed

- **检查更新弹窗对齐 gemini2api**:仪表盘加载时静默自检,有新版本时「检查更新」按钮高亮为「更新到 vX」;点击弹出「更新服务 vX」对话框,内含**当前界面语言**的发布说明(可滚动)与升级命令(`docker compose pull && docker compose up -d`,一键复制)。仅提示展示,不自动升级。
- **文档补全(此前 v0.1.1 遗漏)**:README(根 + 5 语言)「最近更新」表补上 v0.1.1 / v0.1.2 行、功能清单补「模型测试」与「批量导入(逐条验活 + 去重)」;USAGE(5 语言)新增「模型测试」小节、扩写「检查更新」与「批量导入」说明。

### Fixed

- 修复纯 HTTP(非 HTTPS)访问下更新弹窗的复制按钮无反应:非安全上下文 `navigator.clipboard` 不可用时回退 `document.execCommand('copy')`。

## [0.1.1] - 2026-07-25

面板体验与账号导入的一批修复,以及文档/构建完善。

### Added

- **模型测试默认可用主 API-KEY**:未创建任何自建密钥时,模型测试页默认列出并使用「主 API Key」,开箱即可测通。
- **批量导入账号「逐条:添加 → 验活 → 过滤」**:导入 JSON 时逐个账号单独添加,添加后查一次余额(`getUsageLimits` 真打上游)做**验活**——查得到=有效保留,报错=失效自动回滚删除,过滤掉已失效账号。
- **导入去重**:按 `refreshToken` 跳过已在池中的账号,避免同一账号被重复导入产生两条抢同一轮换令牌的凭据(刷新时互相作废、浪费配额、增加上游风控)。
- **用户面板 `/user` 与全部页面的品牌 favicon**;多语言 README 顶部加入 128×128 品牌 logo 与 version 徽章;`scripts/set-version.sh` 一处改动同步 VERSION / Cargo.toml / 各语言徽章。

### Changed

- **批量导入不再一次性提交超大请求**:改回旧版逐条添加(每个请求都小),从根上避免大批量导入触发请求体积上限而报「发生错误」。
- **镜像构建改为交叉编译**:多阶段 Dockerfile 用 `$BUILDPLATFORM` 原生工具链交叉编译 arm64(替代 QEMU 模拟),多架构镜像构建显著提速。

### Fixed

- 修复未创建密钥时模型测试无可用密钥可选的问题。
- 修复批量导入 JSON 在账号较多时报「发生错误」(请求体积超限)。
- 修复用户面板 favicon 未随管理面板一并更新的问题。

## [0.1.0] - 2026-07-25

首个公开版本。多协议 AI 中转，后端为 Kiro（CodeWhisperer），统一提供 Claude 系模型。

### Added

- **四协议前端**：OpenAI Chat（`/v1/chat/completions`）、Anthropic Messages（`/v1/messages`，中枢母格式）、OpenAI Responses（`/v1/responses`）、Gemini 原生（`/v1beta/models/{m}:generateContent`）；每协议同时挂标准裸前缀与显式厂商前缀（`/openai/v1`、`/claude/v1`、`/gemini/v1beta`）。
- **完整能力**：每协议均支持流式（SSE）、函数调用（工具）真透传、图片输入（多模态）。
- **Kiro 账号池**：多账号轮询（`priority`/`balanced`）、每账号 RPM 限流、分级冷却；body-aware 失败分类（真凭据失效才永久禁用，配额/风控/限流冷却自愈）。
- **令牌自愈**：token 到期单飞刷新 + 原子落盘 `credentials.json`；三种交互式登录流（Builder ID 设备码 / IAM SSO 授权码 / 社交令牌）。
- **端点回退与跨账号重试**：Kiro IDE → CodeWhisperer → AmazonQ 按序回退；账号级失败跨账号重试；确定性请求错误（`INVALID_MODEL_ID`）不重试、不误伤账号，直接以 400 回明确说明。
- **统一鉴权闸**：`Authorization: Bearer` / `x-api-key` / `?token=` 常量时间比较；`adminApiKey` 保护 `/api/admin/*`，持有者以自身 API-KEY 访问 `/api/user/*`；`/health`、`/v1/ping` 不鉴权。
- **Web 管理面板 `/admin`**：仪表盘（运行时间/全局积分/系统信息/赞助卡/检查更新）、账号管理、API-KEY 管理、用量统计（含客户端 IP 与账号标签）、实时日志（SSE）、设置（负载均衡/鉴权密钥/集成示例/一键重启）；顶部运行状态、GitHub、重启、深浅色主题、5 语言 i18n。
- **用户面板 `/user`**：持有者以自身 API-KEY 登录，查看额度、累计用量与分页记录。
- **统计与缓存**：每日/账号用量统计、失败/限流日志、账号余额缓存（TTL）、动态模型清单缓存。
- **版本检查 / 更新 / 重启**：`GET /api/admin/check-update`（GitHub Release 比对）、`POST /api/admin/update`（返回更新命令）、`POST /api/admin/restart`（二次确认）。
- **交付**：多阶段 Docker 构建、非 root 运行（gosu）、多架构镜像（amd64/arm64）、健康检查、CI（tag 触发 GHCR 发布）。
