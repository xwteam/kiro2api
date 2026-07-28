# API Reference

Complete API documentation for kiro2api. All endpoints require authentication via API Key.

## Authentication

All API requests must include an API Key. The gate accepts **six** channels and takes the first one present, in this priority order:

**Option 1: Authorization Header (Recommended)**
```
Authorization: Bearer sk-your-api-key
```

**Option 2: Custom Header**
```
x-api-key: sk-your-api-key
```

**Option 3: Gemini-Native Header**
```
x-goog-api-key: sk-your-api-key
```

**Options 4-6: Query Parameters** (for clients that cannot set headers, e.g. browser `EventSource`; the Gemini SDKs use `?key=`)
```
?api_key=sk-your-api-key
?token=sk-your-api-key
?key=sk-your-api-key
```

Headers outrank query parameters, and within each group the order above decides: `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > `?api_key=` > `?token=` > `?key=`. Keys are compared in constant time. `/health` and `/v1/ping` are liveness probes and require no authentication.

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

**Response** (abridged — the list has 17 entries):
```json
{
  "object": "list",
  "data": [
    {"id": "claude-sonnet-4.5", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "claude-sonnet-4.6", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "claude-sonnet-5", "object": "model", "created": 1700000000, "owned_by": "kiro2api"}
  ]
}
```

`created` is the hard-coded constant `1700000000` on every entry (not a real publication date), and `owned_by` is the literal `"kiro2api"`.

> ℹ️ **All three protocol lists serve the same catalog.** `GET /openai/v1/models` (and bare `/v1/models`), `GET /claude/v1/models` and `GET /gemini/v1beta/models` are rendered from one catalog compiled into the binary, in this order:
> `claude-sonnet-4.5`, `claude-sonnet-4.6`, `claude-sonnet-5`, `claude-opus-4.5`, `claude-opus-4.6`, `claude-opus-4.7`, `claude-opus-4.8`, `claude-haiku-4.5`, `claude-fable-5`, `deepseek-3.2`, `glm-5`, `qwen3-coder-next`, `minimax-m2.1`, `minimax-m2.5`, `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.6-sol`.
> Only the per-protocol JSON shape differs; the ids and their order are identical. **List-then-use works**: every id above is one the gateway's model resolution accepts, so an id taken from this response never trips the gateway's own `400`.
>
> The list is still **compiled in, not derived from your pool**: it does not query your accounts and is therefore not filtered by subscription tier. The pool-aware view is `GET /api/admin/models`, which prefers the live per-account capability union once accounts have been probed and falls back to this very same catalog otherwise.

> 💡 **Model Availability Guide**: which models actually *work* depends on the subscription tier of the Kiro (CodeWhisperer) accounts in your pool.
> - Free tier (KIRO FREE): typically authorizes only `claude-sonnet-4.5`.
> - Higher tiers unlock additional Claude-family models (opus/haiku, etc.).
> - Requesting a model your accounts cannot serve returns a clear `400` (`INVALID_MODEL_ID`) — it is **not** retried and does **not** penalize the account.
>
> So a listed id always clears the gateway but is still no guarantee *upstream*, and an unlisted id is not necessarily rejected: incoming model names are resolved by **lowercase substring match** to one of 18 internal Kiro model ids (the 17 catalog ids plus the routing alias `auto`, which the catalog does not list), and only a name matching none of them is rejected with `400` by the gateway itself. Be ready to handle `400` (`INVALID_MODEL_ID`) for any id whose tier your pool does not have. This service's streaming interface is true incremental streaming, pushing tokens as soon as they arrive over the AWS eventstream.

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
    "stream": false
  }'
```

**Request Body:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `model` | string | Yes | Model ID (e.g., `claude-sonnet-4.5`) |
| `messages` | array | Yes | Message history with `role` and `content`. `content` can be a string or array of objects (supports multimodal) |
| `stream` | boolean | No | Enable streaming (default: false) |
| `tools` | array | No | Function definitions for tool calling (nested `{"type":"function","function":{...}}` shape) |
| `temperature` | number | No | **Accepted and ignored** — see the note below |
| `max_tokens` | number | No | **Accepted and ignored** — see the note below |
| `tool_choice` | string or object | No | **Accepted and ignored** — see the note below |

