<div align="center">

<img src="docs/logo.png" width="128" height="128" alt="kiro2api">

<h1>kiro2api</h1>
<h3>多协议 AI 中转 · Kiro 后端</h3>
<p>一套代码同时兼容 OpenAI / Anthropic / OpenAI-Responses / Gemini 四大 AI SDK，由 Kiro（CodeWhisperer）后端统一提供 Claude 系模型，纯异步 Rust 架构，Docker 快速部署。</p>

<p>
  <img src="https://img.shields.io/badge/Rust-2024-orange?style=flat-square&logo=rust&logoColor=white" alt="Rust">
  <img src="https://img.shields.io/badge/axum-0.8-000000?style=flat-square&logo=rust&logoColor=white" alt="axum">
  <img src="https://img.shields.io/badge/tokio-async-4E9A06?style=flat-square&logo=rust&logoColor=white" alt="tokio">
  <img src="https://img.shields.io/badge/Docker-20.10+-2496ED?style=flat-square&logo=docker&logoColor=white" alt="Docker">
  <img src="https://img.shields.io/badge/arch-amd64%20%7C%20arm64-4285F4?style=flat-square&logo=linux&logoColor=white" alt="Arch">
  <img src="https://img.shields.io/badge/License-MIT-green?style=flat-square" alt="License">
  <img src="https://img.shields.io/badge/version-v0.16.0-success?style=flat-square" alt="Version">
</p>

<p>
  <a href="#-最近更新">最近更新</a> &bull;
  <a href="#-核心功能">核心功能</a> &bull;
  <a href="#-系统要求">系统要求</a> &bull;
  <a href="#-快速部署">快速部署</a> &bull;
  <a href="#-接入示例">接入示例</a> &bull;
  <a href="#-api-端点">API 端点</a> &bull;
  <a href="#-配置说明">配置说明</a> &bull;
  <a href="#-注意事项">注意事项</a> &bull;
  <a href="#-开发路线">开发路线</a>
</p>

<p>
  📖 文档语言：<a href="docs/zh-CN/README.md">简体中文</a> | <a href="docs/zh-TW/README.md">繁體中文</a> | <a href="docs/en/README.md">English</a> | <a href="docs/ja/README.md">日本語</a> | <a href="docs/ko/README.md">한국어</a>
</p>

<br>

<a href="https://github.com/xwteam/kiro2api/issues"><img src="https://img.shields.io/github/issues/xwteam/kiro2api?style=flat-square" alt="Issues"></a>
<a href="https://github.com/xwteam/kiro2api/stargazers"><img src="https://img.shields.io/github/stars/xwteam/kiro2api?style=flat-square" alt="Stars"></a>

</div>

---

> [!NOTE]
> 本项目仅供研究和学习用途，请合理使用，不要用于任何商业目的。

> [!IMPORTANT]
> `apiKey`/`API_KEY` 为空**且尚未创建任何 API-KEY** 时，协议端点**开放访问**（启动会告警）；一旦在管理面板发出第一条 API-KEY，协议闸即收口。对外部署务必设置。管理接口 `/api/admin/*` 只有在配置了 `adminApiKey`（缺省回退 `apiKey`）之后才受保护——**两个 key 都不配时管理接口和面板一样是开放的**，任何人都能增删凭据、改鉴权密钥；`/admin`、`/user` 面板本体则始终不鉴权。部署到公网必须设置 `ADMIN_API_KEY`。容器镜像已内置 `HOST=0.0.0.0`；裸机部署请勿轻易把 `HOST` 改成 `0.0.0.0`。

> [!TIP]
> 后端为 Kiro（CodeWhisperer）账号池。**可用模型取决于账号订阅档位**：免费档（KIRO FREE）通常只授权 `claude-sonnet-4.5`，opus/GPT 等需更高档位——请求不支持的模型会明确返回 400（`INVALID_MODEL_ID`），而非静默失败。

---

## 📝 最近更新

> 下表只列**最近 10 次**更新;完整更新日志请查看 [CHANGELOG.md](CHANGELOG.md)。

