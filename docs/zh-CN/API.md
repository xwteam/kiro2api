# API 文档

本文档详细说明 kiro2api 的所有 API 端点和使用方法。kiro2api 以 **Anthropic Messages 为中枢母格式**，一套后端（Kiro / CodeWhisperer 账号池）同时对外提供 OpenAI Chat、Anthropic Messages、OpenAI Responses、Gemini 原生四种协议。

## 认证

所有协议端点走**统一鉴权**。密钥可走下列**六条通道**中的任意一条，服务端按固定优先级取第一条命中的：`Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > `?api_key=` > `?token=` > `?key=`。

### 方式 1：Authorization Header（推荐）

```bash
curl -H "Authorization: Bearer sk-你的API密钥" \
  http://localhost:8080/v1/models
```

### 方式 2：x-api-key Header

```bash
curl -H "x-api-key: sk-你的API密钥" \
  http://localhost:8080/v1/models
```

### 方式 3：x-goog-api-key Header

Gemini 生态的标准头（官方 `google-genai` SDK 默认只发这一条），本服务同样接受：

```bash
curl -H "x-goog-api-key: sk-你的API密钥" \
  http://localhost:8080/v1beta/models
```

### 方式 4：查询参数

无法设置请求头的场景（如浏览器 `EventSource`）可把密钥放进 URL query。三个参数名都认，优先级 `api_key` > `token` > `key`（`?key=` 是 Gemini 官方 SDK 的默认写法）：

```bash
curl "http://localhost:8080/v1/models?api_key=sk-你的API密钥"
curl "http://localhost:8080/v1/models?token=sk-你的API密钥"
curl "http://localhost:8080/v1beta/models?key=sk-你的API密钥"
```

### 获取 API Key

API Key 由部署时的 `API_KEY` 环境变量（或 `config.json` 的 `apiKey`）决定，可通过以下方式查看：

```bash
# 查看 .env 文件
cat .env | grep API_KEY