> ⚠️ **Generation controls do not take effect.** The Kiro data plane this service relays to has no sampling or budget fields on the wire, so nothing is forwarded and nothing is rejected either — the request still returns `200` with a normal answer:
> - `temperature` (and `top_p`, and every other sampling knob) is not even a field on the request struct. It is discarded as an unknown key during deserialization. Setting `temperature: 0` gives you neither determinism nor an error.
> - `max_tokens` is parsed but never sent upstream, so it does **not** cap the answer. A `finish_reason` of `"length"` reports the *upstream's own* output budget or an exhausted context window — never a limit you set here.
> - `tool_choice` is parsed but never sent upstream. You cannot force (`required`, or a named function) or forbid (`none`) a tool on this protocol; the model decides on its own. The only place any such hint is honored is the Gemini front end — see [`generationConfig` / `toolConfig`](#post-geminiv1betamodelsmodelgeneratecontent).
>
> These fields are listed because clients and SDKs already send them and doing so is harmless. Do not build behavior on them.

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
- `image_url`: Image — **Base64 Data URI only** (`data:image/...;base64,...`)

> ⚠️ A remote `http(s)://` image URL is **rejected**, not fetched and not silently dropped: the request fails with `400` `invalid_request_error` telling you to inline the image as a `data:` URL. Download and base64-encode the image yourself before sending it. Anthropic `image` blocks follow the same rule (`source.type` must be `base64`; `source.type: "url"` is the same `400`). The Gemini front end has no remote-image field at all — images ride only in `inlineData`, which is base64 by definition.

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
| `tool_choice` | string or object | No | **Accepted and ignored** — parsed, never sent upstream. `required` / `none` / `{"type":"function","name":"..."}` do not force or forbid anything; the model decides |
| `max_output_tokens` | number | No | **Accepted and ignored** — parsed, never sent upstream, so it does not cap the answer. See the [generation-controls note](#post-openaiv1chatcompletions) |

**`input` array item types** — exactly three are accepted:
- `{"type":"message","role":"user"|"assistant"|"system","content":[...]}` — content parts: `{"type":"input_text","text":...}`, `{"type":"input_image","image_url":"..."}`, `{"type":"output_text","text":...}`. `type` may also be **omitted entirely** on a message item (`{"role":"user","content":"hi"}` works, which is what the official SDKs emit)
- `{"type":"function_call","call_id","name","arguments"}` — a prior assistant tool-call turn (for multi-turn history you resend yourself)
- `{"type":"function_call_output","call_id","output"}` — a tool's result you're sending back

> ⚠️ **This spelling only.** There is no `tool_result` item type and no other alias. Any item carrying an unrecognized `type` fails deserialization, and because `input` is parsed as a whole, **one bad item rejects the entire request** with `400` — the rest of the conversation is not partially accepted. Extra keys *within* an accepted item (`id`, `status`, … as returned in a previous response's `output`) are ignored, so replaying prior output items verbatim is fine.

**Not supported (explicit, not silent):** `previous_response_id` — this server does not keep server-side conversation state. Sending a **non-empty** value returns a `400` `invalid_request_error` rather than silently ignoring it (an explicit `null` or `""` is treated as absent and passes). Resend the full conversation in `input` on every request (this is what Codex CLI already does).

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
        {"type": "output_text", "text": "2 + 2 = 4"}
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
> That is the **complete** shape — this server emits no other top-level keys. In particular `previous_response_id`, `instructions` and `error` are **absent**, not `null`, and an `output_text` part carries no `annotations`; `usage` has exactly the three counters shown, with no `input_tokens_details` / `output_tokens_details` breakdown. Read them defensively (`.get(...)`) if your client library expects the full OpenAI shape.
>
> One optional key can appear: when the upstream truncates the answer, `status` is `"incomplete"` (and so is the message item's `status`) and `incomplete_details` is added as `{"reason": "max_output_tokens"}`. It is omitted entirely on a `"completed"` response. Both an output-budget cut-off and an exhausted context window report that same reason.

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

List available models. Same 17-entry catalog as the other protocol `/models` endpoints, in the same order, rendered in Anthropic's shape — see the note under [GET /openai/v1/models](#get-openaiv1models). `has_more` is always `false` (there is no cursor pagination), `first_id` / `last_id` are the catalog's first and last ids, and `created_at` is the hard-coded constant `2026-01-01T00:00:00Z` on every entry.

**Request:**
```bash
curl http://localhost:8080/claude/v1/models \
  -H "Authorization: Bearer sk-your-api-key"
```

**Response** (abridged — `data` has 17 entries):
```json
{
  "data": [
    {"type": "model", "id": "claude-sonnet-4.5", "display_name": "Claude Sonnet 4.5", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "claude-sonnet-4.6", "display_name": "Claude Sonnet 4.6", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "claude-sonnet-5", "display_name": "Claude Sonnet 5", "created_at": "2026-01-01T00:00:00Z"}
  ],
  "has_more": false,
  "first_id": "claude-sonnet-4.5",
  "last_id": "gpt-5.6-sol"
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
| `messages` | array | Yes | Message history. `content` is a string or an array of blocks: `text` / `image` / `tool_use` / `tool_result` |
| `system` | string or array | No | System prompt. Both a bare string and Anthropic's content-block array form (`[{"type":"text","text":"…"}]`, as sent by Claude Code and prompt-caching SDKs) are accepted |
| `stream` | boolean | No | Enable streaming |
| `tools` | array | No | Tool definitions |
| `max_tokens` | number | No | **Accepted and ignored** — see the note below |
| `temperature` | number | No | **Accepted and ignored** — see the note below |

> ⚠️ Same as on the OpenAI front end (see the [generation-controls note](#post-openaiv1chatcompletions)): `temperature` is not a field on the request struct and is dropped as an unknown key, and `max_tokens` is parsed but never forwarded to the upstream, so it does not cap the answer. A `stop_reason` of `"max_tokens"` reports the *upstream's own* output budget — not a limit you set — and an exhausted context window comes back as `"model_context_window_exceeded"`. (`stop_reason` is otherwise `"tool_use"` when the turn contains tool calls, else `"end_turn"`.)
>
> Note also that **this server does not require `max_tokens`** — omitting it returns `200`, it does not `400`/`422`. The official Anthropic SDKs still require it client-side, so keep sending it when you use one; the examples here do.

**Response:**
```json
{
  "id": "msg_xxx",
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

These endpoints follow Google Gemini API format. **Responses are always camelCase**; requests accept camelCase and, like the official API, the proto snake_case aliases as well.

### GET /gemini/v1beta/models

List available models. Same 17-entry catalog as the other protocol `/models` endpoints, in the same order — see the note under [GET /openai/v1/models](#get-openaiv1models). Each entry carries only `name` (the catalog id prefixed with `models/`) and `supportedGenerationMethods` (the same two methods on every entry); `displayName` is omitted, and there are no `description` / `inputTokenLimit` / `outputTokenLimit` fields.

**Request:**
```bash
curl http://localhost:8080/gemini/v1beta/models \
  -H "Authorization: Bearer sk-your-api-key"
```

**Response** (abridged — `models` has 17 entries):
```json
{
  "models": [
    {"name": "models/claude-sonnet-4.5", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/claude-sonnet-4.6", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/claude-sonnet-5", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]}
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
    ]
  }'
```

`contents[].parts` support `text` and `inlineData`; `systemInstruction` and `tools[].functionDeclarations` are honored. Tool calls are returned as `functionCall`. Inbound keys are accepted in both spellings — camelCase and the proto snake_case aliases (`system_instruction`, `tool_config`, `generation_config`, `max_output_tokens`, `inline_data`, `mime_type`, `function_call`, `function_response`, `function_declarations`) — but responses are always camelCase.

> ⚠️ **`generationConfig` has no effect.** Only `maxOutputTokens` is even parsed (every other key, `temperature` included, is dropped as an unknown field), and the parsed value is never forwarded upstream — so it does not cap the answer. A `finishReason` of `"MAX_TOKENS"` reports the *upstream's own* budget or an exhausted context window, not a limit you set. Nothing here errors; the request just returns `200` with an ordinary answer.
>
> **`toolConfig` is the one partial exception.** `functionCallingConfig.mode: "NONE"` **is** honored (the mode string is compared case-insensitively) — the tool specs are withheld from the upstream, so the model cannot call a function on that turn. `"AUTO"` is the default behavior anyway. `"ANY"` (force at least one call) **cannot** be expressed on the upstream wire and silently falls back to the default — do not rely on it. Nothing else in `toolConfig` is forwarded.

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
> The key may ride on any channel the gate accepts, in this priority order: `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > query (`?api_key=` > `?token=` > `?key=`). The Gemini-native `x-goog-api-key` header and `?key=` parameter **are** honored, so the official `google-genai` SDK works with nothing but a `base_url` swap. What has to change is the *value*: always pass **this service's** API key, never a real Google/OpenAI vendor key.

## Admin API

The admin panel at `/admin` (a static SPA embedded via rust-embed) is backed by the `/api/admin/*` API. Every endpoint below is authenticated with `adminApiKey` (falling back to `apiKey` if unset; if both are empty the admin API is open — do not expose such a deployment). Auth is carried the same way as the protocol gate — all six channels apply (`Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > `?api_key=` > `?token=` > `?key=`); use `?api_key=` for the SSE log stream, which cannot set headers. Response bodies are camelCase **except `GET /api/admin/config` and `GET /api/admin/models`, which are snake_case to match the panel's data model**. The legacy endpoints are snake_case throughout: `GET /admin/api/config` serves the very same view as `/api/admin/config`, and in `GET /admin/api/stats` **both** the `summary` and every entry of `accounts[]` are snake_case (`last_used_unix`, `in_cooldown`, `auth_method`, `expires_at_unix`, `has_profile_arn`, `cooldown_until`) — do not parse them as camelCase. Admin responses **never contain account access/refresh tokens** (`GET /api/admin/credentials` exposes status only).

> [!WARNING]
> Admin responses are **not** secret-free. `GET`/`POST /api/admin/api-keys` return every outbound key as full plaintext in the `key` field, and `GET /api/admin/server-info` returns `masterApiKey` as full plaintext; only `GET /api/admin/config/auth-keys` and `GET /api/admin/config` are masked. There is no read-only admin role — anything holding the admin key can read, create and rotate every key. Treat admin responses as secrets: do not paste them into issues, logs or third-party tooling.

> [!NOTE]
> **Every paginated admin endpoint shares one convention.** The query keys are `?page=` and `?page_size=` — **snake_case**, matching the request struct; a camelCase `pageSize` in the query string is simply an unknown key and the default applies. Defaults are `page=1` and `page_size=20`. `page_size` is raised to at least `1`, `page` is clamped into `[1, totalPages]`, and the response envelope is camelCase: `{records, total, page, pageSize, totalPages}`. An unknown id or an empty store is **not** a `404` — it answers `records: []` with `total: 0`, `page: 1`, `totalPages: 0`.
> This applies to `/api/admin/credentials/{id}/usage/records`, `/failure-logs`, `/throttle-logs`, `/api/admin/usage/daily/{date}/records` and `/api/admin/api-keys/{id}/usage/records`. The user-facing `GET /api/user/usage/records` uses the same shape and query keys but defaults to `page_size=50`.

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
      "successCount": 128,
      "lastUsedAt": "2026-07-25T11:59:00Z",
      "hasProxy": false,
      "healthStatus": "healthy",
      "statusReason": "none",
      "throttleCount": 0
    }
  ]
}
```

`statusReason` records **why** the last failure happened — `none` / `banned` (upstream suspended the account) / `quota` / `token_expired` / `throttled` / `refresh_denied` — and is orthogonal to `healthStatus`, which answers whether the account is selectable. It is derived from the raw upstream response body, affects presentation only (never selection, cooldown length or the disable decision), and clears on the account's next success.

> [!NOTE]
> Every field shown above always serializes. Two further fields, `email` and `nickname`, appear only when the account actually carries them. `expiresAt` and `lastUsedAt` are always present but hold `null` when the underlying timestamp is unset.
>
> The pool picks an account per request, so there is no persistent "current" account: `currentId` is **always** `-1` and `isCurrent` is **always** `false`. Both fields are reserved for a possible sticky-selection mode — do not branch on them. `hasProxy` is the same kind of placeholder: it is hard-coded `false` for every account (the credential model carries no proxy field at all), so it never reports a per-account setting either.
>
> `priority` is not a separate value: it is echoed straight from `weight`, so the two are always equal. `healthStatus` is one of `disabled` | `unhealthy` (in cooldown) | `warning` (has failure strikes) | `healthy`.

### POST /api/admin/credentials

Add one credential to the pool and persist it.

Only `refreshToken` is required (plus `clientId` + `clientSecret` when `authMethod` is `idc`). The access token and its expiry are **not accepted here** — they are left empty and filled in by the first automatic refresh. Unknown keys (including `accessToken` / `expiresAt`) are silently ignored, not rejected, so a request carrying them still returns `200` while those values are dropped. Other optional keys: `authMethod`, `email`, `nickname`, `profileArn`, `priority`, `weight`, `authRegion`, `apiRegion`, `machineId`.

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/credentials \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{
    "refreshToken": "...",
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

Set an account's priority. Priority **is** the pool weight (higher = larger share under `balanced`); values below `1` are clamped to `1`.

Body is `{"priority": <int>}` only. `priority` is required — omitting it returns `422`. There is **no** separate `weight` field on this endpoint; an extra `weight` key is silently ignored. To set the weight explicitly, use `PUT /api/admin/credentials/{id}` with `{"weight": N}`.

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/priority \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{"priority": 2}'
```

### POST /api/admin/credentials/{id}/reset

Clear the failure counter / cooldown for an account.

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/reset \
  -H "Authorization: Bearer sk-your-admin-key"
```

### POST /api/admin/credentials/batch-import

Bulk import credentials. The payload is always wrapped in a required `data` key — posting a bare array returns `422`. `data` itself may be an array, a KAM `{accounts: [...]}` object, or a single object; each row is normalized/validated/persisted independently. As with the single-credential endpoint, only `refreshToken` (plus `clientId`/`clientSecret` for `idc`) matters — `accessToken` / `expiresAt` are ignored during normalization.

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/credentials/batch-import \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{
    "data": [
      {"refreshToken":"...","authMethod":"social","email":"a@x.io"}
    ]
  }'
```

**Response:**
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

Per-item `index` is **1-based**. `status` is `added` | `duplicate` | `failed`; `credentialId` / `email` / `error` are omitted when absent. There is **no** per-item `success` field — filter on `status` instead. Top-level `success` is `true` only when `failed == 0`.

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

`bearerToken` is required (omitting it returns `422`) and holds the **whole newline-separated blob** — the server splits it per line, skips blank lines and caps the batch at 200 entries. `region` is optional and falls back to the hard-coded literal `us-east-1` — **not** to the server's configured `REGION`, so a deployment started with another region must pass it explicitly. Per-line failures come back in `failed[]` as `{lineIndex, error}` with the **0-based** line number of the original input.

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/login/sso-token \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{"bearerToken": "token1\ntoken2", "region": "us-east-1"}'
```

**Response:**
```json
{
  "added": 2,
  "failed": []
}
```

### GET /api/admin/api-keys · POST /api/admin/api-keys

List / create the outbound API keys you hand to callers. Both responses carry each key's `key` field as **full plaintext** — the panel needs the real value for its copy button and masks it client-side only.

**Request:**
```bash
curl http://localhost:8080/api/admin/api-keys \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response:**
```json
[
  {
    "id": 1,
    "key": "sk-c8a63d2e6323ca12efd128144f621e8f",
    "name": "My Key",
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

Update (label / limits / status) or delete an API key. `{id}` is the numeric key id from the list response, not the `sk-…` key string.

Accepted body fields: `name`, `enabled`, `expiresAt`, `spendingLimit`, `limitUnit`, `durationDays`, `boundCredentialIds`. All are optional — an omitted field is left unchanged, while an explicit `null` on `expiresAt` / `spendingLimit` / `durationDays` / `boundCredentialIds` clears it. Unknown keys are silently ignored, so note that the status flag here is **`enabled`** (API keys), not the `disabled` used by the credential endpoints — sending `disabled` returns `200` without changing anything.

**Request:**
```bash
curl -X PUT http://localhost:8080/api/admin/api-keys/7 \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{"name": "My Key", "enabled": false}'
```

### GET /api/admin/api-keys/usage · GET /api/admin/api-keys/{id}/usage

Usage across all keys, or a single key's usage. `DELETE /api/admin/api-keys/{id}/usage` resets it. Paginated records at `GET /api/admin/api-keys/{id}/usage/records?page=&page_size=`.

**Request:**
```bash
curl http://localhost:8080/api/admin/api-keys/usage \
  -H "Authorization: Bearer sk-your-admin-key"
```

### GET /api/admin/credentials/{id}/usage/records · /usage/today

Paginated per-account usage records (newest first), or today's summary for one account. Both take the numeric account id in the path and neither `404`s on an unknown id — records answers an empty page, `usage/today` answers a zero summary.

**Request:**
```bash
curl "http://localhost:8080/api/admin/credentials/12345/usage/records?page=1&page_size=20" \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response** (`/usage/records`):
```json
{
  "records": [
    {
      "model": "claude-sonnet-4.5",
      "inputTokens": 1200,
      "outputTokens": 340,
      "estimatedCost": 0.0123,
      "creditsUsed": 0.0171,
      "createdAt": "2026-07-25T11:59:00Z",
      "credentialId": 12345,
      "credentialLabel": "Primary",
      "clientIp": "203.0.113.7"
    }
  ],
  "total": 1,
  "page": 1,
  "pageSize": 20,
  "totalPages": 1
}
```

`model`, `inputTokens`, `outputTokens`, `estimatedCost`, `createdAt` and `credentialId` always serialize; `creditsUsed`, `cacheReadInputTokens`, `cacheCreationInputTokens`, `clientIp` and `credentialLabel` are omitted when absent. `credentialLabel` is resolved from the pool as nickname → email → `#{id}`. `creditsSaved` is declared on the record but never produced, so it is always absent.

**Request** (`/usage/today`):
```bash
curl http://localhost:8080/api/admin/credentials/12345/usage/today \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response:**
```json
{
  "date": "2026-07-28",
  "credentialId": 12345,
  "totalRequests": 42,
  "totalInputTokens": 50000,
  "totalOutputTokens": 12000,
  "totalCost": 0.42,
  "totalCredits": 0.58
}
```

`date` is the **UTC+8** calendar day, and so is the window being summed — the day rolls over at 16:00 UTC, not at midnight UTC. `totalCreditsSaved` is declared but never produced, so it is always absent. This endpoint takes no query parameters.

### GET /api/admin/credentials/{id}/failure-logs · /throttle-logs

Recent failure / throttle events for one account, newest first. `failure-logs` holds the auth failures (`statusCode` is the real `401` / `403`), `throttle-logs` holds the upstream throttles (`statusCode` is the hard-coded constant `429`). Both share one response shape and the paginated-endpoint convention above, and both answer an empty page instead of `404` for an unknown id.

**Request:**
```bash
curl "http://localhost:8080/api/admin/credentials/12345/throttle-logs?page=1&page_size=20" \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response:**
```json
{
  "records": [
    {
      "credentialId": 12345,
      "requestType": "api",
      "statusCode": 429,
      "responseBody": "<upstream response body, truncated>",
      "createdAt": "2026-07-25T11:59:00Z"
    }
  ],
  "total": 1,
  "page": 1,
  "pageSize": 20,
  "totalPages": 1
}
```

`requestType` is the literal `"api"` on every event today — nothing else is ever recorded. `responseBody` is the upstream body truncated to 2000 characters for failures and to **200** characters for throttles. Both logs keep at most 500 events per account (oldest dropped), so they are a recent-history window, not a complete audit trail.

### GET /api/admin/credentials/{id}/balance

Get an account's balance (5-minute cached).

**Request:**
```bash
curl http://localhost:8080/api/admin/credentials/12345/balance \
  -H "Authorization: Bearer sk-your-admin-key"
```

### GET /api/admin/credits/global

Aggregate remaining credits across the pool. **Read-only over the shared balance cache — it never calls upstream.**

For every account in the pool it reads that account's still-fresh (5-minute TTL) balance snapshot and sums `remaining`; accounts whose entry is missing or stale are skipped silently, not fetched. So `cachedCount` < `totalCount` simply means the rest have not been warmed yet — warm them via `GET /api/admin/credentials/{id}/balance` (or by opening the accounts page). Takes no parameters and always returns `200`.

**Request:**
```bash
curl http://localhost:8080/api/admin/credits/global \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response:**
```json
{
  "globalCredits": 1234.5,
  "cachedCount": 2,
  "totalCount": 3,
  "oldestCacheUnix": 1785000000
}
```

`oldestCacheUnix` is the fetch time of the oldest snapshot that went into the sum (for an "updated N ago" label); it is `null` — present, not omitted — when nothing was cached, in which case `globalCredits` is `0` and `cachedCount` is `0`.

### GET /api/admin/usage/daily · /usage/daily/{date}/records

Daily usage summary, or records for a specific day (includes client IP and account label).

`GET /api/admin/usage/daily` takes no parameters and returns a **bare JSON array**, newest day first. `GET /api/admin/usage/daily/{date}/records` takes the day as `YYYY-MM-DD` in the path — a **UTC+8** calendar day, the same day key `usage/today` reports — plus the shared `?page=&page_size=` convention. The day's records are capped at the newest 2000 before pagination, so `total` never exceeds 2000 however busy the day was. An unknown or malformed date is not an error: it answers an empty page.

**Request:**
```bash
curl http://localhost:8080/api/admin/usage/daily \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response:**
```json
[
  {"date": "2026-07-28", "totalRequests": 128, "totalCost": 1.23, "totalCredits": 1.71}
]
```

`totalCreditsSaved` is declared but never produced, so it is always absent, and there are no token totals on this rollup — only requests / cost / credits.

**Request** (one day's records):
```bash
curl "http://localhost:8080/api/admin/usage/daily/2026-07-28/records?page=1&page_size=20" \
  -H "Authorization: Bearer sk-your-admin-key"
```

The response is the same envelope and the same record fields as [`/api/admin/credentials/{id}/usage/records`](#get-apiadmincredentialsidusagerecords--usagetoday), across all accounts rather than one.

### GET /api/admin/usage/summary

Aggregate usage over a time window, with a bucketed series for charting and best-effort health metrics.

The window is chosen by **one** of two query parameters: `?range=` takes the enum `6h` | `24h` | `3d` | `7d` | `30d` and wins when both are given; `?hours=` takes any positive integer. With neither, the window is `24h`. An unrecognized `range` or `hours=0` returns `400` with `{"error": "...", ...}` (`invalid range` also carries `allowed` and `hint`). `bucketSecs` is derived, never passed in: `3600` for windows up to 24h, `86400` beyond.

**Request:**
```bash
curl "http://localhost:8080/api/admin/usage/summary?range=24h" \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response** (`series` abridged):
```json
{
  "range": "24h",
  "windowSecs": 86400,
  "sinceUnix": 1784913600,
  "untilUnix": 1785000000,
  "bucketSecs": 3600,
  "totalRequests": 128,
  "totalInputTokens": 500000,
  "totalOutputTokens": 120000,
  "totalCost": 1.23,
  "totalCredits": 1.71,
  "dailyFallbackApplied": false,
  "series": [
    {"bucketStartUnix": 1784916000, "totalRequests": 12, "totalCost": 0.11, "totalCredits": 0.15}
  ],
  "successfulRequests": 128,
  "failedRequests": 3,
  "errorRate": 0.0229,
  "avgLatencyMs": 2450.0,
  "rotationSuccessRate": 0.9771
}
```

Every field above always serializes; an empty window returns zeros with `series: []` (still `200`), `series` is ordered by bucket start ascending, and `range` echoes the effective label (`"<N>h"` when you passed `hours`). Read the derived numbers with their caveats:

- `successfulRequests` counts the window's usage records (one per relayed request, including anything added by the daily top-up below); `failedRequests` counts auth-failure plus throttle events in it. Those event logs are capped per account, so `failedRequests` is a **lower bound** and `errorRate` errs low.
- `errorRate` = `failedRequests / (successfulRequests + failedRequests)`, `rotationSuccessRate` is its complement — it is an approximation of "requests that eventually reached upstream", not an instrumented count of cross-account retries. With no activity at all they are `0.0` and `1.0`.
- `avgLatencyMs` averages only records that carry a latency; records written before latency was tracked are excluded, and with no samples it is `0.0`.
- `dailyFallbackApplied` is `true` when a window longer than a day had to be topped up from the daily rollups because raw records had been evicted. Only requests / cost / credits are topped up — token totals cannot be, so they may read low on long windows.

### GET /api/admin/rpm

Live requests-per-minute snapshot.

**Request:**
```bash
curl http://localhost:8080/api/admin/rpm \
  -H "Authorization: Bearer sk-your-admin-key"
```

### GET /api/admin/config

Get a redacted config view (booleans / non-secret fields only). This response is **snake_case**, not camelCase.

**Request:**
```bash
curl http://localhost:8080/api/admin/config \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response:**
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
  "api_key_set": true,
  "admin_api_key_set": true
}
```

### GET /api/admin/models

The real model catalog, with `display_name` / `type` / `max_tokens`. This response is **snake_case**, not camelCase.

This one is pool-aware: it serves the live per-account capability union once accounts have been probed, and otherwise falls back to the same built-in 17-model catalog the protocol `/models` endpoints always serve — so with a warm cache the two lists can differ, and with a cold one they hold the same ids.

When the union is empty it also kicks off a background refill (single-flight, bounded, with a 60-second cooldown) so a later call can answer from the union; the current call still answers from the catalog. `rate_multiplier` only appears on union entries that carry one — the catalog fallback never emits it.

**Request:**
```bash
curl http://localhost:8080/api/admin/models \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response** (abridged):
```json
{
  "object": "list",
  "data": [
    {
      "id": "claude-sonnet-4.5",
      "object": "model",
      "created": 1700000000,
      "owned_by": "kiro2api",
      "display_name": "Claude Sonnet 4.5",
      "type": "chat",
      "max_tokens": 200000
    }
  ]
}
```

### POST /api/admin/credentials/models/refresh · /credentials/{id}/models/refresh

Refresh the model cache that `GET /api/admin/models` reads from. **Both call upstream** (`ListAvailableModels` once per account touched, refreshing that account's token first if it is about to expire), so they are the manual counterpart to the background refill `GET /api/admin/models` triggers on its own. Neither takes a body or query parameters — a posted body is simply unused.

`POST /api/admin/credentials/{id}/models/refresh` refreshes exactly one account:

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/models/refresh \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response:**
```json
{
  "success": true,
  "id": "12345",
  "count": 18
}
```

`id` is echoed as the **string** from the path and `count` is how many models that account reported. An id that is not in the pool returns `404` `{"error": "account not found", "id": "…"}` (a disabled account is still refreshable — being disabled is not a `404`). An upstream failure returns `502` `{"success": false, "id": "…", "error": "<upstream message>"}`, where `error` carries the real reason (status code plus the upstream's own text) rather than a generic string.

`POST /api/admin/credentials/models/refresh` refreshes the pool, but **not account by account**: it groups enabled accounts by subscription tier, refreshes one representative per known tier, then probes accounts of unknown tier until the union stops growing (3 consecutive no-growth successes), 12 successful probes are reached, or they run out. That is deliberate — a pool of hundreds would otherwise mean hundreds of upstream calls per click.

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/credentials/models/refresh \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response:**
```json
{
  "success": true,
  "refreshed": 2,
  "failed": 1,
  "errors": [{"id": 12346, "error": "models upstream HTTP 403: ..."}],
  "tiers": ["KIRO FREE", "KIRO PRO+"]
}
```

The batch endpoint always returns `200` with `success: true` — it reports per-account outcomes instead of failing: `refreshed` / `failed` are counts, `errors[]` holds one `{id, error}` per failed account (here `id` is a **number**, and unparseable ids become `0`), and `tiers[]` lists the tiers actually covered. A pool where every call fails still answers `200` with `refreshed: 0`.

The tier strings are not an enum of this service's making — they are the upstream subscription titles, the same value `GET /api/admin/credentials/{id}/balance` returns as `subscriptionTitle`. Grouping therefore depends on the balance cache being warm; an account with no cached title counts as unknown-tier and can only enter the bounded probe. `tiers[]` uses the literal `"unknown"` for a probed account whose title is still uncached afterwards.

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

`{masterApiKey,version,kiroVersion,rustVersion,…}` plus runtime metrics (`serverTime`, `serverTimeUnix`, `os`, `memoryUsedBytes`, `memoryTotalBytes`, `cpuPercent`, `runMode`, `pid`, `uptimeSecs`). `version` is the kiro2api version, `kiroVersion` is the spoofed upstream UA version.

`masterApiKey` is the configured `apiKey` in **full plaintext** (`null` when unset) — it is **not** masked here; the panel masks it in the browser but its copy button needs the real value. Use `GET /api/admin/config/auth-keys` when you want the masked form.

**Request:**
```bash
curl http://localhost:8080/api/admin/server-info \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response** (abridged):
```json
{
  "masterApiKey": "sk-your-master-key",
  "version": "0.7.0",
  "kiroVersion": "0.11.107",
  "rustVersion": "1.90.0",
  "runMode": "Docker",
  "uptimeSecs": 3600
}
```

### GET /api/admin/check-update

Compare the running version against the latest GitHub Release of `xwteam/kiro2api`.

Takes no parameters and **always returns `200`** — it is deliberately forgiving: a network failure, a rate-limited or `404` GitHub API, a repository with no releases, and an unparseable payload all degrade to `hasUpdate: false` with `latest` equal to `current`, never an error the panel would have to render. It is the only endpoint here that talks to GitHub (`api.github.com`), so a deployment without egress to it simply always reports no update.

**Request:**
```bash
curl http://localhost:8080/api/admin/check-update \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response:**
```json
{
  "current": "0.4.0",
  "latest": "0.4.1",
  "hasUpdate": true,
  "updateUrl": "https://github.com/xwteam/kiro2api/releases/tag/v0.4.1",
  "releaseNotes": "..."
}
```

`current` is this build's crate version. `latest` is the release `tag_name` with a leading `v` stripped, and `hasUpdate` is a plain string inequality against `current`, not a semver comparison — a differently-shaped tag reads as "update available". `updateUrl` is the release's `html_url`, falling back to `https://github.com/xwteam/kiro2api/releases`; `releaseNotes` is the release body verbatim, `""` when there is none.

### POST /api/admin/update

Return the shell command that updates this deployment. **It does not update anything** — nothing is downloaded, nothing is restarted; the panel just shows the command with a copy button. Takes no body or parameters and always returns `200`.

**Request:**
```bash
curl -X POST http://localhost:8080/api/admin/update \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response:**
```json
{
  "status": "ok",
  "message": "请在服务器上执行以下命令完成更新:",
  "command": "docker compose pull && docker compose up -d"
}
```

All three values are hard-coded constants: `status` is always `"ok"`, `message` is that fixed Chinese sentence ("run the following command on the server to finish updating"), and `command` is the Docker Compose pair regardless of how you actually deployed. On a bare-metal or Kubernetes deployment, treat it as a reminder rather than a runnable instruction.

### POST /api/admin/restart

Exit the process so that whatever supervises it starts a fresh one. **This is the only admin endpoint that touches the running process itself.**

`?confirm=true` is required. Without it (or with `confirm=false`) the request is refused with `400` and the process is untouched:

```json
{
  "error": {
    "message": "重启需二次确认,请带查询参数 ?confirm=true",
    "type": "confirmation_required"
  }
}
```

The message is that fixed Chinese sentence ("restarting needs a second confirmation, pass the query parameter `?confirm=true`"); match on `error.type` rather than on the text. With confirmation the response is returned **first**, then a background task waits 0.5 s, flushes the debounced stores (usage stats, balance cache, failure/throttle event logs, and the API-KEY store) and calls `exit(0)`. So a `200` means "the process is about to go away", not "the process has restarted" — expect in-flight requests to be cut off and the port to drop for as long as your supervisor takes to bring it back.

**Request:**
```bash
curl -X POST "http://localhost:8080/api/admin/restart?confirm=true" \
  -H "Authorization: Bearer sk-your-admin-key"
```

**Response:**
```json
{
  "status": "ok",
  "message": "Server restarting..."
}
```

Both fields are hard-coded constants. Note that "restart" is really "exit": whether the service comes back is entirely up to what supervises it. Under Docker with `restart: unless-stopped` (or systemd with a restart policy) it returns on its own; run bare with no supervisor and this endpoint simply stops the service.

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
- `GET /admin/api/config` — redacted config, the same view as `GET /api/admin/config`.
- `POST /admin/api/accounts/{id}/enable` | `disable` — manual enable/disable.

`POST /admin/api/accounts/{id}/disable` is the legacy alias of [`POST /api/admin/credentials/{id}/disabled`](#post-apiadmincredentialsiddisabled) with `{"disabled": true}` (and `/enable` the same with `false`). It runs the identical pool mutation; what differs is the calling convention:

- **No body.** The state is fixed by the path segment, so there is nothing to post and a body is ignored.
- **A different response shape** — `{ok, id, disabled}` instead of the modern `{success, message}`, with `id` echoed as the string from the path:

```bash
curl -X POST http://localhost:8080/admin/api/accounts/12345/disable \
  -H "Authorization: Bearer sk-your-admin-key"
```

```json
{
  "ok": true,
  "id": "12345",
  "disabled": true
}
```

An id that is not in the pool returns `404` `{"error": "account not found", "id": "…"}`, exactly as the modern endpoint does. Both variants only flip the flag on the in-memory pool — neither writes `credentials.json` itself, so a restart normally reverts to the file's value. Do not lean on that either way: the pool is written out whenever something else persists it (a token refresh, or any credential create / update / delete), and the flag is part of what gets written, so it can become durable as a side effect.

## User API

The user panel at `/user` (a static SPA embedded via rust-embed) is backed by `/api/user/*`. These endpoints are **not** behind the admin gate — each request authenticates with the caller's **own API-KEY**, taken from the header channels only, in this order: `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` (the query channels the protocol gate accepts do **not** work here). `POST /api/user/login` additionally accepts `{apiKey}` in the body, and a non-empty body value wins over any header. The handler validates the key and scopes all data to it. Invalid key → `401`. Responses are camelCase; `credits = cost / 0.72`.

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

`GET /api/user/usage/records` uses the same envelope as the admin record endpoints but defaults to **`page_size=50`** (the admin default is 20); `page=1` and the clamping rules are the same. Records are scoped to the calling key — there is no id in the path, so a key can only ever read its own usage. A valid key with no records answers an empty page, not a `404`; an invalid, disabled or expired key answers `401` `{"error": "…"}`.

**Request:**
```bash
curl "http://localhost:8080/api/user/usage/records?page=1&page_size=50" \
  -H "x-api-key: sk-your-api-key"
```

**Response:**
```json
{
  "records": [
    {
      "model": "claude-sonnet-4.5",
      "inputTokens": 1200,
      "outputTokens": 340,
      "estimatedCost": 0.0123,
      "creditsUsed": 0.0171,
      "createdAt": "2026-07-25T11:59:00Z",
      "clientIp": "203.0.113.7"
    }
  ],
  "total": 1,
  "page": 1,
  "pageSize": 50,
  "totalPages": 1
}
```

`model`, `inputTokens`, `outputTokens`, `estimatedCost`, `createdAt` and the envelope counters always serialize; `creditsUsed`, `cacheReadInputTokens`, `cacheCreationInputTokens` and `clientIp` are omitted when absent. Two further fields are declared on this record but never populated on the user side, so they are **always absent**: `creditsSaved` (no data source) and `credentialLabel` (the user view does not resolve account labels — unlike the admin records, which do). Note there is no `credentialId` either: which pooled account served a request is not exposed to the key's owner.

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
  "version": "0.7.0"
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
| 400 | Bad Request | On the relay endpoints, three causes: a body they cannot deserialize (they answer `400`, never `422`); a model name matching nothing in the internal map — rejected by the gateway itself, message `无法识别的模型名: <name>`; or a mapped model the account's tier cannot serve — refused upstream (reason `INVALID_MODEL_ID`) and reported as `Invalid model '<name>': not available for the current account. …`. Two admin endpoints add their own: `POST /api/admin/restart` without `?confirm=true`, and `GET /api/admin/usage/summary` with an unrecognized `range` or `hours=0` |
| 401 | Unauthorized | Missing or invalid API Key (when `apiKey` is configured); also a disabled or expired store key |
| 402 | Payment Required | A store-managed key has reached its spending limit (`{"type":"error","error":{"type":"billing_error",…}}`) |
| 404 | Not Found | Admin endpoints only: unknown account / API-KEY / login-session id |
| 422 | Unprocessable Entity | Admin endpoints and `/api/user/login` only: the body does not deserialize into the expected shape (missing or wrongly typed required field). The body is axum's `text/plain` diagnostic, not any of the three JSON error shapes above. The relay endpoints (the four conversational ones plus `/v1/messages/count_tokens`) never answer `422` — they convert the same failure into a `400` carrying their protocol's error shape |
| 502 | Bad Gateway | Upstream Kiro / CodeWhisperer failure |
| 503 | Service Unavailable | No account available (all in cooldown / disabled / over RPM); also the log endpoints when `logCapacity` is `0` |

### Common Error Causes

| Cause | Meaning | Solution |
|-------|---------|----------|
| Invalid API Key | Key missing or wrong | Verify the value on whichever of the six channels you use (`Authorization: Bearer` / `x-api-key` / `x-goog-api-key` / `?api_key=` / `?token=` / `?key=`); remember the header channels outrank the query ones, so a stale header masks a correct query parameter |
| Spending limit reached (`402`) | A store-managed key is at or over its configured limit | Raise or clear the key's limit in the admin panel, or issue a new key |
| `INVALID_MODEL_ID` | Model not served by your pool | `/v1/models` lists what the gateway accepts, not what your tier authorizes, so it cannot tell you this — check `GET /api/admin/models` and your accounts' subscription tier; the account is **not** penalized. The code is the *upstream* reason string and is not echoed in the response body: match on the `400` plus its message, never on the literal `INVALID_MODEL_ID` |
| No account available | All accounts in cooldown / disabled / over RPM | Add accounts, wait for cooldown, or reset failure counters |
| Upstream failure | Kiro / CodeWhisperer / AmazonQ error | Endpoint fallback and cross-account retry are automatic; check the admin logs |

## Rate Limiting

Per-credential RPM limiting is configurable via `MAX_RPM_PER_CREDENTIAL` (`0` = unlimited). Each account also has its own graded cooldown after consecutive failures, classified by category (permanent invalidation / ambiguous auth / quota / transient) so only true credential failures disable an account.

```env
MAX_RPM_PER_CREDENTIAL=60
```

When an account is over its RPM or in cooldown, the pool rotates to the next available account; if none is available the request returns `503`.

> [!IMPORTANT]
> **Exceeding the local RPM cap never produces a `429`.** It only makes that account ineligible for selection, and the request is served by another account. Only when *every* account is excluded does the caller see anything, and that is a `503` (`overloaded_error`, `no available upstream account`). A `429` reaching your client always originates from an **upstream** throttle. Do not treat `429` as the signal for your own `MAX_RPM_PER_CREDENTIAL`, and do not read the resulting `503` as a total pool outage.

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
