# API 文档

本文档详细说明 kiro2api 的所有 API 端点和使用方法。kiro2api 以 **Anthropic Messages 为中枢母格式**，一套后端（Kiro / CodeWhisperer 账号池）同时对外提供 OpenAI Chat、Anthropic Messages、OpenAI Responses、Gemini 原生四种协议。

## 认证

所有协议端点走**统一鉴权**。支持三种携带方式（任选其一）：

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

### 方式 3：查询参数

无法设置请求头的场景（如浏览器 `EventSource`）可用 `?token=`：

```bash
curl "http://localhost:8080/v1/models?token=sk-你的API密钥"
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

### 常见错误码

| 状态码 | 说明 |
|--------|------|
| 400 | 请求非法 / 未映射到模型（`INVALID_MODEL_ID`）—— 不瞎重试、不误伤账号 |
| 401 | 认证失败，API Key 无效或缺失（已配置 `apiKey` 时） |
| 429 | 请求过于频繁（超过 `MAX_RPM_PER_CREDENTIAL` 或上游限流） |
| 502 | 上游 Kiro 失败 |
| 503 | 无可用账号（全部冷却 / 禁用 / 超 RPM），或日志端点未启用（`logCapacity=0`） |

## 模型名映射

客户端传入的模型名按**小写子串**匹配到 Kiro 内部模型，未匹配到则返回 `400`（`INVALID_MODEL_ID`）：

| 传入含 | 解析为 |
|--------|--------|
| `sonnet`（+`4.6`/`sonnet-5`） | `claude-sonnet-4.5`（/`-4.6`/`-5`） |
| `opus`（+`4.5`/`4.7`/`4.8`） | `claude-opus-4.6`（/对应） |
| `haiku` / `fable` | `claude-haiku-4.5` / `claude-fable-5` |
| `deepseek` / `glm` / `qwen` | `deepseek-3.2` / `glm-5` / `qwen3-coder-next` |
| `minimax`（+`2.5`） | `minimax-m2.1`（/`-m2.5`） |
| `gpt`+`terra`/`luna`/`sol`/`5.6` | `gpt-5.6-terra`/`-luna`/`-sol` |
| `auto` | `auto` |

> [!TIP]
> **可用模型取决于账号订阅档位**：免费档（KIRO FREE）通常只授权 `claude-sonnet-4.5`，opus/GPT 等需更高档位。各协议 `/models` 端点返回本服务**实际可服务**的模型 id，建议客户端 list-then-use。

## OpenAI 兼容 API

### GET /v1/models

获取可用模型列表。也可使用带前缀路径 `/openai/v1/models`。

**请求**：
```bash
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-你的API密钥"
```

**响应**：
```json
{
  "object": "list",
  "data": [
    {
      "id": "claude-sonnet-4.5",
      "object": "model",
      "created": 1715970000,
      "owned_by": "kiro"
    }
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
| `max_tokens` | number | 否 | 最大输出 token 数 |
| `tools` | array | 否 | 函数调用工具列表 |
| `tool_choice` | string | 否 | 工具选择策略，`auto`/`required`/`none` |

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

**`input` 数组条目类型**：
- `{"type":"message","role":"user"|"assistant"|"system","content":[...]}` —— 内容块：`{"type":"input_text","text":...}`、`{"type":"input_image","image_url":"..."}`、`{"type":"output_text","text":...}`
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
        {"type": "output_text", "text": "1+1等于2", "annotations": []}
      ]
    }
  ],
  "usage": {
    "input_tokens": 10,
    "output_tokens": 5,
    "total_tokens": 15
  },
  "previous_response_id": null,
  "instructions": null,
  "error": null
}
```

**响应（流式）**：严格按官方协议顺序发送带命名的 SSE 事件，每个事件都带**单调递增**的 `sequence_number`。**没有** `data: [DONE]` 结尾标记（那是 Chat Completions 的老约定）——完成信号是 `response.completed`：

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

获取模型列表（Anthropic 形状，避开与 OpenAI 裸路径 `/v1/models` 冲突）。

**请求**：
```bash
curl http://localhost:8080/claude/v1/models \
  -H "Authorization: Bearer sk-你的API密钥"