# 或查看运行配置
cat data/config.json | grep apiKey
```

> [!IMPORTANT]
> `apiKey`/`API_KEY` 为空时，协议端点**开放访问**（启动会告警）。对外部署务必设置。密钥比较为**常量时间**，失败即 `401`。`/health`、`/v1/ping` 等探活端点不鉴权。

## 路径说明

每个协议同时挂载两套路径：

### 标准裸路径（主流 SDK 开箱即用）

主流 SDK 填写 `base_url` 时无需添加后缀，直接使用标准路径：

**OpenAI 格式**：
- `/v1/chat/completions`
- `/v1/models`
- `/v1/responses`

**Claude 格式**：
- `/v1/messages`
- `/v1/messages/count_tokens`

**Gemini 格式**：
- `/v1beta/models/{model}:generateContent`
- `/v1beta/models/{model}:streamGenerateContent`
- `/v1beta/models`

### 带前缀路径（四家明确区分）

- OpenAI: `/openai/v1/chat/completions`、`/openai/v1/responses`、`/openai/v1/models`
- Claude: `/claude/v1/messages`、`/claude/v1/messages/count_tokens`、`/claude/v1/models`
- Gemini: `/gemini/v1beta/models/{model}:generateContent`、`:streamGenerateContent`、`/gemini/v1beta/models`

**重要说明**：裸路径 `/v1/models` 返回 OpenAI 格式的模型列表（同一路径无法同时返回两种格式）。如需 Anthropic 形状的模型列表，请使用 `/claude/v1/models`。

## 错误响应

错误体**随协议而异**，尽量贴合各厂商 SDK 的原生形状：

- **Anthropic 形状**：`{"type":"error","error":{"type":"...","message":"..."}}`
- **OpenAI / Responses 形状**：`{"error":{"message":"...","type":"...","code":"..."}}`
- **Gemini 形状**：`{"error":{"code":...,"message":"...","status":"..."}}`

> [!IMPORTANT]
> **例外：鉴权闸返回的 `401` / `402` 恒为 Anthropic 形状。** 这两种响应由鉴权中间件在请求进入协议 handler **之前**产生，四条协议路由共用同一个闸，所以 OpenAI / Responses / Gemini 客户端在 key 无效或超额时收到的同样是 `{"type":"error","error":{"type":"authentication_error"|"billing_error","message":"..."}}`，而不是本协议的原生形状——按状态码判断，别按错误体形状判断。（`/api/user/*` 的 `401` 由 handler 自己产生，形状是 `{"error":"…"}`，见「用户 API」。）

### 常见错误码

| 状态码 | 说明 |
|--------|------|
| 400 | 两类不同成因、都不重试也不误伤账号：①**模型名在本网关本地映射不上**——体为 `无法识别的模型名: <你传的名字>`，类型 `invalid_request_error`，**不含** `INVALID_MODEL_ID` 字样；②**上游确定性拒绝该模型**（账号档位无权），上游 reason 码即 `INVALID_MODEL_ID`，原样转出。另外，四种协议的对话端点在请求体解析失败时也回 `400`（已改写成各自 SDK 认得的错误形状） |
| 401 | 认证失败，API Key 无效或缺失（已配置 `apiKey` 时）；管理端创建的 API-KEY 被停用 / 过期同样是 `401`。协议路由与 `/api/admin/*` 的 `401` 体恒为 Anthropic 形状的 `authentication_error`（见上方例外说明）；`/api/user/*` 的 `401` 则是 `{"error":"…"}` |
| 402 | 该 API-KEY 的**消费额度已达或超上限**（体恒为 Anthropic 形状的 `billing_error`，见上方例外说明）。仅对管理端创建、且设了 `spendingLimit` 的 key 生效；用全局 `apiKey` 调用不会命中 |
| 404 | 管理端点的 `{id}` 不存在：账号回 `{"error":"account not found","id":…}`，API-KEY 回 `{"error":"api key not found","id":…}`，登录会话回 `{"success":false,"error":"login session not found or expired"}` |
| 422 | 请求体反序列化失败（缺必填字段 / 类型不符），axum 默认拒收，`text/plain` 纯文本。出现在带 body 提取器的端点上：`/api/admin/*` 与 `POST /api/user/login`（四个协议对话端点与 `/v1/messages/count_tokens` 已自行接管拒收、改回各自形状的 `400`）（`/api/user/*` 的其余端点只收 query，参数类型不符是 `400` 而非 `422`） |
| 429 | **只来自上游限流**，且只在上游于 HTTP 200 事件流中途下发 `Throttling*` 异常帧时原样映射。本地的 `MAX_RPM_PER_CREDENTIAL` **不会**产生 `429`：超过该上限只是让这个账号本轮选不中，全部账号都选不中才落到 `503`（见下行）。上游在 HTTP 层直接回的 `429` 也**不**原样透出——那会被归为配额失败、冷却该账号并换号重试，用尽后以 `502` 收尾。据自己设的 RPM 上限去监控 `429` 是等不到的 |
| 502 | 上游 Kiro 失败（含跨账号重试用尽后的最后一个账号级错误） |
| 503 | 无可用账号（全部冷却 / 禁用 / **超本地 RPM 上限**），或日志端点未启用（`logCapacity=0`） |

## 模型名映射

客户端传入的模型名按**小写子串**匹配到 Kiro 内部模型，一个都匹配不到则本网关直接返回 `400`，体为 `无法识别的模型名: <你传的名字>`（这一步是本地映射失败，**不会**带 `INVALID_MODEL_ID`——那个码只在名字映射成功、但上游按账号档位拒绝时才由上游给出）：

| 传入含 | 解析为 |
|--------|--------|
| `sonnet`（+`4.6`/`sonnet-5`） | `claude-sonnet-4.5`（/`-4.6`/`-5`） |
| `opus`（+`4.5`/`4.7`/`4.8`） | `claude-opus-4.6`（/对应） |
| `haiku` / `fable` | `claude-haiku-4.5` / `claude-fable-5` |
| `deepseek` / `glm` / `qwen` | `deepseek-3.2` / `glm-5` / `qwen3-coder-next` |
| `minimax`（+`2.5`） | `minimax-m2.1`（/`-m2.5`） |
| `gpt`+`terra`/`luna`/`sol`/`5.6` | `gpt-5.6-terra`/`-luna`/`-sol` |
| `auto` | `auto` |

三个协议的 `/models` 端点（`GET /v1/models`、`GET /claude/v1/models`、`GET /v1beta/models`）返回的是**同一份编译期目录**（共 17 条，顺序一致，只是各自换成本协议的条目形状）。目录内容与上表**一一对应**——列出来的每个 id 都是本地映射认得的名字，唯一不在列表里的可用名是路由别名 `auto`。因此"先列模型、再拿列出的 id 发请求"这条客户端标准流程在本服务上成立，不会列出一个调用即 `400` 的名字。目录全量（即三个端点的返回顺序）：

`claude-sonnet-4.5`、`claude-sonnet-4.6`、`claude-sonnet-5`、`claude-opus-4.5`、`claude-opus-4.6`、`claude-opus-4.7`、`claude-opus-4.8`、`claude-haiku-4.5`、`claude-fable-5`、`deepseek-3.2`、`glm-5`、`qwen3-coder-next`、`minimax-m2.1`、`minimax-m2.5`、`gpt-5.6-terra`、`gpt-5.6-luna`、`gpt-5.6-sol`。

> [!TIP]
> **可用模型取决于账号订阅档位**：免费档（KIRO FREE）通常只授权 `claude-sonnet-4.5`，opus/GPT 等需更高档位。协议侧的 `/models` 是**编译期常量**，**不读账号池、也不打上游、更不按订阅档位过滤**，所以它答的是"本网关认得哪些模型名"，不是"你的账号能用哪些"：列表里的模型若不在你账号的授权范围内，请求照样返回 `400`（上游 reason 码 `INVALID_MODEL_ID`）。要看**账号实际授权**的集合，用 `GET /api/admin/models`——它在上游模型并集缓存命中时返回各账号 `ListAvailableModels` 的并集（缓存为空时回落到同一份 17 条目录）。

## OpenAI 兼容 API

### GET /v1/models

获取模型列表——本网关的**完整目录全量**（17 条），编译期常量，不读账号池、不打上游，故可用性仍受账号档位限制（详见上文提示）。也可使用带前缀路径 `/openai/v1/models`。

**请求**：
```bash
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-你的API密钥"
```

**响应**：下面就是**全部** 17 条与它们的固定顺序。每条的 `created` 恒为常量 `1700000000`、`owned_by` 恒为 `"kiro2api"`（占位值，既不是真实发布时间也不是真实归属方）：
```json
{
  "object": "list",
  "data": [
    {"id": "claude-sonnet-4.5", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "claude-sonnet-4.6", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "claude-sonnet-5", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "claude-opus-4.5", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "claude-opus-4.6", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "claude-opus-4.7", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "claude-opus-4.8", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "claude-haiku-4.5", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "claude-fable-5", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "deepseek-3.2", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "glm-5", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "qwen3-coder-next", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "minimax-m2.1", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "minimax-m2.5", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "gpt-5.6-terra", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "gpt-5.6-luna", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "gpt-5.6-sol", "object": "model", "created": 1700000000, "owned_by": "kiro2api"}
  ]
}
```

### POST /v1/chat/completions

发送对话请求，获取 AI 回复。也可使用带前缀路径 `/openai/v1/chat/completions`。

**请求体**：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `model` | string | 是 | 模型名称，如 `claude-sonnet-4.5` |
| `messages` | array | 是 | 消息列表，每条消息含 `role` 和 `content`。`content` 可以是字符串或对象数组（支持多模态） |
| `stream` | boolean | 否 | 是否流式返回，默认 false |
| `max_tokens` | number | 否 | **接受但不生效**（见下方说明），不会截断回复长度 |
| `tools` | array | 否 | 函数调用工具列表 |
| `tool_choice` | string 或 object | 否 | **接受但不生效**（见下方说明），无法强制/禁止调用某个工具 |

> [!IMPORTANT]
> **生成参数不生效，请勿依赖。** Kiro 数据面的线格式里没有对应字段，本服务**故意不转发、也不改写成提示词伪装成已生效**：
> - `max_tokens`（Anthropic 同名字段、Responses 的 `max_output_tokens`、Gemini 的 `generationConfig.maxOutputTokens` 都折进同一个字段）会被解析、然后丢弃，**不会**限制回复长度。你看到的 `finish_reason:"length"` / `stop_reason:"max_tokens"` 是**上游自己**命中输出预算后回的截断信号，与你传的值无关。
> - `tool_choice` 同理，仅带进内部请求便不再被读取，`auto`/`required`/`none`/指定函数名一律无效果。四种协议里**只有** Gemini 的 `toolConfig.functionCallingConfig.mode:"NONE"` 能被兑现——办法是这一轮干脆不下发工具规格；`AUTO` 等同默认行为，`ANY` 在线格式上无从表达，按默认走。
> - `temperature` / `top_p` 等采样参数**连字段都不存在**，serde 当作未知键静默丢弃：既不生效，也**不会**报错。设 `temperature=0` 求确定性输出是拿不到的。

**多模态 content 格式**：

`content` 可以是字符串（纯文本）或对象数组（支持文本和图片）：

```json
{
  "role": "user",
  "content": [
    {"type": "text", "text": "这是什么"},
    {
      "type": "image_url",
      "image_url": {
        "url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
      }
    }
  ]
}
```

支持的 content 类型：
- `text`：纯文本内容
- `image_url`：图片，支持 Base64 Data URI（`data:image/...;base64,...`）

`role:"tool"` 的消息用于回传工具执行结果。

**非流式请求示例**：

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {"role": "user", "content": "你好"}
    ]
  }'
```

**非流式响应**：

```json
{
  "id": "chatcmpl-xxx",
  "object": "chat.completion",
  "created": 1715970000,
  "model": "claude-sonnet-4.5",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "你好！有什么我可以帮助你的吗？"
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 10,
    "completion_tokens": 20,
    "total_tokens": 30
  }
}
```

> 命中工具时，`message.tool_calls` 承载调用，`finish_reason` 为 `tool_calls`。

**流式请求示例**：

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {"role": "user", "content": "写一首诗"}
    ],
    "stream": true
  }'
```

**流式响应**（Server-Sent Events 格式）：首帧带 `delta.role`，末帧带 `finish_reason`，以 `data: [DONE]` 收尾：

```
data: {"choices":[{"delta":{"role":"assistant"},"index":0}]}

data: {"choices":[{"delta":{"content":"春"},"index":0}]}

data: {"choices":[{"delta":{"content":"风"},"index":0}]}

data: [DONE]
```

**函数调用示例**：

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {"role": "user", "content": "北京今天天气怎么样"}
    ],
    "tools": [
      {
        "type": "function",
        "function": {
          "name": "get_weather",
          "description": "获取指定城市的天气",
          "parameters": {
            "type": "object",
            "properties": {
              "city": {"type": "string", "description": "城市名称"}
            },
            "required": ["city"]
          }
        }
      }
    ]
  }'
```

> [!TIP]
> 工具调用在四种协议间**真透传**（Anthropic `tool_use` / OpenAI `tool_calls` / Gemini `functionCall`），不做模拟。

### POST /v1/responses

OpenAI Responses API。为需要新版 Responses 协议（而非 Chat Completions）的客户端提供支持——例如 **Codex CLI**。支持文本对话、流式输出、工具（函数）调用。也可使用带前缀路径 `/openai/v1/responses`。

**请求**：
```bash
curl -X POST http://localhost:8080/v1/responses \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "model": "claude-sonnet-4.5",
    "input": "1+1等于几？",
    "stream": false
  }'
```

**请求体**：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `model` | string | 是 | 模型名称，如 `claude-sonnet-4.5` |
| `input` | string 或 array | 是 | 字符串（等同一条 user 消息的简写），或输入条目数组（见下） |
| `instructions` | string | 否 | 系统/开发者前置说明，转换为 system，加在对话最前面 |
| `stream` | boolean | 否 | 是否流式返回，默认 false |
| `tools` | array | 否 | 函数调用工具定义 |
| `max_output_tokens` | number | 否 | **接受但不生效**，同 `max_tokens`（见「OpenAI 兼容 API」下的生成参数说明） |
| `tool_choice` | string 或 object | 否 | **接受但不生效**，同上 |

**`input` 数组条目类型**——能映射到中枢的**只有下面三种**（`type` 缺省时按带 `role` 视为 `message`）。其它 `type` 值（`reasoning`、`local_shell_call` 等 Responses 侧产物）**整条跳过，不影响其余条目**，不再拒绝整个请求：客户端做多轮时会把上一轮的**整个** `output` 原样回灌，里面必然带这类条目，判成错误的后果是第一轮能通、第二轮必炸（v0.7.1 修正）：
> ⚠️ **工具数组里的内置工具会被丢弃。** 照 OpenAI 规范，`tools` 里除了 `type:"function"`，还可以有 `web_search`、`local_shell`、`file_search` 等**内置工具**——它们由 OpenAI 服务端自己执行，**照规范就没有 `name` 字段**。本服务的中枢没有等价物，无法代为执行，故**解析后丢弃并记一条 WARN**（`responses_builtin_tool_dropped`，带 `tool_type`），不会因此拒绝整个请求（v0.7.1 之前会返回 `400 tools[N]: missing field name`，一个内置工具就废掉整轮对话）。**后果**：模型不具备该内置能力（如联网搜索）。带 `name` 的 `function` / `custom` 工具照常生效；`parameters` 可省略，缺省按空对象 schema 处理。

- `{"type":"message","role":"user"|"assistant"|"system","content":[...]}` —— 内容块：`{"type":"input_text","text":...}`、`{"type":"input_image","image_url":"..."}`、`{"type":"output_text","text":...}`。`input_image` 的 `image_url` **只支持 `data:` Base64 URI**；远程 http(s) URL 会被**静默跳过**（该图片不进上游，不报错），别指望服务端替你抓图
- `{"type":"function_call","call_id","name","arguments"}` —— 历史里助手调用工具的那一轮
- `{"type":"function_call_output","call_id","output"}` —— 客户端回传的工具执行结果

**明确不支持（会报错，不会假装支持）**：`previous_response_id`——本服务不保存服务端对话状态，传了这个字段会返回 `400`，而不是悄悄忽略。请每次请求都在 `input` 里带上完整对话历史（Codex CLI 本身就是这么做的）。

**响应（非流式）**：
```json
{
  "id": "resp_xxx",
  "object": "response",
  "created_at": 1715970000,
  "status": "completed",
  "model": "claude-sonnet-4.5",
  "output": [
    {
      "id": "msg_xxx",
      "type": "message",
      "role": "assistant",
      "status": "completed",
      "content": [
        {"type": "output_text", "text": "1+1等于2"}
      ]
    }
  ],
  "usage": {
    "input_tokens": 10,
    "output_tokens": 5,
    "total_tokens": 15
  }
}
```

> [!NOTE]
> 上面就是**全部**字段。本服务**不发** `previous_response_id`、`instructions`、`error`，`output_text` 块上也**没有** `annotations`，`usage` 只有这三个计数器（无 `input_tokens_details` / `output_tokens_details`）——按官方 SDK 的完整形状去索引这些键会取到空。唯一的额外字段是 `incomplete_details`：仅当上游截断、`status` 为 `"incomplete"` 时出现，形如 `{"reason":"max_output_tokens"}`；`status:"completed"` 时整个字段省略。

**响应（流式）**：严格按官方协议顺序发送带命名的 SSE 事件，每个事件都带**单调递增**的 `sequence_number`。**没有** `data: [DONE]` 结尾标记（那是 Chat Completions 的老约定）。

> [!IMPORTANT]
> **终结事件有三种，只等 `response.completed` 会把客户端挂死**：正常结束发 `response.completed`；上游截断（输出预算 / 上下文窗口耗尽）发 `response.incomplete`（`status:"incomplete"` + `incomplete_details`）；上游下发非截断异常（限流 / 鉴权 / 参数）或传输中断发 `response.failed`（`status:"failed"` + `error`）。后两种**绝不会**再补一个 `response.completed`——这是刻意的，免得 agent 框架把半截回答当完整结果继续用。收到任一终结事件即可停止读取。

纯文本、正常完成时的事件序列：

```
event: response.created
data: {"type":"response.created","sequence_number":0,"response":{...}}

event: response.in_progress
data: {"type":"response.in_progress","sequence_number":1,...}

event: response.output_item.added
data: {"type":"response.output_item.added","sequence_number":2,...}

event: response.content_part.added
data: {"type":"response.content_part.added","sequence_number":3,...}

event: response.output_text.delta
data: {"type":"response.output_text.delta","sequence_number":4,"delta":"1"}

event: response.output_text.done
data: {"type":"response.output_text.done","sequence_number":5,"text":"1+1等于2"}

event: response.content_part.done
data: {"type":"response.content_part.done","sequence_number":6,...}

event: response.output_item.done
data: {"type":"response.output_item.done","sequence_number":7,...}

event: response.completed
data: {"type":"response.completed","sequence_number":8,"response":{...}}
```

工具调用场景下，`response.output_item.added`（类型 `function_call`）之后跟的是 `response.function_call_arguments.delta` / `response.function_call_arguments.done` / `response.output_item.done`，而不是上面的文本事件。

## Claude 兼容 API

> Anthropic Messages 是本服务的**中枢母格式**，其余协议均双向转换后复用同一条中转内核。

### GET /claude/v1/models

获取模型列表（Anthropic 形状，避开与 OpenAI 裸路径 `/v1/models` 冲突）。**id 与顺序同 `GET /v1/models` 逐条相同**——同一份 17 条目录，只是换成 Anthropic 的条目形状并多了 `display_name`。

**请求**：
```bash
curl http://localhost:8080/claude/v1/models \
  -H "Authorization: Bearer sk-你的API密钥"
```

**响应**：`has_more` 恒为 `false`（目录一次发完，本服务不做游标分页），`first_id` / `last_id` 即目录首尾；每条的 `created_at` 恒为常量 `"2026-01-01T00:00:00Z"`（占位，不是真实发布时间）。下例**只截取了首二条与末一条**，中间 14 条按上文目录顺序同形排列：
```json
{
  "data": [
    {"type": "model", "id": "claude-sonnet-4.5", "display_name": "Claude Sonnet 4.5", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "claude-sonnet-4.6", "display_name": "Claude Sonnet 4.6", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "gpt-5.6-sol", "display_name": "GPT-5.6 Sol", "created_at": "2026-01-01T00:00:00Z"}
  ],
  "has_more": false,
  "first_id": "claude-sonnet-4.5",
  "last_id": "gpt-5.6-sol"
}
```

### POST /v1/messages

发送消息请求（Claude 格式）。也可使用带前缀路径 `/claude/v1/messages`。

**请求体**：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `model` | string | 是 | 模型名称 |
| `messages` | array | 是 | 消息列表，`content` 支持字符串或块数组（`text`/`image`/`tool_use`/`tool_result`）。`image` 的 `source` **只收 `type:"base64"`**，远程图片 URL 一律 `400` |
| `max_tokens` | number | 否 | **接受但不生效**（见「OpenAI 兼容 API」下的生成参数说明），不会截断回复长度。注意 Anthropic 官方规范里该字段是必填，本服务放宽为可选 |
| `system` | string | 否 | 系统提示，字符串或内容块数组均可 |
| `stream` | boolean | 否 | 是否流式返回 |
| `tools` | array | 否 | 工具列表 |
| `tool_choice` | object | 否 | **接受但不生效**（见上），无法强制/禁止调用某个工具 |

**请求示例**：

```bash
curl -X POST http://localhost:8080/v1/messages \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "model": "claude-sonnet-4.5",
    "max_tokens": 1024,
    "messages": [
      {"role": "user", "content": "Hello"}
    ]
  }'
```

**响应**：

```json
{
  "id": "msg-xxx",
  "type": "message",
  "role": "assistant",
  "content": [
    {
      "type": "text",
      "text": "Hello! How can I help you?"
    }
  ],
  "model": "claude-sonnet-4.5",
  "stop_reason": "end_turn",
  "usage": {
    "input_tokens": 10,
    "output_tokens": 20
  }
}
```

> [!NOTE]
> 非流式响应体**没有** `stop_sequence` 字段（该字段只出现在流式的 `message_start` / `message_delta` 事件里）。`stop_reason` 取值：正常结束 `end_turn`、命中工具 `tool_use`、上游输出预算耗尽 `max_tokens`、上下文窗口耗尽 `model_context_window_exceeded`。

> 流式为 Anthropic 标准 SSE：`message_start` → `content_block_start` → `content_block_delta` → … → `message_stop`。工具走 `tool_use` 块与 `input_json_delta`。

### POST /v1/messages/count_tokens

估算消息的 token 数（粗略）。也可使用带前缀路径 `/claude/v1/messages/count_tokens`。

**请求**：
```bash
curl -X POST http://localhost:8080/v1/messages/count_tokens \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {"role": "user", "content": "Hello"}
    ]
  }'
```

**响应**：
```json
{
  "input_tokens": 10
}
```

## Gemini 原生 API

> Gemini 端点全程 **camelCase**，返回 Gemini 原生线格式。

### GET /v1beta/models

获取模型列表。也可使用带前缀路径 `/gemini/v1beta/models`。**id 与顺序同 `GET /v1/models` 逐条相同**——同一份 17 条目录，换成 Gemini 的条目形状。

**请求**：
```bash
curl http://localhost:8080/v1beta/models \
  -H "Authorization: Bearer sk-你的API密钥"
```

**响应**：`name` 为 `models/<id>`；每条的 `supportedGenerationMethods` 恒为 `["generateContent","streamGenerateContent"]` 这两项；**没有** `displayName` 字段（本服务不下发，序列化时整键省略）。下例**只截取了首末各一条**，中间 15 条按上文目录顺序同形排列：
```json
{
  "models": [
    {"name": "models/claude-sonnet-4.5", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/gpt-5.6-sol", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]}
  ]
}
```

### POST /v1beta/models/{model}:generateContent

生成内容（非流式）。也可使用带前缀路径 `/gemini/v1beta/models/{model}:generateContent`。

**请求体**：`contents[]`（`parts[]` 支持 `text`/`inline_data`）、`system_instruction?`、`tools[].function_declarations`、`toolConfig?`、`generationConfig?`。camelCase 与 proto 的 snake_case 拼写（`system_instruction`、`generation_config`、`inline_data`、`mime_type`…）**两种都认**。

> [!IMPORTANT]
> `generationConfig` 里**只有** `maxOutputTokens` 会被解析，而且解析完也**不生效**（详见「OpenAI 兼容 API」下的生成参数说明）；`temperature`、`topP`、`topK`、`stopSequences`、`responseMimeType` 等键属于未知字段，照 Gemini 官方 SDK 的惯例被**静默丢弃**——既不生效、也不报错。
> `toolConfig` 只有 `functionCallingConfig.mode:"NONE"` 会被兑现（做法是本轮不下发工具规格）；`AUTO` 就是默认行为，`ANY`（强制至少调一次）在上游线格式里无从表达，按默认走。

**请求**：
```bash
curl -X POST http://localhost:8080/v1beta/models/claude-sonnet-4.5:generateContent \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "contents": [
      {
        "role": "user",
        "parts": [{"text": "Hello"}]
      }
    ]
  }'
```

**响应**：返回 `{candidates[].content.parts, finishReason, usageMetadata}`；工具走 `functionCall`。

### POST /v1beta/models/{model}:streamGenerateContent

生成内容（流式，`?alt=sse`）。也可使用带前缀路径 `/gemini/v1beta/models/{model}:streamGenerateContent`。

**请求**：
```bash
curl -X POST "http://localhost:8080/v1beta/models/claude-sonnet-4.5:streamGenerateContent?alt=sse" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "contents": [
      {
        "role": "user",
        "parts": [{"text": "Hello"}]
      }
    ]
  }'
```

**流式响应**（SSE，camelCase，无 `[DONE]`）：

```
data: {"candidates":[{"content":{"parts":[{"text":"Hello"}]}}]}

data: {"candidates":[{"content":{"parts":[{"text":" there"}]}}]}
```

> [!NOTE]
> 密钥可走鉴权闸接受的任意通道，优先级为：`Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > query（`?api_key=` > `?token=` > `?key=`）。Gemini 原生的 `x-goog-api-key` 头与 `?key=` 参数**同样被接受**，官方 `google-genai` SDK 只换 `base_url` 即可直接用。要换的是**值**：一律传**本服务**的 API-KEY，绝不是真的 Google / OpenAI 厂商密钥。

## 管理 API

`/admin` 管理面板（静态，`rust-embed` 编译期嵌入）由 `/api/admin/*` 接口驱动。下列端点均需 `adminApiKey`（未设则回退 `apiKey`；两者皆空时管理 API 开放——切勿如此对外暴露）。鉴权携带方式与优先级同协议闸的六条通道（`Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > `?api_key=` > `?token=` > `?key=`；无法设头的 SSE 日志流用 `?api_key=`）。响应体**默认 camelCase**，但下列端点为对齐面板的数据模型返回 **snake_case**：`GET /api/admin/config`、`GET /api/admin/models`，以及两个旧端点 `GET /admin/api/config`（与 `/api/admin/config` 同一 handler）和 `GET /admin/api/stats`（**整个响应**，`summary` 与 `accounts[]` 的每一项都是——`last_used_unix`、`auth_method`、`expires_at_unix`、`has_profile_arn`、`cooldown_until`、`in_cooldown` 等）。所有响应**绝不含账号的 access/refresh token**（`GET /api/admin/credentials` 只出状态）。

> [!WARNING]
> 管理接口的响应**并非无密**：`GET`/`POST /api/admin/api-keys` 的 `key` 字段是**完整明文**，`GET /api/admin/server-info` 的 `masterApiKey` 也是**完整明文**；只有 `GET /api/admin/config/auth-keys` 与 `GET /api/admin/config` 做了脱敏。本服务没有"只读管理员"角色——拿到管理密钥即可读取、创建、轮换全部 key。请把管理接口的响应当密钥对待：不要贴进 issue、日志或第三方工具。

### GET /api/admin/credentials

获取账号池状态（也是隐式"登录校验"面——返回 200 即视为 key 有效）。

**请求**：
```bash
curl http://localhost:8080/api/admin/credentials \
  -H "Authorization: Bearer sk-你的管理密钥"
```

**响应**：
```json
{
  "total": 2,
  "available": 2,
  "currentId": -1,
  "credentials": [
    {
      "id": 12345,
      "priority": 1,
      "weight": 1,
      "disabled": false,
      "failureCount": 0,
      "isCurrent": false,
      "expiresAt": "2026-07-25T12:00:00Z",
      "authMethod": "social",
      "hasProfileArn": true,
      "successCount": 10,
      "healthStatus": "healthy",
      "statusReason": "none",
      "throttleCount": 0
    }
  ]
}
```

> [!NOTE]
> 账号池是**每次请求**现选账号，并不存在一个持久的"当前账号"：`currentId` 恒为 `-1`、每行的 `isCurrent` 恒为 `false`，两个字段是给未来的粘性选择模式预留的。请勿依赖它们做判断。

`healthStatus` 为 `healthy` / `warning`(有失败计数但仍会被选中)/ `unhealthy`(在冷却窗口内、或已被上游封禁)/ `disabled`(已关闭)。> **`failureCount` 是累计失败数,`throttleCount` 是限流事件条数。** 两者曾经错位:前者装的是连续失败连击数(成功一次或进冷却即清零),后者装的是累计失败数——于是被封禁的账号显示成「限流 1、失败 0」,把「账号被停用」错报成「歇一会儿就好」。

`statusReason` 记录**最近一次失败的原因**——`none` / `banned`(被上游停用)/ `quota` / `token_expired` / `throttled` / `refresh_denied`——答「为什么不能用」。

> **`banned` 会真正把账号挡在池外**,其余原因只影响展示。冷却是计时器,到点自动回池;封禁是上游给的结论(原话为「账号已锁定,请联系客服验证身份」),不随时间解除。若只按计时器放行,冷却一过账号就重新入选、再失败、再冷却,循环烧真实请求,而 `available` 还把它算作可用——面板一边挂着「封禁」、计数一边说没事,两个数字互相矛盾。因此封禁账号不被选中、不计入 `available`、`healthStatus` 报 `unhealthy`。它不会自愈(永远等不到那次成功来清标签),**唯一出口是面板的「重置」**(`POST /api/admin/credentials/{id}/reset`,会一并清掉该结论)。其余原因仍在账号下次成功时自动清空。该结论随 `credentials.json` 落盘(`statusReason` 键)、重启后还原——只活在内存里的话,每次发版都会把它抹掉、账号悄悄回池。**strike 计数与冷却截止时刻仍不落盘**:那两个是计时器,重启从零开始无非早重试一次;结论不同,它决定账号能不能进池。

### POST /api/admin/credentials

新增一条凭据入池并落盘。

**必填的只有 `refreshToken`**（`authMethod` 为 `idc` 时还需 `clientId` + `clientSecret`）。本端点**不接受** access token 与到期时间——它们由首次自动刷新时补齐；多余的键会被**静默忽略**（不报错）。可选键：`authMethod`、`email`、`nickname`、`clientId`、`clientSecret`、`profileArn`、`priority`、`weight`、`authRegion`、`apiRegion`、`machineId`、`proxyUrl`、`proxyUsername`、`proxyPassword`。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/credentials \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的管理密钥" \
  -d '{
    "refreshToken": "...",
    "authMethod": "social",
    "profileArn": "arn:aws:codewhisperer:us-east-1:...:profile/..."
  }'
```

### PUT /api/admin/credentials/{id}

更新已有凭据。

**请求**：
```bash
curl -X PUT http://localhost:8080/api/admin/credentials/12345 \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的管理密钥" \
  -d '{"weight": 2}'
```

### DELETE /api/admin/credentials/{id}

从池中移除凭据。

**请求**：
```bash
curl -X DELETE http://localhost:8080/api/admin/credentials/12345 \
  -H "Authorization: Bearer sk-你的管理密钥"
```

### POST /api/admin/credentials/{id}/disabled

启用/禁用账号。请求体 `{"disabled": <布尔>}`，该字段**必填**（缺失 `422`）。改的是**内存态**、不落盘，重启后复位为 `credentials.json` 里的值。旧路径 `POST /admin/api/accounts/{id}/enable|disable` 是本端点的别名，见文末「旧管理端点」。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/disabled \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的管理密钥" \
  -d '{"disabled": true}'
```

**响应**：`message` 只有两个取值——`"credential disabled"` 或 `"credential enabled"`；id 不在池内 → `404`，体 `{"error":"account not found","id":"12345"}`。
```json
{
  "success": true,
  "message": "credential disabled"
}
```

### POST /api/admin/credentials/{id}/priority

设置账号优先级——它**就是**池内权重（`balanced` 策略下越大分到的流量越多），小于 1 会被钳到 1。请求体只有 `{"priority": <整数>}`，`priority` **必填**（缺失返回 `422`）；本端点**没有**独立的 `weight` 字段，多传会被静默忽略。要显式设权重请改用 `PUT /api/admin/credentials/{id}` 传 `{"weight": N}`。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/priority \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的管理密钥" \
  -d '{"priority": 2}'
```

### POST /api/admin/credentials/{id}/reset

清失败计数 / 冷却。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/reset \
  -H "Authorization: Bearer sk-你的管理密钥"
```

### POST /api/admin/credentials/batch-import

批量导入凭据；逐条规整 / 校验 / 落盘，返回逐项结果与计数。

顶层请求体**必须**是 `{"data": …}` 对象（缺 `data` 键直接 `422`）；`data` 本身可以是数组、KAM 的 `{accounts:[…]}` 对象或单个对象。逐项只认 `refreshToken`（必填）、`clientId`、`clientSecret`、`region` / `authRegion` / `apiRegion`、`email`、`nickname`、`machineId`、`priority`（也支持 KAM 的 `credentials: {…}` 嵌套）；`authMethod` 由 `clientId`+`clientSecret` 是否成对出现**自动推断**（成对 → `idc`，否则 `social`；只给其一即判失败），`accessToken`、`expiresAt`、`profileArn` 等键会被忽略。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/credentials/batch-import \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的管理密钥" \
  -d '{"data":[{"refreshToken":"...","email":"a@x.io"}]}'
```

**响应**：
```json
{
  "success": true,
  "message": "imported 1 of 1 credential(s), 0 duplicate, 0 failed",
  "total": 1,
  "added": 1,
  "duplicate": 0,
  "failed": 0,
  "results": [{"index": 1, "status": "added", "credentialId": 2, "email": "a@x.io"}]
}
```

逐项 `index` 从 **1** 起；`status` 为 `added` | `duplicate` | `failed`；`credentialId`/`email`/`error` 缺省时不出现；逐项**没有** `success` 字段。

### POST /api/admin/login/builderid/start · /poll

AWS Builder ID 设备码流。`start` 返回 `{sessionId,userCode,verificationUri,interval}`，`poll` 返回 `{success,completed,status,interval?,credentialId?,email?}`，成功即落库（无需手改 `credentials.json`）。

> [!IMPORTANT]
> 两个端点都用 JSON body 提取器，**必须带 `Content-Type: application/json` 和请求体**——不带体的裸 `POST` 会被在进 handler 之前拒掉。`start` 的字段全可选（不指定 region 就发 `{}`）；`poll` 的 `sessionId` **必填**，缺失返回 `422`。

**请求**：
```bash
# 发起：region 可省，但 body 不能省
curl -X POST http://localhost:8080/api/admin/login/builderid/start \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的管理密钥" \
  -d '{}'

# 轮询：用 start 回的 sessionId
curl -X POST http://localhost:8080/api/admin/login/builderid/poll \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的管理密钥" \
  -d '{"sessionId": "start 返回的 sessionId"}'
```

### POST /api/admin/login/iam-sso/start · /complete

IAM Identity Center（SSO）流。`start` 返回 `{sessionId,authorizeUrl}`；`complete` 消费回调 URL（校验 `state`）后落库。

> [!IMPORTANT]
> `start` 的 `startUrl` **必填**：整个键缺失是 `422`，给了空串 / 全空白是 `400`（`startUrl is required`）。`region` 可省（默认 `us-east-1`）。`complete` 需要 `{"sessionId":…,"callbackUrl":…}` 两个字段，同样都必填。两者都必须带 `Content-Type: application/json`。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/login/iam-sso/start \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的管理密钥" \
  -d '{"startUrl": "https://your-domain.awsapps.com/start", "region": "us-east-1"}'
```

### POST /api/admin/login/sso-token

批量导入原始 bearer/SSO token（每行一个）。

请求体的字段名是 **`bearerToken`**（必填，缺失返回 `422`），值为**整段换行分隔文本**，服务端按行拆分逐行兑换，最多 200 行（超出的行记为 failed）。可选 `region`（缺省 `us-east-1`）。其它键会被静默忽略。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/login/sso-token \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的管理密钥" \
  -d '{"bearerToken": "token1\ntoken2", "region": "us-east-1"}'
```

**响应**：
```json
{
  "added": 2,
  "failed": [{"lineIndex": 3, "error": "..."}]
}
```

### GET /api/admin/api-keys · POST /api/admin/api-keys

列出 / 创建对外调用 API-KEY（你发给调用方的 key）。两者的响应里 `key` 字段均为**完整明文**——前端列表卡片自行脱敏显示，但"复制"按钮需要完整值。

**请求**：
```bash
curl http://localhost:8080/api/admin/api-keys \
  -H "Authorization: Bearer sk-你的管理密钥"
```

**响应**：
```json
[
  {
    "id": 1,
    "key": "sk-c8a63d2e6323ca12efd128144f621e8f",
    "name": "我的 Key",
    "enabled": true,
    "createdAt": "2026-07-25T12:00:00Z",
    "expiresAt": null,
    "spendingLimit": null,
    "limitUnit": "usd",
    "durationDays": null,
    "activatedAt": null
  }
]
```

### PUT /api/admin/api-keys/{id} · DELETE /api/admin/api-keys/{id}

更新 / 删除 API-KEY。

### GET /api/admin/api-keys/usage

获取全部 key 的用量。

### GET /api/admin/api-keys/{id}/usage · DELETE

单 key 用量 / 清零。

### GET /api/admin/api-keys/{id}/usage/records

单 key 分页用量记录。

**请求**：
```bash
curl "http://localhost:8080/api/admin/api-keys/7/usage/records?page=1&page_size=20" \
  -H "Authorization: Bearer sk-你的管理密钥"
```

### GET /api/admin/credentials/{id}/usage/records · /usage/today

单账号分页用量记录 / 当日汇总。两者都**不校验账号是否存在**：未知 id 回空页 / 全零汇总，不是 `404`（路径 id 按数值解析，非数值回落 `0`）。

`/usage/records` 的分页参数是 **snake_case 的 `?page=&page_size=`**（缺省 `page=1`、`page_size=20`；`page_size` 至少按 1 计，`page` 越界钳到最后一页，空集回 `page=1`、`totalPages=0`），响应体字段则是 camelCase：

**请求**：
```bash
curl "http://localhost:8080/api/admin/credentials/12345/usage/records?page=1&page_size=20" \
  -H "Authorization: Bearer sk-你的管理密钥"
```

**响应**：
```json
{
  "records": [
    {
      "model": "claude-sonnet-4.5",
      "inputTokens": 1200,
      "outputTokens": 340,
      "estimatedCost": 0.0123,
      "creditsUsed": 1.7,
      "cacheReadInputTokens": 0,
      "cacheCreationInputTokens": 0,
      "createdAt": "2026-07-25T12:00:00Z",
      "credentialId": 12345,
      "credentialLabel": "a@x.io",
      "clientIp": "203.0.113.7"
    }
  ],
  "total": 1,
  "page": 1,
  "pageSize": 20,
  "totalPages": 1
}
```

记录按时间降序（最新在前）。`creditsUsed`、`cacheReadInputTokens`、`cacheCreationInputTokens`、`credentialLabel`、`clientIp` 无值时**整个键不出现**（不是 `null`）；`creditsSaved` 目前**恒无数据源**，永远不会出现。`credentialLabel` 由账号池现算：昵称 → 邮箱 → `#<数值 id>`。

`/usage/today` 不收任何参数，按 **CST（UTC+8）** 定"今天"：

**请求**：
```bash
curl http://localhost:8080/api/admin/credentials/12345/usage/today \
  -H "Authorization: Bearer sk-你的管理密钥"
```

**响应**：
```json
{
  "date": "2026-07-25",
  "credentialId": 12345,
  "totalRequests": 12,
  "totalInputTokens": 3400,
  "totalOutputTokens": 900,
  "totalCost": 0.12,
  "totalCredits": 0.34
}
```

`totalCredits` 是当日各条记录里**上游回报的积分消耗**之和（缺该值的记录按 0 计），不是由 `totalCost` 换算而来。`totalCreditsSaved` 同样恒无数据源、不会出现。

### GET /api/admin/credentials/{id}/failure-logs · /throttle-logs

单账号近期失败 / 限流事件。两者**同形同参**：`?page=&page_size=`（缺省 `page=1`、`page_size=20`，降序、最新在前），未知 id 回空页而非 `404`。区别只在数据源与几个恒定字段：

- `failure-logs`：中转过程中判为账号级**鉴权失败**的事件，`statusCode` 为 `401` 或 `403`（无从解析时按 `401` 记），`responseBody` 截断到 **2000** 字符。
- `throttle-logs`：判为**配额/限流**的事件，`statusCode` **恒为 `429`**（写入时硬编码的常量，不是上游原样状态码），`responseBody` 截断到 **200** 字符。

两者都按账号各留最近 **500** 条（超出丢最旧的，所以这两个列表都不能当完整审计流水）。`requestType` 目前中转侧只会写 `"api"` 一个值。

**请求**：
```bash
curl "http://localhost:8080/api/admin/credentials/12345/throttle-logs?page=1&page_size=20" \
  -H "Authorization: Bearer sk-你的管理密钥"
```

**响应**：
```json
{
  "records": [
    {
      "credentialId": 12345,
      "requestType": "api",
      "statusCode": 429,
      "responseBody": "ThrottlingException: ...",
      "createdAt": "2026-07-25T12:00:00Z"
    }
  ],
  "total": 1,
  "page": 1,
  "pageSize": 20,
  "totalPages": 1
}
```

### GET /api/admin/credentials/{id}/balance

账号余额（5 分钟缓存）。

**请求**：
```bash
curl http://localhost:8080/api/admin/credentials/12345/balance \
  -H "Authorization: Bearer sk-你的管理密钥"
```

### GET /api/admin/credits/global

全局剩余积分聚合，**只读上面那份共享余额缓存、零上游调用**：遍历池内全部账号 id，只累加**仍新鲜**（同一份 5 分钟 TTL 缓存）的快照的 `remaining`；缓存 miss / 过期的账号直接跳过，本端点**不会**替它们去打上游。因此 `cachedCount < totalCount` 时 `globalCredits` 是**部分和**，要补全得先由账号页 / 仪表盘去查各账号余额把缓存填热。

**请求**：
```bash
curl http://localhost:8080/api/admin/credits/global \
  -H "Authorization: Bearer sk-你的管理密钥"
```

**响应**：
```json
{
  "globalCredits": 1234.5,
  "cachedCount": 8,
  "totalCount": 10,
  "oldestCacheUnix": 1753444800
}
```

`oldestCacheUnix` 是参与求和的缓存条目里**最旧**的抓取时刻（Unix 秒），供前端显示"更新于 X 之前"；一条都没命中时为 `null`（此时 `globalCredits` 为 `0`、`cachedCount` 为 `0`）。恒返回 `200`。

### GET /api/admin/usage/daily · /usage/daily/{date}/records

每日用量汇总 / 指定日期的记录（含客户端 IP 与账号标签）。日界一律按 **CST（UTC+8）**，`{date}` 写 `YYYY-MM-DD`。

`/usage/daily` 不收参数，返回按日期降序的数组（空存储 → `[]`）：

**响应**：
```json
[
  {"date": "2026-07-25", "totalRequests": 120, "totalCost": 1.23, "totalCredits": 3.4}
]
```

只有这四个字段（没有 tokens 计数）；`totalCreditsSaved` 恒无数据源、不会出现。

`/usage/daily/{date}/records` 的分页参数同为 `?page=&page_size=`（缺省 `page=1`、`page_size=20`），未知日期回空页而非 `404`。注意服务端在分页**之前**先把该日记录截到最新 **2000** 条，故 `total` 最大就是 2000，再往后翻也取不到更旧的。记录形状与 `GET /api/admin/credentials/{id}/usage/records` 完全一致（同样含 `credentialId`、`credentialLabel`、`clientIp`）。

**请求**：
```bash
curl "http://localhost:8080/api/admin/usage/daily/2026-07-25/records?page=1&page_size=20" \
  -H "Authorization: Bearer sk-你的管理密钥"
```

### GET /api/admin/usage/summary

时间窗口内跨全部账号的用量聚合 + 图表分桶 + 运行健康指标。

窗口二选一：`?range=` 取枚举 `6h` | `24h` | `3d` | `7d` | `30d`（**优先**），或 `?hours=<正整数>` 给任意小时数；两个都不给按 `24h`。非法 `range`、`hours=0` → `400`。分桶宽度由窗口自动决定：窗口 ≤ 24 小时按小时（`bucketSecs` 为 `3600`），更长按天（`86400`）。

**请求**：
```bash
curl "http://localhost:8080/api/admin/usage/summary?range=24h" \
  -H "Authorization: Bearer sk-你的管理密钥"
```

**响应**：
```json
{
  "range": "24h",
  "windowSecs": 86400,
  "sinceUnix": 1753358400,
  "untilUnix": 1753444800,
  "bucketSecs": 3600,
  "totalRequests": 117,
  "totalInputTokens": 34000,
  "totalOutputTokens": 9000,
  "totalCost": 1.23,
  "totalCredits": 3.4,
  "dailyFallbackApplied": false,
  "series": [
    {"bucketStartUnix": 1753358400, "totalRequests": 5, "totalCost": 0.05, "totalCredits": 0.14}
  ],
  "successfulRequests": 117,
  "failedRequests": 3,
  "errorRate": 0.025,
  "avgLatencyMs": 812.5,
  "rotationSuccessRate": 0.975
}
```

各字段口径（都按实际实现说，别当精确埋点用）：

- `range` 回显规整后的窗口标签（用 `?hours=N` 时是 `"<N>h"`）；`untilUnix` 为当前时刻，`sinceUnix = untilUnix - windowSecs`，闭区间。
- 数值一律是**未预舍入**的 f64/i64 原始精度，格式化交给前端。
- `dailyFallbackApplied`：窗口 > 1 天时，服务端会拿每日汇总与原始记录逐个**完整 CST 日**取较大值，用差额补齐 requests/cost/credits（原始记录按账号有条数上限，长窗最旧的可能已被淘汰）。补过就是 `true`——**此时 tokens 两项没有对应的每日汇总、补不了，可能偏低**。
- `successfulRequests` = 窗口内用量记录条数（含上面兜底补齐的部分），与 `totalRequests` 同源同值；`failedRequests` = 窗口内失败日志（401/403）+ 限流日志（429）条数。事件日志按账号有 500 条上限，极高频失败时最旧的已被淘汰，故 `failedRequests` 是**下界**、`errorRate` 只会偏保守。
- `errorRate` = 失败 /（成功 + 失败）；`rotationSuccessRate` 就是 `1 - errorRate`（近似值：跨账号重试链路本身没有单独埋点，只以"最终有没有落一条成功用量记录"来近似）。窗口内无任何活动时，二者分别为 `0.0` 与 `1.0`。
- `avgLatencyMs` 只统计带延迟样本的成功记录（早期记录没有该字段，不计入），无样本 → `0.0`。
- `series` 按桶起始升序；空窗口 → `[]`。

### GET /api/admin/rpm

实时 RPM 快照。

### GET /api/admin/config

获取脱敏配置视图（仅布尔 / 非密字段）。**本端点的字段名是 snake_case**（对齐面板数据模型），不是 camelCase。

**请求**：
```bash
curl http://localhost:8080/api/admin/config \
  -H "Authorization: Bearer sk-你的管理密钥"
```

**响应**：
```json
{
  "host": "127.0.0.1",
  "port": 8080,
  "region": "us-east-1",
  "load_balancing_mode": "priority",
  "max_rpm_per_credential": 0,
  "kiro_version": "0.11.107",
  "system_version": "win32#10.0.22631",
  "node_version": "22.22.0",
  "credentials_path": "/app/data/credentials.json",
  "api_key_set": false,
  "admin_api_key_set": false
}
```

### GET /api/admin/models

带 `display_name`/`type`/`max_tokens` 的模型列表（**snake_case 字段名**，同上）。取值优先级：各账号上游 `ListAvailableModels` 的**并集**（缓存命中即用）→ 并集为空时回落到与协议侧 `/models` **同一份** 17 条编译期目录，同时在后台惰性触发一次回填（单飞 + 60 秒冷却 + 有界扫描，不阻塞本次响应，下次请求才可能拿到并集）。所以本端点与协议 `/models` 现在只差一处：缓存热时它给的是**上游按档位实际授权**的并集，协议侧则恒定给目录全量。

条目形状：`{id, object:"model", created, owned_by, display_name, type, max_tokens, rate_multiplier?}`。`created` 恒为常量 `1700000000`、`type` 恒为 `"chat"`；`rate_multiplier` 只有上游并集条目带该值时才出现（回落目录的条目不带）。`owned_by` 在回落条目上恒为 `"kiro2api"`，在并集条目上由 id **本地推断**（`auto` → `kiro`，含 `claude` → `anthropic`，含 `gpt` → `openai`，另有 `deepseek`/`minimax`/`glm`/`qwen`，都不匹配则 `unknown`），并非上游下发的字段。并集条目的 `max_tokens` 取上游 `tokenLimits.maxOutputTokens`，缺失（0）时按 `200000` 回落。

### POST /api/admin/credentials/models/refresh · /credentials/{id}/models/refresh

手动实拉上游 `ListAvailableModels` 并回填模型缓存（即上面那份并集的来源）。两个端点都**不收请求体**，无需 `Content-Type`。

**单账号**（`/api/admin/credentials/{id}/models/refresh`）：路径 id 必须在池内，**已禁用的账号也照刷**（单账号端点不跳过禁用）；池里没有这个 id → `404`，体 `{"error":"account not found","id":"…"}`。上游失败 → `502`，`error` 里带真因（上游状态码 + 短说明），且不写缓存。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/models/refresh \
  -H "Authorization: Bearer sk-你的管理密钥"
```

**响应**：
```json
{
  "success": true,
  "id": "12345",
  "count": 18
}
```

`id` 原样回显路径里的字符串（不转数值），`count` 为该账号本次拉到并写入缓存的模型条数。

**全池**（`/api/admin/credentials/models/refresh`）：并**不会**挨个刷全部账号——先跳过已禁用账号，再按**订阅档位**分组，每个已缓存档位只刷**一个代表账号**；档位未知的账号走有界发现（并集连续 3 次不再增长、或累计成功 12 个、或试完即停）。**恒返回 `200`**：批量调用本身算成功，逐账号失败只进 `errors[]` 与 `failed` 计数（全失败时也是 `200` + `refreshed:0`）。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/credentials/models/refresh \
  -H "Authorization: Bearer sk-你的管理密钥"
```

**响应**：
```json
{
  "success": true,
  "refreshed": 2,
  "failed": 1,
  "errors": [{"id": 12345, "error": "models upstream HTTP 403: ..."}],
  "tiers": ["KIRO FREE", "KIRO PRO+"]
}
```

`errors[].id` 是**数值**账号 id（非数值 id 回落 `0`）；`tiers` 是本次实际刷成功所涵盖的档位列表，发现阶段刷成功、但档位仍拿不到的账号会让列表里多一个 `"unknown"` 占位。

### GET /api/admin/config/load-balancing · PUT

运行期读取 / 切换负载均衡模式（`priority` / `balanced`），落盘 `config.json`。

**请求**：
```bash
curl -X PUT http://localhost:8080/api/admin/config/load-balancing \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的管理密钥" \
  -d '{"mode": "balanced"}'
```

### GET /api/admin/config/auth-keys · PUT

运行期读取（脱敏）/ 轮换 `apiKey` 与 `adminApiKey`；即时生效（无需重启）。

### GET /api/admin/server-info

**请求**：
```bash
curl http://localhost:8080/api/admin/server-info \
  -H "Authorization: Bearer sk-你的管理密钥"
```

**响应**：`masterApiKey` 为所配置 `apiKey` 的**完整明文**（未配置则 `null`），此处**不脱敏**——前端在浏览器里自行脱敏显示、"复制"按钮取完整值；要脱敏形式请用 `GET /api/admin/config/auth-keys`。`version` 为 kiro2api 版本，`kiroVersion` 为伪装上游 UA 版本，`rustVersion` 为构建时 rustc 版本；其余为运行期指标（`serverTime`、`serverTimeUnix`、`os`、`memoryUsedBytes`、`memoryTotalBytes`、`cpuPercent`、`runMode`、`pid`、`uptimeSecs`）。
```json
{
  "masterApiKey": "sk-你的主密钥明文",
  "version": "0.7.8",
  "kiroVersion": "0.11.107",
  "rustVersion": "1.90.0",
  "runMode": "Docker",
  "uptimeSecs": 3600
}
```

### GET /api/admin/check-update

查 GitHub Releases 的最新版并与当前版本比对。服务端出站打 `https://api.github.com/repos/xwteam/kiro2api/releases/latest`。

**请求**：
```bash
curl http://localhost:8080/api/admin/check-update \
  -H "Authorization: Bearer sk-你的管理密钥"
```

**响应**：
```json
{
  "current": "0.4.0",
  "latest": "0.4.1",
  "hasUpdate": true,
  "updateUrl": "https://github.com/xwteam/kiro2api/releases/tag/v0.4.1",
  "releaseNotes": "..."
}
```

`current` 是本进程编译进去的 crate 版本；`latest` 取 Release 的 `tag_name` 去掉前导 `v`；`updateUrl` 取该 Release 的 `html_url`，`releaseNotes` 取其正文。`hasUpdate` 只是 `latest != current` 的**字符串不等**判断，不做语义化版本比较（回退到旧版部署时也会显示"有更新"）。本端点**永不报错**：网络不通、仓库没有 Release、私有仓 404 等一律保守回 `200` + `latest = current` + `hasUpdate: false`，`updateUrl` 退成仓库 releases 首页、`releaseNotes` 为空串。

### POST /api/admin/update

**不会自动更新**——只返回一段要你自己去服务器上执行的命令（配合面板的"复制"按钮）。不收请求体，恒 `200`，三个字段全是硬编码常量：

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/update \
  -H "Authorization: Bearer sk-你的管理密钥"
```

**响应**：
```json
{
  "status": "ok",
  "message": "请在服务器上执行以下命令完成更新:",
  "command": "docker compose pull && docker compose up -d"
}
```

### POST /api/admin/restart

**真的会退出当前进程**，靠容器的 `restart` 策略（或 systemd/supervisor）把它重新拉起；裸机无守护时等同于停机。

必须带 `?confirm=true` 二次确认。缺这个参数、或传 `confirm=false` 时 handler 直接回 `400`，体为 `{"error":{"message":"重启需二次确认,请带查询参数 ?confirm=true","type":"confirmation_required"}}`（`confirm` 只认布尔字面量，写别的值会在进 handler 之前被查询串反序列化拒掉）。确认后**先**回 `200`，再由后台任务等 0.5 秒、把统计 / API-KEY / 余额缓存 / 失败限流日志这四份去抖存储**全部刷盘**，然后 `exit(0)`——所以刚在面板上删掉的 key 不会因为这次重启而复活。不收请求体。

**请求**：
```bash
curl -X POST "http://localhost:8080/api/admin/restart?confirm=true" \
  -H "Authorization: Bearer sk-你的管理密钥"
```

**响应**：
```json
{
  "status": "ok",
  "message": "Server restarting..."
}
```

`status` 与 `message` 均为硬编码常量。

### GET /api/admin/logs/stream

实时日志 SSE 流（需 `logCapacity > 0`，否则 `503`）：先 history 事件，再逐条 log 事件带心跳。`EventSource` 无法设头，用 `?api_key=<管理密钥>` 鉴权。

**请求**：
```bash
curl "http://localhost:8080/api/admin/logs/stream?api_key=sk-你的管理密钥"
```

### GET /api/admin/logs/snapshot · /logs/download

当前缓冲的 JSON 数组 / 导出为 `.txt` 附件。

### 旧管理端点（保留向后兼容）

- `GET /admin/api/stats` — `{accounts:[…], summary:{total,active,disabled,in_cooldown}}`
- `GET /admin/api/config` — 脱敏配置（与 `GET /api/admin/config` 是**同一个 handler**，响应逐字相同）
- `POST /admin/api/accounts/{id}/enable` | `disable` — 手动启停。这两条是 `POST /api/admin/credentials/{id}/disabled` 的**旧别名**：走同一个池方法，同样只改内存、不落盘（重启后复位为 `credentials.json` 里的值），`enable` 等价于新端点传 `{"disabled": false}`、`disable` 等价于 `{"disabled": true}`。差别只有两点：旧端点**不收请求体**（启停写在路径上，不必带 `Content-Type`），以及响应形状不同——回 `{"ok":true,"id":"12345","disabled":true}`（`id` 原样回显路径里的字符串），而不是新端点的 `{"success":true,"message":"…"}`。id 不在池内 → `404`，体 `{"error":"account not found","id":"12345"}`。

**请求**：
```bash
curl -X POST http://localhost:8080/admin/api/accounts/12345/disable \
  -H "Authorization: Bearer sk-你的管理密钥"
```

## 用户 API

`/user` 用户面板（静态，`rust-embed` 嵌入）由 `/api/user/*` 驱动。这些端点**不走** admin 闸——每次请求用调用方**自己的 API-KEY** 鉴权：key 从请求头按 `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` 的优先级提取（**没有 query 通道**，`?api_key=` / `?token=` 在这里不认）；`POST /api/user/login` 额外接受 body 里的 `{apiKey}`，且 body 的 `apiKey` **优先于**请求头。handler 校验后把数据面限定到该 key。key 非法 → `401`，体 `{"error":"…"}`。响应 camelCase；`credits = cost / 0.72`。

### POST /api/user/login

校验 key，返回该 key 的额度与用量概览。

**请求**：
```bash
curl -X POST http://localhost:8080/api/user/login \
  -H "Content-Type: application/json" \
  -d '{"apiKey": "sk-你的API密钥"}'
```

**响应**：
```json
{
  "id": 7,
  "name": "我的 Key",
  "spendingLimit": 100,
  "limitUnit": "credits",
  "totalCost": 1.2,
  "totalCredits": 1.67,
  "expiresAt": null,
  "durationDays": 30,
  "activatedAt": "2026-07-25T00:00:00Z"
}
```

### GET /api/user/usage

该 key 的用量汇总（含 `byModel[]`）。

**请求**：
```bash
curl http://localhost:8080/api/user/usage \
  -H "x-api-key: sk-你的API密钥"
```

### GET /api/user/usage/records

该 key 的用量记录，分页（降序、最新在前）。参数名是 **snake_case 的 `?page=&page_size=`**；缺省 `page=1`、**`page_size=50`**（与管理端同名端点的 `20` **不一样**），`page` 越界钳到最后一页。key 校验失败 → `401`；key 有效但一条记录都没有 → 空页（`total=0`），不是 `404`。

**请求**：
```bash
curl "http://localhost:8080/api/user/usage/records?page=1&page_size=20" \
  -H "x-api-key: sk-你的API密钥"
```

**响应**：
```json
{
  "records": [
    {
      "model": "claude-sonnet-4.5",
      "inputTokens": 1200,
      "outputTokens": 340,
      "estimatedCost": 0.0123,
      "creditsUsed": 1.7,
      "cacheReadInputTokens": 0,
      "cacheCreationInputTokens": 0,
      "createdAt": "2026-07-25T12:00:00Z",
      "clientIp": "203.0.113.7"
    }
  ],
  "total": 1,
  "page": 1,
  "pageSize": 20,
  "totalPages": 1
}
```

无值的可选键**整个不出现**（不是 `null`）：`creditsUsed`、`cacheReadInputTokens`、`cacheCreationInputTokens`、`clientIp` 均如此；`creditsSaved` 与 `credentialLabel` 则是**恒定无值**（前者没有数据源，后者用户面不解析账号标签），永远不会出现。用户面的记录里也**没有** `credentialId`——用哪个账号中转只在管理端可见。

## 系统 API

### GET /health

健康检查（Docker 探针适配，不鉴权）。

**请求**：
```bash
curl http://localhost:8080/health
```

**响应**：
```json
{
  "service": "kiro2api",
  "status": "ok",
  "version": "0.7.8"
}
```

### GET /v1/ping

探活（不鉴权）。

**请求**：
```bash
curl http://localhost:8080/v1/ping
```

**响应**：
```json
{
  "pong": true
}
```

## 请求示例

> base URL 用**标准裸前缀**：OpenAI = `{host}/v1`，Anthropic = `{host}`（SDK 自动补 `/v1/messages`），Gemini = `{host}/v1beta`。也可用显式厂商前缀 `/openai/v1`、`/claude/v1`、`/gemini/v1beta`。

### Python - OpenAI SDK

```python
from openai import OpenAI

client = OpenAI(
    api_key="sk-你的API密钥",
    base_url="http://localhost:8080/v1"
)

# 非流式请求
response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "Hello"}]
)
print(response.choices[0].message.content)

# 流式请求
for chunk in client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "Hello"}],
    stream=True
):
    if chunk.choices[0].delta.content:
        print(chunk.choices[0].delta.content, end="")
```

### Python - Anthropic SDK

```python
import anthropic

client = anthropic.Anthropic(
    api_key="sk-你的API密钥",
    base_url="http://localhost:8080"
)

message = client.messages.create(
    model="claude-sonnet-4.5",
    max_tokens=1024,
    messages=[{"role": "user", "content": "Hello"}]
)
print(message.content[0].text)
```

### Python - Gemini SDK

```python
from google import genai

client = genai.Client(
    api_key="sk-你的API密钥",
    http_options={"base_url": "http://localhost:8080/v1beta"}
)

resp = client.models.generate_content(
    model="claude-sonnet-4.5",
    contents="Hello"
)
print(resp.text)
```

### JavaScript - Node.js

```javascript
import OpenAI from "openai";

const client = new OpenAI({
  apiKey: "sk-你的API密钥",
  baseURL: "http://localhost:8080/v1"
});

const message = await client.chat.completions.create({
  model: "claude-sonnet-4.5",
  messages: [{ role: "user", content: "Hello" }]
});

console.log(message.choices[0].message.content);
```

### cURL

```bash
# 非流式请求
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "Hello"}]
  }'

# 流式请求
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "Hello"}],
    "stream": true
  }'
```

## 获取帮助

- 查看 [DEPLOY.md](./DEPLOY.md) 了解部署方法
- 查看 [USAGE.md](./USAGE.md) 了解使用方法
- 查看 [README.md](../../README.md) 了解项目概况
- 提交 Issue：[GitHub Issues](https://github.com/xwteam/kiro2api/issues)
