# API Reference

Complete API documentation for kiro2api. All endpoints require authentication via API Key.

## Authentication

All API requests must include an API Key in one of these formats:

**Option 1: Authorization Header (Recommended)**
```
Authorization: Bearer sk-your-api-key
```

**Option 2: Custom Header**
```
x-api-key: sk-your-api-key
```

**Option 3: Query Parameter**
```
?token=sk-your-api-key
```

Keys are compared in constant time. `/health` and `/v1/ping` are liveness probes and require no authentication.

Example:
```bash
curl http://localhost:8080/health
```

## Standard Bare Paths

Each protocol supports two sets of paths:

**Prefixed paths** (explicit per-provider, used in the endpoint documentation below):
- OpenAI: `/openai/v1/chat/completions`, `/openai/v1/responses`, `/openai/v1/models`
- Claude: `/claude/v1/messages`, `/claude/v1/messages/count_tokens`, `/claude/v1/models`
- Gemini: `/gemini/v1beta/models/{model}:generateContent`, `/gemini/v1beta/models/{model}:streamGenerateContent`, `/gemini/v1beta/models`

**Standard bare paths** (major SDKs work out of the box without a suffix on `base_url`):
- OpenAI: `/v1/chat/completions`, `/v1/responses`, `/v1/models`
- Claude: `/v1/messages`, `/v1/messages/count_tokens`
- Gemini: `/v1beta/models/{model}:generateContent`, `/v1beta/models/{model}:streamGenerateContent`, `/v1beta/models`

**Important**: The bare `/v1/models` endpoint returns OpenAI format (a single path cannot return two formats). For the Claude-format model list, use `/claude/v1/models`.

## OpenAI Compatible API

These endpoints follow OpenAI API format and are compatible with OpenAI SDKs.

### GET /openai/v1/models

List available models.

**Request:**
```bash
curl http://localhost:8080/openai/v1/models \
  -H "Authorization: Bearer sk-your-api-key"
```

**Response:**
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

> 💡 **Model Availability Guide**: The model set is not fixed — it depends on the subscription tier of the Kiro (CodeWhisperer) accounts in your pool.
> - Free tier (KIRO FREE): typically authorizes only `claude-sonnet-4.5`.
> - Higher tiers unlock additional Claude-family models (opus/haiku, etc.).
> - Requesting a model your accounts cannot serve returns a clear `400` (`INVALID_MODEL_ID`) — it is **not** retried and does **not** penalize the account.
>
> Always call `/models` first (list-then-use). Incoming model names are resolved by **lowercase substring match** to an internal Kiro model; an unmatched name returns `400`. This service's streaming interface is true incremental streaming, pushing tokens as soon as they arrive over the AWS eventstream.

### POST /openai/v1/chat/completions

Generate chat completions. Supports streaming and function calling.

**Request:**
```bash
curl -X POST http://localhost:8080/openai/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {"role": "user", "content": "What is 2+2?"}
    ],
    "stream": false,
    "temperature": 0.7,
    "max_tokens": 1024
  }'
```

**Request Body:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `model` | string | Yes | Model ID (e.g., `claude-sonnet-4.5`) |
| `messages` | array | Yes | Message history with `role` and `content`. `content` can be a string or array of objects (supports multimodal) |
| `stream` | boolean | No | Enable streaming (default: false) |
| `temperature` | number | No | Randomness |
| `max_tokens` | number | No | Max response length |
| `tools` | array | No | Function definitions for tool calling (nested `{"type":"function","function":{...}}` shape) |
| `tool_choice` | string | No | `auto`, `required`, or a function name |

**Multimodal Content Format:**

`content` can be a string (text only) or array of objects (supports text and images):

```json
{
  "role": "user",
  "content": [
    {"type": "text", "text": "What is this"},
    {
      "type": "image_url",
      "image_url": {
        "url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
      }
    }
  ]
}
```

Supported content types:
- `text`: Plain text content
- `image_url`: Image supporting Base64 Data URI (`data:image/...;base64,...`)