```

**响应**：
```json
{
  "data": [
    {
      "id": "claude-sonnet-4.5",
      "type": "model",
      "display_name": "Claude Sonnet 4.5"
    }
  ]
}
```

### POST /v1/messages

发送消息请求（Claude 格式）。也可使用带前缀路径 `/claude/v1/messages`。

**请求体**：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `model` | string | 是 | 模型名称 |
| `messages` | array | 是 | 消息列表，`content` 支持字符串或块数组（`text`/`image`/`tool_use`/`tool_result`） |
| `max_tokens` | number | 否 | 最大输出 token 数 |
| `system` | string | 否 | 系统提示 |
| `stream` | boolean | 否 | 是否流式返回 |
| `tools` | array | 否 | 工具列表 |

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
  "stop_sequence": null,
  "usage": {
    "input_tokens": 10,
    "output_tokens": 20
  }
}
```

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

获取模型列表。也可使用带前缀路径 `/gemini/v1beta/models`。

**请求**：
```bash
curl http://localhost:8080/v1beta/models \
  -H "Authorization: Bearer sk-你的API密钥"
```

### POST /v1beta/models/{model}:generateContent

生成内容（非流式）。也可使用带前缀路径 `/gemini/v1beta/models/{model}:generateContent`。

**请求体**：`contents[]`（`parts[]` 支持 `text`/`inline_data`）、`system_instruction?`、`tools[].function_declarations`。

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
> Gemini/OpenAI 客户端一律用本服务的**统一鉴权**（Bearer / `x-api-key` / `?token=`），不是厂商原生的 `?key=`/`x-goog-api-key`。

## 管理 API

`/admin` 管理面板（静态，`rust-embed` 编译期嵌入）由 `/api/admin/*` 接口驱动。下列端点均需 `adminApiKey`（未设则回退 `apiKey`；两者皆空时管理 API 开放——切勿如此对外暴露）。鉴权携带方式同协议闸（`Authorization: Bearer` / `x-api-key` / `?token=`；无法设头的 SSE 日志流用 `?api_key=`）。响应体一律 **camelCase**，**绝不含 access/refresh token 或任何密钥**。

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
  "currentId": 12345,
  "credentials": [
    {
      "id": 12345,
      "priority": 1,
      "weight": 1,
      "disabled": false,
      "failureCount": 0,
      "isCurrent": true,
      "expiresAt": "2026-07-25T12:00:00Z",
      "authMethod": "social",
      "hasProfileArn": true,
      "successCount": 10,
      "throttleCount": 0
    }
  ]
}
```

### POST /api/admin/credentials

新增一条凭据入池并落盘。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/credentials \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的管理密钥" \
  -d '{
    "accessToken": "...",
    "refreshToken": "...",
    "expiresAt": "2026-07-25T12:00:00Z",
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

启用/禁用账号。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/disabled \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的管理密钥" \
  -d '{"disabled": true}'
```

**响应**：
```json
{
  "success": true,
  "message": "..."
}
```

### POST /api/admin/credentials/{id}/priority

设置账号优先级 / 权重。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/priority \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的管理密钥" \
  -d '{"priority": 1, "weight": 2}'
```

### POST /api/admin/credentials/{id}/reset

清失败计数 / 冷却。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/reset \
  -H "Authorization: Bearer sk-你的管理密钥"
```

### POST /api/admin/credentials/batch-import

批量导入凭据；接受数组、KAM `{accounts}` 对象或单对象；逐条规整 / 校验 / 落盘，返回逐项结果与计数。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/credentials/batch-import \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的管理密钥" \
  -d '[{"accessToken":"...","refreshToken":"...","expiresAt":"...","authMethod":"social"}]'
