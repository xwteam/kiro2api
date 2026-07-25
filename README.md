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
  <img src="https://img.shields.io/badge/version-v0.1.0-success?style=flat-square" alt="Version">
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
> `apiKey`/`API_KEY` 为空时，协议端点**开放访问**（启动会告警）。对外部署务必设置。容器镜像已内置 `HOST=0.0.0.0`；裸机部署请勿轻易把 `HOST` 改成 `0.0.0.0`（当前 `/admin`、`/user` 面板本体尚未接鉴权，受保护的是 `/api/admin/*`、`/api/user/*` 接口）。

> [!TIP]
> 后端为 Kiro（CodeWhisperer）账号池。**可用模型取决于账号订阅档位**：免费档（KIRO FREE）通常只授权 `claude-sonnet-4.5`，opus/GPT 等需更高档位——请求不支持的模型会明确返回 400（`INVALID_MODEL_ID`），而非静默失败。

---

## 📝 最近更新

> 完整更新日志请查看 [CHANGELOG.md](CHANGELOG.md)。

| 日期 | 更新内容 |
|------|----------|
| 2026-07-25 | v0.1.0 - 🚀 首个版本：四协议前端（Anthropic 中枢 + OpenAI / OpenAI-Responses / Gemini）、Kiro 账号池（多账号轮询 / 分级冷却 / 令牌自愈）、端点回退与跨账号重试、统一鉴权闸、`/admin` 管理面板与 `/user` 用户面板、每日/账号用量统计、失败/限流日志、账号余额缓存、实时日志（SSE）、三种交互式登录流、Docker 多架构（amd64/arm64）交付与 CI |

---

## 🌟 核心功能

> 📖 详细使用文档：[简体中文](docs/zh-CN/USAGE.md) | [繁體中文](docs/zh-TW/USAGE.md) | [English](docs/en/USAGE.md) | [日本語](docs/ja/USAGE.md) | [한국어](docs/ko/USAGE.md)

### 🔌 四协议前端，一套后端

- 一个服务同时提供 **OpenAI Chat**、**Anthropic Messages**、**OpenAI Responses**、**Gemini 原生** 四种 SDK 格式
- 内部以 **Anthropic Messages 为中枢母格式**，其余协议双向转换后复用同一条中转内核
- 每个协议都支持**流式（SSE）**、**函数调用（工具）真透传**、**图片输入（多模态）**
- **双前缀挂载**：每协议同时挂标准裸前缀与显式厂商前缀（`/openai/v1`、`/claude/v1`、`/gemini/v1beta`），主流 SDK 填 `base_url` 即插即用

### 🔐 统一鉴权闸

- 三选一：`Authorization: Bearer` / `x-api-key` / `?token=`，常量时间比较，失败即 `401`
- `adminApiKey`（缺省回退 `apiKey`）保护 `/api/admin/*`；持有者用自己的 **API-KEY** 访问 `/api/user/*`
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
- **仪表盘**：运行时间实时计时、全局剩余积分、系统信息（版本/Rust/OS/内存/CPU/PID/运行模式）、赞助二维码卡（实时拉取远程配置）、**检查更新**（GitHub Release 比对）
- **账号管理**：增删改查、三种交互式登录、批量导入、优先级/权重、余额查询
- **API-KEY 管理**：发放/禁用/改标签、按 key 用量与分页记录
- **用量统计**：每日/账号维度、含客户端 IP 与账号标签、按日下钻
- **实时日志**：结构化表格 + 方向过滤 + 搜索 + 分页 + SSE 实时推送 + 下载
- **设置**：运行期切负载均衡/鉴权密钥、集成示例（协议×语言可复制片段）、**一键重启服务**
- 顶部控制栏：运行状态徽章、GitHub、重启、深浅色主题、5 语言切换

### 👤 用户面板

- 内置用户台（`/user`），持有者用自己的 **API-KEY** 登录（无需 admin 权限）
- 查看该 key 的额度、累计用量与分页记录，由 `/api/user/*` 驱动

### 🧭 模型名映射