| 日期 | 更新内容 |
|------|----------|
| 2026-08-10 | v0.16.0 - 🔍 **又跑了一轮独立复核,找出 6 处——好几处是此前的修复只落了一半**。①`amz-sdk-invocation-id` 的 UUID 修复**只落了 3/5 个调用点**:balance 与 models_cache 各自私有一份同名旧生成器被局部遮蔽,搜索时看着像"已统一",实际同一枚 Bearer 在数据面发 UUID、在余额发裸 hex;②**登录流对 oidc 主机一个头都不带、连 UA 都没有**——正是 v0.10.0 在刷新链路修过的同一缺陷,而登录打的是**同一台主机的相邻路径**,且注册裸奔、几分钟后同一 clientId 又用完美 SDK 头去刷新,这种前后矛盾比裸请求更易被关联;③**region 三条链路各算各的**(数据面按 profileArn、余额/模型用裸 cred.region)→ 余额恒查不出,且同一枚 Bearer 同时命中两个 region;④history 里的工具名没走缩短,与 tools 列表的短名对不上;⑤**改优先级不触发重新选号**,粘滞档下等于没改;⑥**全池被判停后无自愈**——上游抖一阵把所有号判停,池子就此彻底不可用直到有人重启。另:README 更新历史只留最近 10 条,完整看 CHANGELOG |
| 2026-08-10 | v0.15.0 - 🔁 **额度耗尽的账号不再每次重启都要重新学一遍**(用户直接问到:"额度用完的号不是已经禁用了吗,为什么还会请求到已禁用的账号?")。是禁用了,但只在**内存**里——v0.10.2 为免一次抖动把账号永久写死,让运行期停用不落盘;而配额恰恰是**有明确恢复时刻**的那类。于是每次发版重启就把"谁没额度"忘光,再拿**用户的请求**去重新发现(实测 13 个耗尽号:前 2 次 502、第 3 次才成)→ 现按恢复时刻落盘,重启仍记得、到点自动回池;恢复时刻优先取上游 `nextResetAt`,没有才按下月一号估。另:**服务端内置搜索 `web_search` 真正接上了** —— 此前只是"容忍"这个工具声明(不再 400),但从不真的搜索,模型照常回段文本、客户端以为搜过了。现在这类请求在进数据面前被截住,调上游 `/mcp` 端点拿结果再合成 `server_tool_use` + `web_search_tool_result` |
| 2026-08-10 | v0.14.1 - 🔧 **线上验证时当场发现的两件事**。①响应里 `usage.input_tokens` **恒为 0**:v0.13.0 补的输入估算只喂给了计费、没回写客户端 —— 账单里有、响应里没有,而客户端拿它算成本和上下文占用 → 非流式 `usage` 与流式 `message_start` 现在都带同一个值;②**池里有一批耗尽的号时,用户前几次请求会连续失败**:配额耗尽此前与鉴权失败共用 3 次重试预算,实测 13 个耗尽号要连烧 2 次 502、第 3 次才成 → 配额是**确定性**结论(本周期换谁都一样),现与"模型不可用"同归账号级确定性档、按池大小给预算;瞬态/鉴权仍保持 3 次小上限。全池确实耗尽时回 `429` 并说清是额度问题,而不是语焉不详的 502 |
| 2026-08-10 | v0.14.0 - 🧩 **对照收尾**。①历史里助手轮 `content` 可能是**空串**,上游据此拒掉整条请求——纯工具调用那一轮(只有 tool_use、没文本)就是空,用户轮早有兜底、助手轮一直漏着,于是"上一轮只调了工具没说话"的对话再发一次就必挂,而那正是工具链里最常见的形态 → 用单个空格占位;②**损坏帧被逐字节重扫,而它的边界本来已知**:prelude CRC 一旦通过 `total_len` 就可信,此前不分情况逐字节再同步,于是一个 message-CRC 失败的帧会把整段 payload 当噪声重扫,既慢又可能从 payload 里凑出假帧头、解出根本不存在的消息 → 边界已知的坏帧整帧跳过;③**`tlsBackend` 改为运行时可切**(此前编译期二选一,换后端要重出镜像)——native-tls 用系统证书库、rustls 用内置根证书,走自签 CA 代理时往往只有一个握得上手,而现象是"刷不出令牌/连不上",与 TLS 毫无字面关系 |
| 2026-08-09 | v0.13.0 - 🧠 **扩展思考(thinking)完整接上**,此前整个功能缺失:请求侧 `thinking` 字段被静默丢弃(上游根本收不到指令),响应侧上游把思考用 `<thinking>…</thinking>` 包在普通文本里下发、我们原样透传 → 客户端把整段思考当正文显示。现按 enabled/adaptive 生成指令注入 system 最前,并切成独立 `thinking` 块(流式发 `thinking_delta`),流式与非流式共用同一份增量切分器;**普通文本零延迟透传**(只压最后一个 `<` 起的那一小段,无脑压标签长度会让下行文本卡顿——实现时真踩到过,被测试挡下)。另修:**token 估算对中文低估约三倍**(全局 chars/4 → 按字符类别加权,直接影响用量统计与 USD 限额);**流式记账 input token 恒为 0**(非流式早有估算、流式一直没有,两条路的账对不上);**上下文窗口全表钉死 200K**(上游 `maxInputTokens` 解析了又丢,1M 的模型被低报五倍;且 `max_tokens` 装的其实是输出上限,与静态表的窗口含义打架)→ 拆成两个字段 |
| 2026-08-09 | v0.12.0 - 🎚️ **不同订阅档位的账号终于能共存**(用户提供的真实 ksk 实测复现并验证)。两个半根因:①`INVALID_MODEL_ID` 被归为**请求级**错误直接回 400,而它其实是**账号级**的——可用模型由档位决定,而我们对客户端暴露的是全池**并集**,并集里的模型落到不支持它的账号上就必错 → 拆出 `ModelUnavailable`,不罚账号但换号再试;②换号预算只有 3 次,而支持该模型的号可能排第 14 → 模型不可用**不占**账号故障预算。另:记住「谁不支持哪个模型」(第 2 次同模型请求只选 1 个号,第 1 次是 14 个);**`priority` 此前只是 `weight` 的别名、从不参与选号** → 现数字越小越优先,导入一律 999,手工可调;非 us-east-1 账号刷新模型必然失败(`codewhisperer.{region}` 在该区不存在,DNS 都解析不了)→ 回落 `q.{region}` |
| 2026-08-09 | v0.11.1 - 🔬 **线上实测挖出的两件事**。①**工具描述为空时上游拒掉整条请求**(实测:同一工具带描述 200 并正常回 tool_use,去掉描述 → `400 Invalid tool use format / REQUEST_BODY_INVALID`)。v0.11.0 把 null 改成了空串,而上游要的是**非空** → 现用工具名兜底;②`REQUEST_BODY_INVALID` 被当成可重试:它是确定性的,换账号也一样失败,此前一条畸形请求连烧几个账号的重试配额(实测一次打了 4 个号)最后回个语焉不详的 502 → 现归入不重试不罚账号那档,直接回 400 并点明多半是工具规格的问题 |
| 2026-08-09 | v0.11.0 - 🧰 **修「会让请求被上游拒」的一类**。①Anthropic **服务端内置工具**(web_search 等)没有 `input_schema`,而该字段此前是必填 → 客户端一用官方写法,请求就在我们这层 400;②工具 `description` 会序列化成 **null**(真实客户端恒为字符串);③`input_schema` 原样透传,`properties: null` 这类形状不合法会被上游拒掉**整条请求** → 现统一规范化(只补形状不改语义);④超长工具名不缩短(上游上限 63)、缩短后也无法还原 → 现确定性缩短并把 `短名→原名` 带到出口,流式/非流式都还原,否则客户端收到自己没声明过的工具;⑤`:message-type == "error"` 的框架级错误帧被整个忽略 → 上游报错却还原成 200+空消息;⑥面板缺失资源返回 200+HTML 而非 404 —— **这条会让「用 curl 核对产物是否上线」本身骗人**。另:新增 `POST /api/admin/credentials/{id}/refresh` 强制换发令牌 |
| 2026-08-09 | v0.10.2 - 🩹 **修:运行期停用被写进磁盘,「重启即复活」形同虚设**。v0.10.0 把额度耗尽/连续鉴权失败改为停止使用并声明只置内存态,但落盘时 `snapshot_credentials()` 拿运行时 `disabled` 覆写了 `cred.disabled` —— 一次配额耗尽或两次 401/403 就把账号**永久**写死在 credentials.json 里,重启也回不来,比修复前更糟(线上真的死过一个号)。现落盘只取持久结论;管理员手工停用与响应体确证的失效本就同时写 `cred.disabled`,不会漏。**若你的 credentials.json 里有你没手工停过的 `"disabled": true`,改回 false 即可恢复** |
| 2026-08-09 | v0.10.1 - 🔌 **出站代理按账号分流 + 补齐会话身份字段**。①代理三件套此前**收下即丢**、`hasProxy` 还硬编码 false —— 面板显示配好了、实际全程直连;现真正落库生效,优先级 凭据级 > 全局 > 直连(`"direct"` 显式直连),且**同一账号的数据面/刷新/余额/模型清单/后台续期一律同一出口**(数据面走代理而刷新从主 IP 出比不配更糟);②`conversationId` 此前每请求新生成且不是 UUID 形状 → 改为优先取客户端 `metadata.user_id` 里的 session UUID,同会话共用;③此前**根本不发** `agentContinuationId`,已补;④管理面新增的账号不会冻结 machineId(v0.10.0 只覆盖启动时文件里已有的),现入池即冻结;⑤`isCurrent` 硬编码 false,现报真值 |