**Response (Non-Streaming):**
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
        "content": "2 + 2 = 4"
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 10,
    "completion_tokens": 5,
    "total_tokens": 15
  }
}
```

**Response (Streaming):** `chat.completion.chunk` frames. The first frame carries `delta.role`, the last carries `finish_reason`, terminated with `data: [DONE]`:
```
data: {"choices":[{"delta":{"role":"assistant"},"index":0}]}
data: {"choices":[{"delta":{"content":"2"},"index":0}]}
data: {"choices":[{"delta":{"content":" + 2 = 4"},"index":0}]}
data: {"choices":[{"delta":{},"finish_reason":"stop","index":0}]}
data: [DONE]
```

**Function Calling Example:**
```bash
curl -X POST http://localhost:8080/openai/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {"role": "user", "content": "What is the weather in Paris?"}
    ],
    "tools": [
      {
        "type": "function",
        "function": {
          "name": "get_weather",
          "description": "Get weather for a city",
          "parameters": {
            "type": "object",
            "properties": {
              "city": {"type": "string"}
            },
            "required": ["city"]
          }
        }
      }
    ]
  }'
```

Tool calls are returned in `choices[0].message.tool_calls` with `finish_reason:"tool_calls"`. Send the tool result back with a `role:"tool"` message. Tool calls are passed through **verbatim** — no simulation.

### POST /openai/v1/responses

OpenAI Responses API. Added for clients that require the newer Responses protocol instead of Chat Completions (e.g. **Codex CLI**, which dropped Chat Completions support in Feb 2026 — pointing Codex CLI at kiro2api needs this endpoint). Supports text, streaming, and function/tool calling.

**Request:**
```bash
curl -X POST http://localhost:8080/openai/v1/responses \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "input": "What is 2+2?",
    "stream": false
  }'
```

**Request Body:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `model` | string | Yes | Model ID (e.g., `claude-sonnet-4.5`) |
| `input` | string or array | Yes | A plain string (shorthand for a single user message), or an array of input items (see below) |
| `instructions` | string | No | System/developer preamble prepended to the conversation |
| `stream` | boolean | No | Enable streaming (default: false) |
| `tools` | array | No | Function definitions for tool calling, **flat shape**: `{"type":"function","name","description","parameters"}` (note: different from Chat Completions' nested `{"type":"function","function":{...}}` shape) |
| `tool_choice` | string or object | No | `auto`, `none`, `required`, or `{"type":"function","name":"..."}` to force a specific tool |

**`input` array item types:**
- `{"type":"message","role":"user"|"assistant"|"system","content":[...]}` — content parts: `{"type":"input_text","text":...}`, `{"type":"input_image","image_url":"..."}`, `{"type":"output_text","text":...}`
- `{"type":"function_call","call_id","name","arguments"}` — a prior assistant tool-call turn (for multi-turn history you resend yourself)
- `{"type":"function_call_output","call_id","output"}` (or `"tool_result"`) — a tool's result you're sending back

**Not supported (explicit, not silent):** `previous_response_id` — this server does not keep server-side conversation state. Sending it returns a `400` `invalid_request_error` rather than silently ignoring it. Resend the full conversation in `input` on every request (this is what Codex CLI already does).

**Response (Non-Streaming):**
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
        {"type": "output_text", "text": "2 + 2 = 4", "annotations": []}
      ]
    }
  ],
  "usage": {
    "input_tokens": 10,
    "input_tokens_details": {"cached_tokens": 0},
    "output_tokens": 5,
    "output_tokens_details": {"reasoning_tokens": 0},
    "total_tokens": 15
  },
  "previous_response_id": null,
  "instructions": null,
  "error": null
}
```

**Response (Streaming):** a spec-correct sequence of named SSE events, each carrying a monotonically increasing `sequence_number`. There is **no** `data: [DONE]` sentinel (that's a Chat Completions convention) — completion is signaled by `response.completed` (or `response.failed`):

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
data: {"type":"response.output_text.delta","sequence_number":4,"delta":"2"}

event: response.output_text.done
data: {"type":"response.output_text.done","sequence_number":5,"text":"2 + 2 = 4"}

event: response.content_part.done
data: {"type":"response.content_part.done","sequence_number":6,...}

event: response.output_item.done
data: {"type":"response.output_item.done","sequence_number":7,...}

event: response.completed
data: {"type":"response.completed","sequence_number":8,"response":{...}}
```

For a tool call, `response.output_item.added` (type `function_call`) is followed by `response.function_call_arguments.delta` / `response.function_call_arguments.done` / `response.output_item.done` instead of the text events above.

**Function Calling Example:**
```bash
curl -X POST http://localhost:8080/openai/v1/responses \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "input": "What is the weather in Paris?",
    "tools": [
      {
        "type": "function",
        "name": "get_weather",
        "description": "Get weather for a city",
        "parameters": {
          "type": "object",
          "properties": {
            "city": {"type": "string"}
          },
          "required": ["city"]
        }
      }
    ]
  }'