- 客户端传入的模型名按**小写子串**匹配到 Kiro 内部模型（未匹配到 → `400`）
- `/models` 端点返回本服务实际可服务的模型 id，建议客户端 list-then-use

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
ADMIN_API_KEY=可选,管理端独立密钥（留空回退用 API_KEY）
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
# {"service":"kiro2api","status":"ok","version":"0.1.0"}

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
> 所有 API 请求都需要携带 API Key。支持两种方式：
> - `Authorization: Bearer sk-xxx`（推荐，兼容 OpenAI/Anthropic SDK）
> - `x-api-key: sk-xxx`
>
> base URL 用**标准裸前缀**：OpenAI = `{host}/v1`，Anthropic = `{host}`（SDK 自动补 `/v1/messages`），Gemini = `{host}/v1beta`。也可用显式厂商前缀 `/openai/v1`、`/claude/v1`、`/gemini/v1beta`。

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
| GET | `/models` | 可用模型列表 |
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
| GET | `/claude/v1/models` | 模型列表（Anthropic 形状，避开与 OpenAI `/v1/models` 冲突） |
| POST | `/claude/v1/messages` · `.../count_tokens` | 显式前缀变体 |

### Gemini 原生（`/v1beta` 或 `/gemini/v1beta`）

| 方法 | 端点 | 功能 |
|------|------|------|
| GET | `/models` | 模型列表 |
| POST | `/models/{m}:generateContent` | 内容生成（非流式） |
| POST | `/models/{m}:streamGenerateContent` | 流式生成（`?alt=sse`，camelCase） |

### 管理 / 用户 / 运维

| 方法 | 端点 | 功能 |
|------|------|------|
| GET | `/admin` · `/api/admin/*` | 管理面板 + 管理接口（凭 `adminApiKey`：凭据 CRUD / 登录 / API-KEY / 用量 / 日志 / 余额 / 设置 / 检查更新 / 重启） |
| GET | `/user` · `/api/user/*` | 用户面板 + 接口（凭自身 API-KEY） |
| GET | `/health` · `/v1/ping` | 探活（不鉴权） |

</details>

> URL 里的 `localhost:8080` 只是示例；端口由 `PORT`/`config.json` 配置，按你的部署替换。
>
> Gemini/OpenAI 客户端一律用本服务的**统一鉴权**（Bearer/`x-api-key`/`?token=`），不是厂商原生的 `?key=`/`x-goog-api-key`。

---

## ⚙ 配置说明

优先级：**环境变量 > `config.json` > 内置默认**。挂载卷 `./data` 存放 `config.json`、`credentials.json`、日志与运行态。

**环境变量**（见 `.env.example`）：

| 变量 | 必填 | 默认值 | 说明 |
|------|------|--------|------|
| `API_KEY` | ✅ | — | 对外调用密钥（留空则协议端点开放访问，启动告警） |
| `ADMIN_API_KEY` | ❌ | 回退 `API_KEY` | 管理端独立鉴权 key |
| `HOST` | ❌ | `127.0.0.1`（镜像内置 `0.0.0.0`） | 监听地址 |
| `PORT` | ❌ | `8080` | 服务端口 |
| `REGION` | ❌ | `us-east-1` | 默认 AWS region（账号 `profileArn` 内的 region 优先） |
| `LOAD_BALANCING_MODE` | ❌ | `priority` | 负载均衡：`priority`（等权轮询）/ `balanced`（按 weight 加权） |
| `MAX_RPM_PER_CREDENTIAL` | ❌ | `0` | 每账号每分钟请求上限，`0` = 无限 |
| `CREDENTIALS_PATH` | ❌ | `/app/data/credentials.json` | 凭据文件路径 |

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
  "logCapacity": 1000,
  "kiroVersion": "0.11.107",
  "systemVersion": "win32#10.0.22631",
  "nodeVersion": "22.22.0"
}
```

- `logCapacity`：实时日志环形缓冲条数，`>0` 启用日志捕获（管理面板日志页回放/SSE），`0` 关闭（日志端点返回 503）；默认 `1000`。
- `kiroVersion`/`systemVersion`/`nodeVersion`：伪装 UA 版本号，从配置注入。

---

## ⚠ 注意事项

1. **对外部署务必设置 `API_KEY`**：留空时协议端点开放访问（启动会告警）。`/admin`、`/user` 面板本体尚未接鉴权，受保护的是 `/api/admin/*`、`/api/user/*`；裸机部署慎改 `HOST=0.0.0.0`。

2. **可用模型取决于账号订阅档位**：免费档（KIRO FREE）通常只授权 `claude-sonnet-4.5`；请求不支持的模型返回 `400`（`INVALID_MODEL_ID`），不瞎重试、不误伤账号。

3. **令牌自愈**：token 到期自动内存刷新并原子落盘 `credentials.json`；真正的凭据失效才永久禁用，配额/风控/限流一律冷却自愈。

4. **流式输出**：四种协议均支持流式；`stream:false` 时服务内部仍解码事件流，收集完毕后一次性返回完整 JSON。

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
- [x] 统一鉴权闸（Bearer / x-api-key / ?token=）
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