---

## 🌟 核心功能

> 📖 详细使用文档：[简体中文](docs/zh-CN/USAGE.md) | [繁體中文](docs/zh-TW/USAGE.md) | [English](docs/en/USAGE.md) | [日本語](docs/ja/USAGE.md) | [한국어](docs/ko/USAGE.md)

### 🔌 四协议前端，一套后端

- 一个服务同时提供 **OpenAI Chat**、**Anthropic Messages**、**OpenAI Responses**、**Gemini 原生** 四种 SDK 格式
- 内部以 **Anthropic Messages 为中枢母格式**，其余协议双向转换后复用同一条中转内核
- 每个协议都支持**流式（SSE）**、**函数调用（工具）真透传**、**图片输入（多模态）**——图片一律**只接受内联 Base64 `data:` URI**；远程 http(s) 图片 URL 在 Anthropic（`source.type:"url"`）与 OpenAI Chat（`image_url`）端点会被拦成 `400`，而在 **Responses（`input_image`）与 Gemini** 端点会被**静默跳过**：请求照常返回 `200`，但模型根本收不到那张图（Gemini 只认 `inlineData`，没有 `fileData` 字段）。发图前请自行转成 `data:` URI
- **双前缀挂载**：每协议同时挂标准裸前缀与显式厂商前缀（`/openai/v1`、`/claude/v1`、`/gemini/v1beta`），主流 SDK 填 `base_url` 即插即用

### 🔐 统一鉴权闸