```
Response `output` will contain a `function_call` item:
```json
{"id": "fc_xxx", "type": "function_call", "status": "completed", "call_id": "call_xxx", "name": "get_weather", "arguments": "{\"city\": \"Paris\"}"}
```

## Claude Compatible API

These endpoints follow Anthropic Claude API format. Anthropic Messages is the **internal hub format** — the other protocols are converted to and from it and reuse the same relay core.

### GET /claude/v1/models

List available models.

**Request:**
```bash
curl http://localhost:8080/claude/v1/models \
  -H "Authorization: Bearer sk-your-api-key"
```

**Response:**
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

### POST /claude/v1/messages

Generate messages using Claude API format.

**Request:**
```bash
curl -X POST http://localhost:8080/claude/v1/messages \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "max_tokens": 1024,
    "messages": [
      {"role": "user", "content": "Hello"}
    ],
    "stream": false
  }'
```

**Request Body:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `model` | string | Yes | Model ID |
| `max_tokens` | number | Yes | Maximum response tokens |
| `messages` | array | Yes | Message history. `content` is a string or an array of blocks: `text` / `image` / `tool_use` / `tool_result` |
| `system` | string | No | System prompt |
| `stream` | boolean | No | Enable streaming |
| `temperature` | number | No | Randomness |
| `tools` | array | No | Tool definitions |

**Response:**
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
    "output_tokens": 15
  }
}
```

**Response (Streaming):** standard Anthropic SSE — `message_start` → `content_block_start` → `content_block_delta` → … → `content_block_stop` → `message_stop`. Tool calls are carried via `tool_use` blocks and `input_json_delta`.

### POST /claude/v1/messages/count_tokens

Estimate token count for messages.

**Request:**
```bash
curl -X POST http://localhost:8080/claude/v1/messages/count_tokens \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {"role": "user", "content": "Hello"}
    ]
  }'
```

**Response:**
```json
{
  "input_tokens": 10
}
```

## Gemini Native API

These endpoints follow Google Gemini API format. **All fields are camelCase.**

### GET /gemini/v1beta/models

List available models.

**Request:**
```bash
curl http://localhost:8080/gemini/v1beta/models \
  -H "Authorization: Bearer sk-your-api-key"
```

**Response:**
```json
{
  "models": [
    {
      "name": "models/claude-sonnet-4.5",
      "displayName": "Claude Sonnet 4.5",
      "description": "Served via Kiro (CodeWhisperer)",
      "inputTokenLimit": 200000,
      "outputTokenLimit": 8192
    }
  ]
}
```

### POST /gemini/v1beta/models/{model}:generateContent

Generate content using Gemini format.

**Request:**
```bash
curl -X POST http://localhost:8080/gemini/v1beta/models/claude-sonnet-4.5:generateContent \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "contents": [
      {
        "role": "user",
        "parts": [{"text": "Hello"}]
      }
    ],
    "generationConfig": {
      "temperature": 0.7,
      "maxOutputTokens": 1024
    }
  }'
```

`contents[].parts` support `text` and `inline_data`; `system_instruction` and `tools[].function_declarations` are honored. Tool calls are returned as `functionCall`.

**Response:**
```json
{
  "candidates": [
    {
      "content": {
        "role": "model",
        "parts": [{"text": "Hello! How can I help?"}]
      },
      "finishReason": "STOP"
    }
  ],
  "usageMetadata": {
    "promptTokenCount": 10,
    "candidatesTokenCount": 5,
    "totalTokenCount": 15
  }
}
```

### POST /gemini/v1beta/models/{model}:streamGenerateContent

Stream content generation (SSE, `?alt=sse` shape, camelCase, no `[DONE]`).

**Request:**
```bash
curl -X POST "http://localhost:8080/gemini/v1beta/models/claude-sonnet-4.5:streamGenerateContent?alt=sse" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "contents": [{"role": "user", "parts": [{"text": "Hello"}]}]
  }'
```

**Response (SSE):**
```
data: {"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}
data: {"candidates":[{"content":{"role":"model","parts":[{"text":"!"}]}}]}
```

> [!NOTE]
> Gemini/OpenAI clients must use this service's **unified authentication** (`Authorization: Bearer` / `x-api-key` / `?token=`), **not** the vendor-native `?key=` / `x-goog-api-key`.

## Admin API