```

### POST /api/admin/login/builderid/start · /poll

AWS Builder ID 设备码流。`start` 返回设备码信息，`poll` 返回 `{success,completed,status,interval?,credentialId?,email?}`，成功即落库（无需手改 `credentials.json`）。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/login/builderid/start \
  -H "Authorization: Bearer sk-你的管理密钥"
```

### POST /api/admin/login/iam-sso/start · /complete

IAM Identity Center（SSO）流。`start` 返回 `{sessionId,authorizeUrl}`；`complete` 消费回调 URL（校验 `state`）后落库。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/login/iam-sso/start \
  -H "Authorization: Bearer sk-你的管理密钥"
```

### POST /api/admin/login/sso-token

批量导入原始 bearer/SSO token（每行一个）。

**请求**：
```bash
curl -X POST http://localhost:8080/api/admin/login/sso-token \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的管理密钥" \
  -d '{"tokens": "..."}'
```

**响应**：
```json
{
  "added": 2,
  "failed": [{"lineIndex": 3, "error": "..."}]
}
```

### GET /api/admin/api-keys · POST /api/admin/api-keys

列出 / 创建对外调用 API-KEY（你发给调用方的 key）。

**请求**：
```bash
curl http://localhost:8080/api/admin/api-keys \
  -H "Authorization: Bearer sk-你的管理密钥"
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
curl "http://localhost:8080/api/admin/api-keys/key-1/usage/records?page=1&page_size=20" \
  -H "Authorization: Bearer sk-你的管理密钥"
```

### GET /api/admin/credentials/{id}/usage/records · /usage/today

单账号分页用量记录 / 当日汇总。

### GET /api/admin/credentials/{id}/failure-logs · /throttle-logs

单账号近期失败 / 限流事件。

### GET /api/admin/credentials/{id}/balance

账号余额（5 分钟缓存）。

**请求**：
```bash
curl http://localhost:8080/api/admin/credentials/12345/balance \
  -H "Authorization: Bearer sk-你的管理密钥"
```

### GET /api/admin/usage/daily · /usage/daily/{date}/records

每日用量汇总 / 指定日期的记录（含客户端 IP 与账号标签）。

### GET /api/admin/rpm

实时 RPM 快照。

### GET /api/admin/config

获取脱敏配置视图（仅布尔 / 非密字段）。

**请求**：
```bash
curl http://localhost:8080/api/admin/config \
  -H "Authorization: Bearer sk-你的管理密钥"
```

### GET /api/admin/models

带 `display_name`/`type`/`max_tokens` 的模型列表（与 `/v1/models` 同源模型集）。

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

**响应**：`masterApiKey` 已脱敏（未配置则 `null`），`version` 为 kiro2api 版本，`kiroVersion` 为伪装上游 UA 版本。
```json
{
  "masterApiKey": "sk-****",
  "version": "0.1.0",
  "kiroVersion": "0.11.107"
}
```

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
- `GET /admin/api/config` — 脱敏配置
- `POST /admin/api/accounts/{id}/enable` | `disable` — 手动启停（内存态，重启复位为文件值）

## 用户 API

`/user` 用户面板（静态，`rust-embed` 嵌入）由 `/api/user/*` 驱动。这些端点**不走** admin 闸——每次请求用调用方**自己的 API-KEY** 鉴权（`x-api-key` 头，或登录 body 里的 `{apiKey}`）；handler 校验后把数据面限定到该 key。key 非法 → `401`，体 `{"error":"…"}`。响应 camelCase；`credits = cost / 0.72`。

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
  "id": "key-1",
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

该 key 的用量记录，分页（`?page=&page_size=`，降序）。

**请求**：
```bash
curl "http://localhost:8080/api/user/usage/records?page=1&page_size=20" \
  -H "x-api-key: sk-你的API密钥"
```

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
  "version": "0.1.0"
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