- 凭据通道按优先级取用：`Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > query（`?api_key=` > `?token=` > `?key=`），常量时间比较，失败即 `401`
- `adminApiKey`（缺省回退 `apiKey`）保护 `/api/admin/*`，两者都未配置时该闸为开放模式；持有者用自己的 **API-KEY** 访问 `/api/user/*`
- `/health`、`/v1/ping` 等探活端点不鉴权

### 🔄 账号池与令牌自愈

- **多账号轮询**：`priority`（等权轮询，默认）与 `balanced`（按 `weight` 加权）两种策略，可在管理面板运行期切换
- 每账号独立 RPM 限流、分级冷却；连续失败按类别（永久失效 / 歧义鉴权 / 配额 / 瞬时）差异化处置
- token 到期**自动内存刷新**（单飞协调，避免并发刷新级联 401），刷新成功原子落盘 `credentials.json`
- 支持 Builder ID 设备码 / IAM SSO 授权码 / 社交令牌三种登录流，凭据可 drop-in 现有 Kiro 数据

### 🔀 端点回退与跨账号重试

- Kiro IDE → CodeWhisperer → AmazonQ 多端点按序回退，`429`/网络错自动切换
- 账号级失败自动跨账号重试；确定性请求错误（如不支持的模型 `INVALID_MODEL_ID`）**不瞎重试、不误伤账号**，直接把上游原因回给客户端
- body-aware 失败分类：只有真正的凭据失效才永久禁用，配额/风控/限流一律冷却自愈

### 🖥 Web 管理面板

- 内置静态管理台（`/admin`），凭 `adminApiKey` 登录，`/api/admin/*` 富接口驱动
- **仪表盘**：运行时间实时计时、全局剩余积分、系统信息（版本/Rust/OS/内存/CPU/PID/运行模式）、赞助二维码卡（实时拉取远程配置）、**检查更新**（GitHub Release 比对，弹窗展示当前语言的版本更新说明 + 可一键复制的升级命令）
- **账号管理**：增删改查、三种交互式登录、批量导入（逐条验活 + 去重）、优先级/权重、余额查询
- **模型测试**：从面板向任意模型发一条测试请求验证连通性；未创建自定义 key 时默认用主 API-KEY
- **API-KEY 管理**：发放/禁用/改标签、消费上限与有效期（在四种协议前端统一计量并拦截）、按 key 用量与分页记录
- **用量统计**：每日/账号维度、含客户端 IP 与账号标签、按日下钻
- **实时日志**：结构化表格 + 方向过滤 + 搜索 + 分页 + SSE 实时推送 + 下载
- **设置**：运行期切负载均衡/鉴权密钥、集成示例（协议×语言可复制片段）、**一键重启服务**
- 顶部控制栏：运行状态徽章、GitHub、重启、深浅色主题、5 语言切换

### 👤 用户面板

- 内置用户台（`/user`），持有者用自己的 **API-KEY** 登录（无需 admin 权限）
- 查看该 key 的额度、累计用量与分页记录，由 `/api/user/*` 驱动

### 🧭 模型名映射

- 客户端传入的模型名按**小写子串**匹配到 Kiro 内部模型（未匹配到 → `400`）
- 协议侧 `/models` 返回**固定短清单**（OpenAI/Gemini：`claude-sonnet-4.5`、`claude-opus-4.6`、`gpt-5.6-sol`；Anthropic 形状：`claude-sonnet-4.5`、`claude-opus-4.6`、`claude-haiku-4.5`），**不读账号池、不按订阅档位过滤**——列出的模型你的档位未必授权（返回 `400 INVALID_MODEL_ID`），未列出的模型只要能映射到内部模型且档位授权也照样可用。完整目录见管理接口 `GET /api/admin/models`（账号池实拉的并集，为空时回落静态 17 模型目录）

### ⚡ 高性能架构

- 基于 **Rust + axum 0.8 + tokio**，全链路异步非阻塞
- AWS eventstream 帧解码、账号池串行占锁最小临界区、网络发出即释放
- 强类型 serde 校验，每种协议独立适配器模块
- 多阶段 Docker 构建、非 root 运行（gosu）、多架构镜像、健康检查

---

## 🏗 技术架构

```
                               kiro2api
┌─────────────────────────────────────────────────────────────┐
│                                                             │
│  Client (OpenAI SDK / Anthropic SDK / Gemini SDK / cURL)    │
│       |                                                     │
│  POST /v1/chat/completions        (或 /openai/v1/...)       │
│  POST /v1/messages                (或 /claude/v1/...)       │
│  POST /v1/responses               (或 /openai/v1/...)       │
│  POST /v1beta/models/:m:generateContent (或 /gemini/...)    │
│       |                                                     │
│       v                                                     │
│  +-----------+    +----------------+    +---------------+   │
│  |  Adapters |--->|  Anthropic Hub |--->| Account Pool  |   │
│  |  (4 proto)|    |  (母格式中枢)  |    |  (负载均衡)   |   │
│  +-----------+    +----------------+    +---------------+   │
│                                               |             │
│                                    ┌──────────┼────────┐    │
│                                    v          v        v    │
│                               Account-0  Account-1   ...   │
│                                                             │
│  +-----------+    +----------------+    +---------------+   │
│  |   Auth    |    | Token Refresh  |    | Failure       |   │
│  | Bearer/key|    | Single-flight  |    | Classify+Cool │   │
│  +-----------+    +----------------+    +---------------+   │
│                                                             │
│  +-----------+    +----------------+    +---------------+   │
│  | Endpoint  |    | Usage / Balance|    |  Live Logs    |   │
│  | Fallback  |    |  Stats + Cache |    |   (SSE)       │   │
│  +-----------+    +----------------+    +---------------+   │
│                                                             │
└─────────────────────────────────────────────────────────────┘
                           |
                  Kiro Data Plane (AWS eventstream)
              Kiro IDE → CodeWhisperer → AmazonQ 回退
                           |
                           v
                  generateAssistantResponse
                    (Claude 系模型)
```

---

## 📋 系统要求

| 依赖 | 版本 | 说明 |
|------|------|------|
| Rust | 2024 edition | 仅从源码构建时需要；Docker 部署无需本地安装 |
| Docker | 20.10+ | 推荐使用 Docker 部署 |
| Kiro 账号 | — | 需有效的 Kiro（CodeWhisperer）凭据（Builder ID / IdC / 社交登录） |
| 架构 | amd64 / arm64 | 官方镜像多架构，二选一自动匹配 |

> [!TIP]
> 使用 Docker 部署无需本地安装 Rust 环境，只需 Docker 和有效的 Kiro 凭据即可。

---

## ⚡ 快速部署

> 📖 详细部署文档：[简体中文](docs/zh-CN/DEPLOY.md) | [繁體中文](docs/zh-TW/DEPLOY.md) | [English](docs/en/DEPLOY.md) | [日本語](docs/ja/DEPLOY.md) | [한국어](docs/ko/DEPLOY.md)

> **前置条件**：你需要一份有效的 Kiro（CodeWhisperer）账号凭据。

### 1. 获取 Kiro 凭据

从你的 Kiro 客户端 / 已有 Kiro 凭据中导出以下字段，或使用管理面板的三种交互式登录（Builder ID 设备码 / IAM SSO 授权码 / 社交令牌）现场获取：

| 字段 | 说明 |
|------|------|
| `accessToken` / `refreshToken` | 访问令牌与刷新令牌（到期自动刷新） |
| `expiresAt` | 令牌过期时间（RFC3339） |
| `authMethod` | `social`（带 `profileArn`）或 `idc`（带 `clientId`/`clientSecret`） |

### 2. Docker 部署

```bash
# 克隆仓库
git clone https://github.com/xwteam/kiro2api.git
cd kiro2api

# 创建环境变量文件
cp .env.example .env
```

编辑 `.env`，至少填一个对外调用密钥 `API_KEY`：

```env
API_KEY=sk-你的对外调用密钥
# 管理端独立密钥；公网部署必填（不设则 /api/admin/* 回退用 API_KEY 鉴权，两者都不设即开放）。
# 不需要就把整行注释掉或留空——空值（含仅空白）一律按「未设置」处理，不会覆盖 config.json 里已配的密钥。
ADMIN_API_KEY=sk-你的管理端密钥
```

把 Kiro 账号凭据放到 `data/credentials.json`（数组，可直接 drop-in 现有 Kiro 凭据）：

```json
[
  {
    "id": 12345,
    "accessToken": "...",
    "refreshToken": "...",
    "expiresAt": "2026-07-19T12:00:00Z",
    "authMethod": "social",
    "profileArn": "arn:aws:codewhisperer:us-east-1:...:profile/...",
    "machineId": "..."
  }
]
```

启动服务：

```bash
mkdir -p data
docker compose up -d
```

查看日志确认启动成功：

```bash
docker compose logs -f
# 看到账号池就绪、监听端口即表示启动成功
```

### 3. 验证

```bash
# 健康检查
curl http://localhost:8080/health
# {"service":"kiro2api","status":"ok","version":"0.16.0"}

# 查看可用模型
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-你的API密钥"

# 发送测试请求
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"你好"}]}'
```

看到 AI 回复的文字即部署成功。如果返回 401，请检查 API Key 是否正确。

---

## 🧪 接入示例

> [!NOTE]
> 所有 API 请求都需要携带 API Key。鉴权闸按以下优先级取用凭据：
> - `Authorization: Bearer sk-xxx`（推荐，兼容 OpenAI/Anthropic SDK）
> - `x-api-key: sk-xxx`
> - `x-goog-api-key: sk-xxx`（Gemini 原生头，官方 `google-genai` SDK 默认走这条）
> - query 参数 `?api_key=` > `?token=` > `?key=`（浏览器原生 EventSource 只能用这条）
>
> base URL 用**标准裸前缀**：OpenAI = `{host}/v1`，Anthropic = `{host}`（SDK 自动补 `/v1/messages`），Gemini = `{host}/v1beta`。也可用显式厂商前缀 `/openai/v1`、`/claude/v1`、`/gemini/v1beta`。

> [!IMPORTANT]
> **这些生成参数会被接受但不生效**：Kiro 数据面 wire 没有对应字段，服务既不转发也不报错，请求照常成功——
> - `temperature` / `top_p`：请求结构体里根本没有这两个字段，serde 当未知键直接丢弃。设 `temperature: 0` 不会让输出变确定，也不会有任何提示。
> - `max_tokens` / Gemini `maxOutputTokens`：会被解析进内部结构，但从不下发给上游，**限制不了回复长度**。响应里的 `stop_reason:"max_tokens"` / `finish_reason:"length"` 来自上游自己的预算，与你传的值无关。下面 Anthropic 示例里的 `max_tokens=1024` 只是官方 SDK 的必填参数，保留是为了让 SDK 能把请求发出去。
> - `tool_choice`（OpenAI / Anthropic / Responses）与 Gemini `toolConfig`：**无法强制模型调用指定工具**。唯一被兑现的是 Gemini `toolConfig.functionCallingConfig.mode = "NONE"`——它靠**不下发任何工具规格**来禁用本轮函数调用；`AUTO` 就是默认行为，`ANY`（强制至少调一次）在 wire 上无从表达，按默认处理。

<details>
<summary><b>OpenAI SDK（Python）</b></summary>

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://localhost:8080/v1",
    api_key="sk-你的API密钥",
)

resp = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "Hello"}],
)
print(resp.choices[0].message.content)
```

</details>

<details>
<summary><b>Anthropic SDK（Python）</b></summary>

```python
import anthropic

client = anthropic.Anthropic(
    base_url="http://localhost:8080",
    api_key="sk-你的API密钥",
)

msg = client.messages.create(
    model="claude-sonnet-4.5",
    max_tokens=1024,
    messages=[{"role": "user", "content": "Hello"}],
)
print(msg.content[0].text)
```

</details>

<details>
<summary><b>Gemini SDK（Python）</b></summary>

```python
from google import genai

client = genai.Client(
    api_key="sk-你的API密钥",
    http_options={"base_url": "http://localhost:8080/v1beta"},
)

resp = client.models.generate_content(
    model="claude-sonnet-4.5",
    contents="Hello",
)
print(resp.text)
```

</details>

<details>
<summary><b>cURL</b></summary>

```bash
# 非流式请求
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"Hi"}]}'

# 流式请求
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"Hi"}],"stream":true}'
```

</details>

<details>
<summary><b>函数调用（工具）</b></summary>

```python
resp = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "北京今天天气怎么样"}],
    tools=[{
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "获取指定城市的天气",
            "parameters": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }
        }
    }]
)
```

> 工具调用在四种协议间**真透传**（Anthropic `tool_use` / OpenAI `tool_calls` / Gemini `functionCall`），不做模拟。

</details>

---

## 📡 API 端点

> 📖 详细 API 文档：[简体中文](docs/zh-CN/API.md) | [繁體中文](docs/zh-TW/API.md) | [English](docs/en/API.md) | [日本語](docs/ja/API.md) | [한국어](docs/ko/API.md)

<details>
<summary><b>点击展开完整端点列表</b></summary>

> **双前缀并存**：每协议同时提供「标准裸路径」和「显式厂商前缀路径」。裸路径让官方 SDK 填 `base_url` 时无需加后缀，开箱即用；厂商前缀用于四家明确区分。

### OpenAI 兼容（`/v1` 或 `/openai/v1`）

| 方法 | 端点 | 功能 |
|------|------|------|
| GET | `/models` | 模型列表（固定短清单，不按账号订阅档位过滤） |
| POST | `/chat/completions` | 对话补全（流式返回 `chat.completion.chunk` + `[DONE]`，含工具/图片） |

### OpenAI Responses（`/v1/responses` 或 `/openai/v1/responses`）

| 方法 | 端点 | 功能 |
|------|------|------|
| POST | `/responses` | Responses API（流式为命名事件 + 单调 `sequence_number`，无 `[DONE]`；`previous_response_id` 返回 400） |

### Anthropic 兼容（`/v1` 对话入口；`/claude/v1` 显式前缀）

| 方法 | 端点 | 功能 |
|------|------|------|
| POST | `/v1/messages` | Messages（流式/工具/图片） |
| POST | `/v1/messages/count_tokens` | token 估算 |
| GET | `/claude/v1/models` | 模型列表（Anthropic 形状，固定短清单，不按账号订阅档位过滤；避开与 OpenAI `/v1/models` 冲突） |
| POST | `/claude/v1/messages` · `.../count_tokens` | 显式前缀变体 |

### Gemini 原生（`/v1beta` 或 `/gemini/v1beta`）

| 方法 | 端点 | 功能 |
|------|------|------|
| GET | `/models` | 模型列表（固定短清单，不按账号订阅档位过滤） |
| POST | `/models/{m}:generateContent` | 内容生成（非流式） |
| POST | `/models/{m}:streamGenerateContent` | 流式生成（`?alt=sse`，camelCase） |

### 管理 / 用户 / 运维

| 方法 | 端点 | 功能 |
|------|------|------|
| GET | `/admin` · `/api/admin/*` | 管理面板 + 管理接口（凭 `adminApiKey`，未配置任何 key 时开放：凭据 CRUD / 登录 / API-KEY / 用量 / 日志 / 余额 / 设置 / 检查更新 / 重启） |
| GET | `/user` · `/api/user/*` | 用户面板 + 接口（凭自身 API-KEY） |
| GET | `/health` · `/v1/ping` | 探活（不鉴权） |

</details>

> URL 里的 `localhost:8080` 只是示例；端口由 `PORT`/`config.json` 配置，按你的部署替换。
>
> 鉴权闸接受的通道按优先级为：`Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > query（`?api_key=` > `?token=` > `?key=`）。厂商原生的 `x-goog-api-key` 头与 `?key=` 参数**同样被接受**，官方 `google-genai` SDK 只换 `base_url` 即可直连。要换掉的是**值**：任何通道里传的都必须是**本服务**的 API Key，而不是真正的 Google/OpenAI 厂商密钥。

---

## ⚙ 配置说明

优先级：**命令行参数 > 环境变量 > `config.json` > 内置默认**。命令行只有两个参数：`-c/--config`（配置文件路径）与 `--credentials`（凭据文件路径，不给则由 `CREDENTIALS_PATH`/`config.json`/默认值决定）。挂载卷 `./data` 存放 `config.json`、`credentials.json`、日志与运行态。

> 凭据路径同时决定用量统计（`stats/`）、API-KEY 存储（`api_keys.json`）与余额缓存的落盘目录——它们都取 `credentials.json` 的父目录。内置默认值解析在 `-c` 指定的配置文件所在目录下，容器以 `-c /app/data/config.json` 启动，因此默认落点就是 `/app/data/credentials.json`，这些数据默认就落在挂载卷里；自定义路径时请一并指向挂载卷，否则容器重建即丢。

**环境变量**（见 `.env.example`）：

| 变量 | 必填 | 默认值 | 说明 |
|------|------|--------|------|
| `API_KEY` | ✅ | — | 对外调用密钥（留空且未创建任何 API-KEY 时协议端点开放访问，启动告警） |
| `ADMIN_API_KEY` | ❌ | 回退 `API_KEY` | 管理端独立鉴权 key；与 `API_KEY` 都不设时 `/api/admin/*` 开放，公网部署必填 |
| `HOST` | ❌ | `127.0.0.1`（镜像内置 `0.0.0.0`） | 监听地址 |
| `PORT` | ❌ | `8080` | 服务端口（compose 的端口映射与健康检查都跟随该值） |
| `REGION` | ❌ | `us-east-1` | 仅存进配置本身、供 `GET /api/admin/config` 展示（`config.json` 的 `region` 同理），**不参与上游调用**：数据面与令牌刷新的 region 依次取账号 `profileArn` 内的 region → 账号自身 `region` 字段 → 硬编码 `us-east-1` |
| `LOAD_BALANCING_MODE` | ❌ | `priority` | 负载均衡：`priority`（等权轮询）/ `balanced`（按 weight 加权） |
| `MAX_RPM_PER_CREDENTIAL` | ❌ | `0` | 每账号每分钟请求上限，`0` = 无限。触顶只会让该账号在窗口内**不被选中**，不会给客户端回 `429`；所有账号都不可选时请求以 `503` 结束（`429` 只来自上游限流） |
| `CREDENTIALS_PATH` | ❌ | `credentials.json`，解析在 `-c` 配置文件所在目录（容器内即 `/app/data/credentials.json`） | 凭据文件路径；被命令行 `--credentials` 覆盖 |

**`data/config.json`**（camelCase，均可选；`logCapacity` 仅在此配置）：

```json
{
  "host": "0.0.0.0",
  "port": 8080,
  "region": "us-east-1",
  "apiKey": "sk-你的对外调用密钥",
  "adminApiKey": "可选,管理端",
  "credentialsPath": "/app/data/credentials.json",
  "loadBalancingMode": "priority",
  "maxRpmPerCredential": 0,
  "logCapacity": 5000,
  "kiroVersion": "0.11.107",
  "systemVersion": "win32#10.0.22631",
  "nodeVersion": "22.22.0"
}
```

- `logCapacity`：实时日志环形缓冲条数，`>0` 启用日志捕获（管理面板日志页回放/SSE），`0` 关闭（日志端点返回 503）；默认 `5000`。
- `kiroVersion`/`systemVersion`/`nodeVersion`：伪装 UA 版本号，从配置注入。

---

## ⚠ 注意事项

1. **对外部署务必设置 `API_KEY` 与 `ADMIN_API_KEY`**：`API_KEY` 留空且尚未创建任何 API-KEY 时，协议端点开放访问（启动会告警），发出第一条 API-KEY 后协议闸即收口；`adminApiKey`/`apiKey` 都不配时 `/api/admin/*` 同样开放（发多少条 API-KEY 都不会收口），凭据、API-KEY、鉴权设置都能被任意改写。`/admin`、`/user` 面板本体始终不鉴权（真正的闸在其 `/api/**` 接口上）；裸机部署慎改 `HOST=0.0.0.0`。

2. **可用模型取决于账号订阅档位**：免费档（KIRO FREE）通常只授权 `claude-sonnet-4.5`；请求不支持的模型返回 `400`（`INVALID_MODEL_ID`），不瞎重试、不误伤账号。

3. **令牌自愈**：token 到期自动内存刷新并原子落盘 `credentials.json`；真正的凭据失效才永久禁用，配额/风控/限流一律冷却自愈。

4. **流式输出**：四种协议均支持流式；`stream:false` 时服务内部仍解码事件流，收集完毕后一次性返回完整 JSON。上游报错或流传输中途中断时，流会以该协议自身的错误事件收尾，绝不会伪装成一次正常完成；上游命中**它自己的**输出预算（`ContentLengthExceededException`）或上下文窗口耗尽时，如实回报截断原因（`stop_reason:"max_tokens"` / `finish_reason:"length"`、`model_context_window_exceeded`）。注意请求里的 `max_tokens` 并不参与其中——它不会下发到上游，见[接入示例](#-接入示例)里的参数说明。

5. **网络环境**：部署服务器需能访问 AWS CodeWhisperer/Kiro 端点（`*.amazonaws.com`）。

---

## 🗂 项目结构

```
kiro2api/
├── src/
│   ├── main.rs / cli.rs / lib.rs   # 入口、CLI、库根
│   ├── config.rs                   # 配置（env > config.json > 默认）
│   ├── http.rs                     # 出站 HTTP 客户端（超时硬顶）
│   ├── logcap.rs                   # 实时日志环形缓冲 + SSE 广播
│   ├── server/                     # axum 路由装配、统一鉴权闸
│   ├── protocol/                   # 四协议适配器
│   │   ├── anthropic/              #   Anthropic Messages（中枢母格式 + relay 内核）
│   │   ├── openai/                 #   OpenAI Chat Completions
│   │   ├── responses/              #   OpenAI Responses
│   │   └── gemini/                 #   Gemini 原生 v1beta
│   ├── kiro/                       # Kiro 数据面
│   │   ├── pool.rs                 #   账号池（负载均衡 + 失败分类 + 冷却）
│   │   ├── provider.rs             #   上游发送 + 端点回退
│   │   ├── convert.rs              #   模型映射 + 请求/响应转换
│   │   ├── ensure_fresh.rs / refresh.rs  # 令牌单飞刷新
│   │   ├── eventstream/            #   AWS eventstream 帧解码
│   │   └── login/                  #   Builder ID / IAM SSO / 社交登录流
│   ├── admin/                      # /api/admin/* 管理接口
│   ├── user/                       # /api/user/* 用户接口
│   ├── apikey/                     # API-KEY 存储与校验
│   ├── balance/                    # 余额缓存（TTL）
│   ├── stats/                      # 用量/失败/限流统计 + 定价
│   ├── models_cache/               # 动态模型清单缓存
│   └── webui/                      # rust-embed 静态面板服务（admin-ui-v2/、user-ui/dist）
├── admin-ui-v2/                    # 静态管理面板（HTML/CSS/JS，编译期嵌入）
├── user-ui/                        # 用户面板（构建产物嵌入）
├── data/                           # 持久化数据（Docker 卷挂载）
│   ├── config.json                 #   运行配置
│   └── credentials.json            #   Kiro 账号凭据
├── docs/                           # 5 语言文档（README/USAGE/DEPLOY/API/SPONSORS）
├── Dockerfile                      # 多阶段构建（多架构、非 root）
├── docker-compose.yml              # 编排配置
├── Cargo.toml / Cargo.lock
└── .env.example
```

---

## 🗺 开发路线

- [x] 四协议前端（OpenAI / Anthropic / OpenAI-Responses / Gemini）
- [x] Anthropic Messages 中枢母格式 + 统一中转内核
- [x] 流式（SSE）+ 函数调用真透传 + 图片多模态
- [x] Kiro 账号池（多账号轮询、分级冷却、负载均衡）
- [x] 令牌单飞自动刷新 + 原子落盘
- [x] 端点回退（Kiro/CodeWhisperer/AmazonQ）+ 跨账号重试
- [x] body-aware 失败分类（永久失效才禁用，其余冷却自愈）
- [x] 统一鉴权闸（Bearer / x-api-key / x-goog-api-key / query 参数）
- [x] Web 管理面板（凭据/登录/API-KEY/用量/日志/余额/设置）
- [x] 用户面板（持有者用自身 API-KEY 登录）
- [x] 三种交互式登录流（Builder ID / IAM SSO / 社交令牌）
- [x] 每日/账号用量统计（含客户端 IP 与账号标签）
- [x] 实时日志（SSE）+ 余额缓存 + 动态模型清单
- [x] 集成示例（协议×语言可复制片段）
- [x] 服务重启 + 版本检查更新（GitHub Release 比对）
- [x] Docker 多架构（amd64/arm64）交付 + CI
- [ ] `/admin`、`/user` 面板本体鉴权
- [ ] GitHub Actions 自动构建并发布镜像

---

## ☕ 赞赏 & 共享

> 完整内容请查看 [SPONSORS.md](SPONSORS.md)

觉得有帮助？请作者喝杯咖啡，或加入微信交流群获取使用帮助。二维码见管理面板仪表盘。

kiro2api 主要由个人维护，欢迎通过代码、文档、修复或 PR 参与建设。

**参与贡献：**

1. Fork 本仓库
2. 创建分支 `git checkout -b feature/your-feature`
3. 提交代码 `git commit -m "feat: add something"`
4. 推送并创建 Pull Request

---

## 🙏 致谢

感谢所有在 [Issues](https://github.com/xwteam/kiro2api/issues) 里提交 bug 复现、日志、兼容性反馈和功能建议的用户。这些反馈直接推动了账号池、令牌自愈、端点回退、多协议兼容、Web 面板等核心能力的迭代。

---

## ⭐ Star History

<a href="https://star-history.com/#xwteam/kiro2api&Date">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=xwteam/kiro2api&type=date&theme=dark&legend=top-left" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=xwteam/kiro2api&type=date&legend=top-left" />
   <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=xwteam/kiro2api&type=date&legend=top-left" />
 </picture>
</a>

---

## 📄 许可协议

本项目采用 [MIT 许可](LICENSE)：

- **允许**：个人学习、研究、自用部署、二次开发
- **要求**：保留版权与许可声明

本项目与 Amazon / AWS / Kiro 无关联。使用者需自行承担风险并遵守相关服务条款。

---

## ⚠ 免责声明

1. **技术性质**：kiro2api 是一个技术研究项目，把 Kiro（CodeWhisperer）后端封装为多协议兼容 API。本项目不提供任何 AI 服务，所有生成内容均来自上游。使用本项目可能违反相关服务条款，由此产生的一切后果由使用者自行承担。

2. **无担保声明**：本项目按"原样"提供，不作任何明示或暗示的保证，包括但不限于适销性、特定用途适用性。开发者不对因使用本项目导致的账号封禁、数据丢失或其他任何损失承担责任。

3. **数据与隐私**：本项目完全在用户本地环境运行，不收集、不上传、不存储任何用户数据。你的凭据和 API Key 仅保存在本地配置中，请妥善保管，切勿泄露。

4. **合规责任**：使用者应确保其使用行为符合所在地区的法律法规。严禁将本项目用于任何违法违规活动。

5. **第三方服务**：本项目与 Amazon / AWS / Kiro 无任何关联或授权关系。上游服务的可用性、稳定性及内容准确性均由其提供方负责，与本项目无关。

---

<div align="center">
  <sub>Built with Rust + axum + tokio | Powered by Kiro (CodeWhisperer)</sub>
</div>