The admin panel at `/admin` (a static SPA embedded via rust-embed) is backed by the `/api/admin/*` API. Every endpoint below is authenticated with `adminApiKey` (falling back to `apiKey` if unset; if both are empty the admin API is open — do not expose such a deployment). Auth is carried the same way as the protocol gate (`Authorization: Bearer` / `x-api-key` / `?token=`, or `?api_key=` for the SSE log stream that cannot set headers). All response bodies are camelCase and **never contain access/refresh tokens or secrets**.

### GET /api/admin/credentials

Get the account pool status. This is also the implicit login check — a `200` means the key is valid.

**Request:**
```bash
curl http://localhost:8080/api/admin/credentials \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response:**
```json
{
  "total": 3,
  "available": 2,
  "currentId": 12345,
  "credentials": [
    {
      "id": 12345,
      "priority": 0,
      "weight": 1,
      "disabled": false,
      "failureCount": 0,
      "isCurrent": true,
      "expiresAt": "2026-07-25T12:00:00Z",
      "authMethod": "social",
      "hasProfileArn": true,
      "healthStatus": "healthy",
      "throttleCount": 0
    }
  ]
}
```

### POST /api/admin/credentials

Add one credential to the pool and persist it.

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/credentials \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{
    "accessToken": "...",
    "refreshToken": "...",
    "expiresAt": "2026-07-25T12:00:00Z",
    "authMethod": "social",
    "profileArn": "arn:aws:codewhisperer:us-east-1:...:profile/..."
  }'
```

### PUT /api/admin/credentials/{id}

Update an existing credential.

**Request:**
```bash
curl -X PUT http://localhost:8080/api/admin/credentials/12345 \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{"nickname": "Primary"}'
```

### DELETE /api/admin/credentials/{id}

Remove a credential from the pool.

**Request:**
```bash
curl -X DELETE http://localhost:8080/api/admin/credentials/12345 \
  -H "Authorization: Bearer sk-your-admin-key"
```

### POST /api/admin/credentials/{id}/disabled

Enable/disable an account. Body `{disabled:bool}`; returns `{success,message}`.

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/disabled \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{"disabled": true}'
```

### POST /api/admin/credentials/{id}/priority

Set an account's priority / weight.

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/priority \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{"priority": 0, "weight": 2}'
```

### POST /api/admin/credentials/{id}/reset

Clear the failure counter / cooldown for an account.

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/reset \
  -H "Authorization: Bearer sk-your-admin-key"
```

### POST /api/admin/credentials/batch-import

Bulk import credentials. Accepts an array, a KAM `{accounts}` object, or a single object; each row is normalized/validated/persisted independently.

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/credentials/batch-import \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '[
    {"accessToken":"...","refreshToken":"...","expiresAt":"...","authMethod":"social"}
  ]'
```

**Response:**
```json
{
  "added": 1,
  "failed": 0,
  "results": [{"index": 0, "success": true}]
}
```

### POST /api/admin/login/builderid/start · /api/admin/login/builderid/poll

AWS Builder ID device-code login. `start` returns a device code and verification URL; `poll` returns `{success,completed,status,interval?,credentialId?,email?}` and persists the credential on success. Brings in a new Kiro account without hand-editing `credentials.json`.

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/login/builderid/start \
  -H "Authorization: Bearer sk-your-admin-key"
```

### POST /api/admin/login/iam-sso/start · /api/admin/login/iam-sso/complete

IAM Identity Center (SSO) login. `start` returns `{sessionId,authorizeUrl}`; `complete` consumes the callback URL (validates `state`) and persists the credential.

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/login/iam-sso/start \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{"startUrl": "https://xxx.awsapps.com/start", "region": "us-east-1"}'
```

### POST /api/admin/login/sso-token

Bulk-import raw bearer/SSO tokens (one per line).

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/login/sso-token \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{"tokens": "token1\ntoken2"}'
```

**Response:**
```json
{
  "added": 2,
  "failed": []
}
```

### GET /api/admin/api-keys · POST /api/admin/api-keys

List / create the outbound API keys you hand to callers.

**Request:**
```bash
curl http://localhost:8080/api/admin/api-keys \
  -H "Authorization: Bearer sk-your-admin-key"
```

### PUT /api/admin/api-keys/{id} · DELETE /api/admin/api-keys/{id}

Update (label / limits / status) or delete an API key.

**Request:**
```bash
curl -X PUT http://localhost:8080/api/admin/api-keys/7 \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{"name": "My Key", "disabled": false}'
```

### GET /api/admin/api-keys/usage · GET /api/admin/api-keys/{id}/usage

Usage across all keys, or a single key's usage. `DELETE /api/admin/api-keys/{id}/usage` resets it. Paginated records at `GET /api/admin/api-keys/{id}/usage/records?page=&page_size=`.

**Request:**
```bash
curl http://localhost:8080/api/admin/api-keys/usage \
  -H "Authorization: Bearer sk-your-admin-key"
```

### GET /api/admin/credentials/{id}/usage/records · /usage/today

Paginated per-account usage records, or today's summary for one account.

**Request:**
```bash
curl "http://localhost:8080/api/admin/credentials/12345/usage/records?page=1&page_size=20" \
  -H "Authorization: Bearer sk-your-admin-key"
```

### GET /api/admin/credentials/{id}/failure-logs · /throttle-logs

Recent failure / throttle events for one account.

**Request:**
```bash
curl http://localhost:8080/api/admin/credentials/12345/failure-logs \
  -H "Authorization: Bearer sk-your-admin-key"
```

### GET /api/admin/credentials/{id}/balance

Get an account's balance (5-minute cached).

**Request:**
```bash
curl http://localhost:8080/api/admin/credentials/12345/balance \
  -H "Authorization: Bearer sk-your-admin-key"
```

### GET /api/admin/usage/daily · /usage/daily/{date}/records

Daily usage summary, or records for a specific day (includes client IP and account label).

**Request:**
```bash
curl http://localhost:8080/api/admin/usage/daily \
  -H "Authorization: Bearer sk-your-admin-key"
```

### GET /api/admin/rpm

Live requests-per-minute snapshot.

**Request:**
```bash
curl http://localhost:8080/api/admin/rpm \
  -H "Authorization: Bearer sk-your-admin-key"
```

### GET /api/admin/config

Get a redacted config view (booleans / non-secret fields only).

**Request:**
```bash
curl http://localhost:8080/api/admin/config \
  -H "Authorization: Bearer sk-your-admin-key"
```

### GET /api/admin/models

Model list with `display_name` / `type` / `max_tokens` (same model set as `/v1/models`).

**Request:**
```bash
curl http://localhost:8080/api/admin/models \
  -H "Authorization: Bearer sk-your-admin-key"
```

### GET /api/admin/config/load-balancing · PUT

Read / change the load-balancing mode at runtime (`priority` / `balanced`), persisted to `config.json`.

**Request:**
```bash
curl -X PUT http://localhost:8080/api/admin/config/load-balancing \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{"mode": "balanced"}'
```

### GET /api/admin/config/auth-keys · PUT

Read (masked) / rotate `apiKey` and `adminApiKey` at runtime; takes effect immediately (no restart).

**Request:**
```bash
curl -X PUT http://localhost:8080/api/admin/config/auth-keys \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{"apiKey": "sk-new-key"}'
```

### GET /api/admin/server-info

`{masterApiKey,version,kiroVersion}`. `masterApiKey` is **masked** (or `null` when unset), `version` is the kiro2api version, `kiroVersion` is the spoofed upstream UA version.

**Request:**
```bash
curl http://localhost:8080/api/admin/server-info \
  -H "Authorization: Bearer sk-your-admin-key"
```

### GET /api/admin/logs/stream · /snapshot · /download

Live logs (requires `logCapacity > 0`; otherwise `503`).

- `GET /api/admin/logs/stream` — SSE stream (history event, then per-line log events with heartbeats). EventSource cannot set headers, so authenticate with `?api_key=<admin key>`.
- `GET /api/admin/logs/snapshot` — current buffer as a JSON array.
- `GET /api/admin/logs/download` — buffer as a `.txt` attachment.

**Request:**
```bash
curl "http://localhost:8080/api/admin/logs/stream?api_key=sk-your-admin-key"
```

> [!NOTE]
> The service self-heals its accounts: an expired token is refreshed **in memory** (single-flight coordinated) and the result is atomically written back to `credentials.json`. Only genuine credential invalidation permanently disables an account; quota / throttle / transient failures are cooled down and retried automatically, with endpoint fallback (Kiro IDE → CodeWhisperer → AmazonQ) and cross-account retry.

### Legacy admin endpoints (kept for backward compatibility)

- `GET /admin/api/stats` — `{accounts:[…], summary:{total,active,disabled,in_cooldown}}`.
- `GET /admin/api/config` — redacted config.
- `POST /admin/api/accounts/{id}/enable` | `disable` — manual enable/disable (in-memory; resets to the file value on restart).

## User API

The user panel at `/user` (a static SPA embedded via rust-embed) is backed by `/api/user/*`. These endpoints are **not** behind the admin gate — each request authenticates with the caller's **own API-KEY** (`x-api-key` header, or `{apiKey}` in the login body); the handler validates it and scopes all data to that key. Invalid key → `401`. Responses are camelCase; `credits = cost / 0.72`.

### POST /api/user/login

Validate the key.

**Request:**
```bash
curl -X POST http://localhost:8080/api/user/login \
  -H "Content-Type: application/json" \
  -d '{"apiKey": "sk-your-api-key"}'
```

**Response:**
```json
{
  "id": 7,
  "name": "My Key",
  "spendingLimit": 100,
  "limitUnit": "usd",
  "totalCost": 1.23,
  "totalCredits": 1.71,
  "expiresAt": null,
  "durationDays": null,
  "activatedAt": "2026-07-25T00:00:00Z"
}
```

### GET /api/user/usage · /usage/records

That key's usage summary (including `byModel[]`), or paginated usage records (`?page=&page_size=`, newest first).

**Request:**
```bash
curl http://localhost:8080/api/user/usage \
  -H "x-api-key: sk-your-api-key"
```

## System Endpoints

### GET /health

Health check endpoint (no authentication required).

**Request:**
```bash
curl http://localhost:8080/health
```

**Response:**
```json
{
  "service": "kiro2api",
  "status": "ok",
  "version": "0.1.0"
}
```

### GET /v1/ping

Liveness probe (no authentication required).

**Request:**
```bash
curl http://localhost:8080/v1/ping
```

**Response:**
```json
{
  "pong": true
}
```

## Error Responses

The error body shape varies by protocol:

- **Anthropic**: `{"type":"error","error":{"type","message"}}`
- **OpenAI / Responses**: `{"error":{"message","type","code"}}`
- **Gemini**: `{"error":{"code","message","status"}}`

### HTTP Status Codes

| Code | Meaning | Description |
|------|---------|-------------|
| 200 | OK | Request succeeded |
| 400 | Bad Request | Unmapped model (`INVALID_MODEL_ID`) or malformed request |
| 401 | Unauthorized | Missing or invalid API Key (when `apiKey` is configured) |
| 502 | Bad Gateway | Upstream Kiro / CodeWhisperer failure |
| 503 | Service Unavailable | No account available (all in cooldown / disabled / over RPM) |

### Common Error Causes

| Cause | Meaning | Solution |
|-------|---------|----------|
| Invalid API Key | Key missing or wrong | Verify the key in the `Authorization` / `x-api-key` header or `?token=` |
| `INVALID_MODEL_ID` | Model not served by your pool | Check available models with `/v1/models`; the account is **not** penalized |
| No account available | All accounts in cooldown / disabled / over RPM | Add accounts, wait for cooldown, or reset failure counters |
| Upstream failure | Kiro / CodeWhisperer / AmazonQ error | Endpoint fallback and cross-account retry are automatic; check the admin logs |

## Rate Limiting

Per-credential RPM limiting is configurable via `MAX_RPM_PER_CREDENTIAL` (`0` = unlimited). Each account also has its own graded cooldown after consecutive failures, classified by category (permanent invalidation / ambiguous auth / quota / transient) so only true credential failures disable an account.

```env
MAX_RPM_PER_CREDENTIAL=60
```

When an account is over its RPM or in cooldown, the pool rotates to the next available account; if none is available the request returns `503`.

## Streaming

All four protocols support streaming. The service decodes the AWS eventstream from the upstream and re-encodes it into each protocol's native format:

**OpenAI Chat (SSE):**
```
data: {"choices":[{"delta":{"content":"text"},"index":0}]}
data: [DONE]
```

**OpenAI Responses (named SSE events, monotonic `sequence_number`, no `[DONE]`):**
```
event: response.output_text.delta
data: {"type":"response.output_text.delta","sequence_number":4,"delta":"text"}
```

**Anthropic (SSE):**
```
event: content_block_delta
data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"text"}}
```

**Gemini (SSE, `?alt=sse`, camelCase):**
```
data: {"candidates":[{"content":{"role":"model","parts":[{"text":"text"}]}}]}
```

Set `stream: true` in the request body to enable streaming. When `stream: false`, the service still decodes the full event stream internally and returns one complete JSON response.

## Related Documentation

- [README](README.md) — overview, features, and quick deploy
- [USAGE](USAGE.md) — client integration guide
- [DEPLOY](DEPLOY.md) — deployment and configuration
- [Root README](../../README.md)
