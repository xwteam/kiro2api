# API 레퍼런스

kiro2api의 모든 API 엔드포인트 상세 정보입니다.

## 인증

모든 프로토콜 요청에 인증이 필요합니다(`apiKey`/`API_KEY`가 설정된 경우). 게이트는 아래 채널을 **`Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > 쿼리(`api_key` > `token` > `key`)** 우선순위로 모두 받아들이며, 값은 상수 시간 비교로 검증됩니다:

### Bearer Token (권장)

```bash
curl -H "Authorization: Bearer sk-당신의키"
```

### API Key 헤더

```bash
curl -H "x-api-key: sk-당신의키"
```

### Gemini 네이티브 헤더

```bash
curl -H "x-goog-api-key: sk-당신의키"
```

### 쿼리 파라미터

`api_key`(브라우저 SSE용) / `token`(구 계약) / `key`(Gemini 생태 표준) 중 아무거나 쓸 수 있습니다:

```bash
curl "http://localhost:8080/v1/models?token=sk-당신의키"
curl "http://localhost:8080/v1beta/models?key=sk-당신의키"
```

> API Key는 `.env` 파일의 `API_KEY` 값, `data/config.json`의 `apiKey`, 또는 서비스 시작 로그에서 확인 가능합니다. `/health`와 `/v1/ping` 탐활 엔드포인트는 인증이 필요 없습니다.

## 이중 프리픽스 경로

각 프로토콜은 두 가지 경로 형식을 동시에 제공합니다:

### 표준 베어 경로 (권장)

주요 SDK가 `base_url`에 접미사 없이 즉시 작동하도록 표준 경로를 노출합니다:

**OpenAI 형식**:
- `/v1/chat/completions`
- `/v1/models`

**Anthropic 형식**:
- `/v1/messages`
- `/v1/messages/count_tokens`

**Gemini 형식**:
- `/v1beta/models/{model}:generateContent`
- `/v1beta/models/{model}:streamGenerateContent`
- `/v1beta/models`

### 접두사 경로 (제공자별 명시)

네 제공자를 명확히 구분해야 할 때 사용합니다:

- OpenAI: `/openai/v1/chat/completions`, `/openai/v1/models`, `/openai/v1/responses`
- Claude: `/claude/v1/messages`, `/claude/v1/messages/count_tokens`, `/claude/v1/models`
- Gemini: `/gemini/v1beta/models/{model}:generateContent`, `:streamGenerateContent`

> [!IMPORTANT]
> 베어 `/v1/models`는 OpenAI 형식을 반환합니다(하나의 경로로 두 형식을 반환할 수 없음). Anthropic 형식 모델 목록이 필요하면 `/claude/v1/models`를 사용하세요.

## OpenAI 호환 API

### POST /openai/v1/chat/completions

OpenAI 형식의 대화 완성 API입니다.

**요청**:

```bash
curl -X POST http://localhost:8080/openai/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의키" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {"role": "user", "content": "안녕하세요"}
    ],
    "stream": false
  }'
```

**요청 파라미터**:

| 파라미터 | 타입 | 필수 | 설명 |
|---------|------|------|------|
| `model` | string | ✅ | 모델 ID (예: claude-sonnet-4.5). 소문자 부분 문자열 매칭 |
| `messages` | array | ✅ | 메시지 배열. `content`는 문자열 또는 객체 배열 (멀티모달 지원) |
| `stream` | boolean | ❌ | 스트리밍 응답 (기본값: false) |
| `tools` | array | ❌ | 함수 호출 도구 정의 (네이티브 `tool_calls` 진짜 전달) |
| `tool_choice` | string 또는 object | ❌ | **받기만 하고 무시됩니다** — 아래 「생성 파라미터에 대한 주의」 참고 |
| `max_tokens` | integer | ❌ | **받기만 하고 무시됩니다** — 응답 길이를 제한하지 않습니다. 아래 참고 |
| `temperature` | number | ❌ | **파싱조차 되지 않고 버려집니다** — 아래 참고 |

> [!IMPORTANT]
> **생성 파라미터에 대한 주의(4개 프로토콜 공통)**
>
> Kiro 업스트림 데이터 평면 요청 포맷에는 샘플링/길이/도구 강제에 해당하는 필드가 **존재하지 않습니다**. 따라서 아래 파라미터는 보내도 오류가 나지 않지만 **아무 효과도 없습니다**:
>
> - `temperature` / `top_p` — 요청 구조체에 필드 자체가 없어 역직렬화 단계에서 조용히 버려집니다. `temperature: 0`으로 결정론적 응답을 기대해도 그렇게 되지 않으며, 경고도 나오지 않습니다.
> - `max_tokens` / `maxOutputTokens` / `max_output_tokens` — 내부 중추 구조체까지는 들어오지만 업스트림 요청 본문에는 **실리지 않습니다**. 응답 길이를 제한하지 못합니다. 실제로 응답이 잘리는 것은 업스트림 자신의 예산에 걸렸을 때뿐이며, 그때는 프로토콜별 잘림 신호(OpenAI `finish_reason:"length"`, Anthropic `stop_reason:"max_tokens"`, Gemini `finishReason:"MAX_TOKENS"`, Responses `status:"incomplete"` + `incomplete_details.reason:"max_output_tokens"`)로 정직하게 보고됩니다.
> - `tool_choice` — 중추 구조체까지 실려 오지만 그 뒤로 아무도 읽지 않습니다. 특정 도구를 강제하거나 `required`로 만들 수 없습니다. 도구를 **끄는 것**만은 Gemini에서 가능합니다: `toolConfig.functionCallingConfig.mode`가 `"NONE"`이면 도구 명세 자체를 업스트림에 내려보내지 않습니다(`AUTO`는 기본 동작과 같고, `ANY`는 표현할 방법이 없어 기본 동작으로 처리됩니다). 다른 프로토콜에서 도구를 끄려면 `tools`를 아예 보내지 마십시오.

**멀티모달 content 형식**:

`content`는 문자열 (텍스트만) 또는 객체 배열 (텍스트와 이미지 지원):

```json
{
  "role": "user",
  "content": [
    {"type": "text", "text": "이것은 무엇입니까"},
    {
      "type": "image_url",
      "image_url": {
        "url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
      }
    }
  ]
}
```

지원되는 content 타입:
- `text`: 순수 텍스트 콘텐츠
- `image_url`: 이미지. **Base64 Data URI(`data:image/...;base64,...`)만 지원합니다.** 업스트림 Kiro는 인라인 base64만 받으므로 `https://example.com/x.jpg` 같은 원격 URL을 넣으면 조용히 무시되는 것이 아니라 요청 전체가 `400 invalid_request_error`로 거부됩니다(이미지가 빠진 채 답변이 나가는 것을 막기 위한 의도적 동작). 원격 이미지는 클라이언트에서 먼저 내려받아 Data URI로 인라인하십시오.

**메시지 형식**:

```json
{
  "role": "user|assistant|system|tool",
  "content": "텍스트 또는 배열"
}
```

**응답 (비스트리밍)**:

```json
{
  "id": "chatcmpl-xxx",
  "object": "chat.completion",
  "created": 1234567890,
  "model": "claude-sonnet-4.5",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "응답 텍스트"
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 10,
    "completion_tokens": 50,
    "total_tokens": 60
  }
}
```

도구를 사용하면 `message`에 `tool_calls`가 들어가고 `finish_reason`은 `tool_calls`가 됩니다.

**응답 (스트리밍)**:

```
data: {"choices":[{"delta":{"role":"assistant"}}]}
data: {"choices":[{"delta":{"content":"응답"}}]}
data: {"choices":[{"delta":{"content":" 텍스트"}}]}
data: [DONE]
```

첫 프레임에 `delta.role`, 마지막 프레임에 `finish_reason`이 실리며 `chat.completion.chunk` 객체 타입으로 반환됩니다.

**에러 응답**:

```json
{
  "error": {
    "message": "오류 설명",
    "type": "invalid_request_error",
    "code": null
  }
}
```

`code`는 중계 자신이 만든 오류(모델명 매핑 실패, 본문 파싱 실패, 사용 가능한 계정 없음 등)에서는 **항상 `null`**이고, 업스트림 예외를 변환한 오류에서만 HTTP 상태 코드가 **숫자로** 들어갑니다. 문자열 `INVALID_MODEL_ID`가 `code`에 실리는 일은 없습니다(아래 「에러 코드」의 `400` 항목 참고).

### GET /openai/v1/models

프로토콜 고정 모델 목록 조회 (컴파일 시점 고정 목록이며 계정 풀·구독 등급으로 필터링되지 않습니다 — 아래 ⚠️ 참고)

**요청**:

```bash
curl http://localhost:8080/openai/v1/models \
  -H "Authorization: Bearer sk-당신의키"
```

**응답**:

```json
{
  "object": "list",
  "data": [
    {
      "id": "claude-sonnet-4.5",
      "object": "model",
      "created": 1700000000,
      "owned_by": "kiro2api"
    }
  ]
}
```

> 💡 **모델 선택 가이드**: 실제로 사용 가능한 모델은 **Kiro 계정의 구독 등급**에 따라 달라집니다.
> - 무료 등급(KIRO FREE)은 보통 `claude-sonnet-4.5`만 인가됩니다.
> - opus / GPT 계열 등은 더 높은 등급이 필요합니다.
> - 인가되지 않은 모델을 요청하면 정적 실패가 아니라 명확히 `400`(`INVALID_MODEL_ID`)을 반환합니다 — 헛되이 재시도하지 않고 계정을 손상시키지도 않습니다.
>
> ⚠️ 프로토콜의 `/models`는 **컴파일 시점에 고정된 짧은 목록**을 반환합니다. 계정 풀이나 구독 등급으로 필터링되지 않으며, 프로토콜마다 항목도 서로 다릅니다 — OpenAI: `claude-sonnet-4.5` / `claude-opus-4.6` / `gpt-5.6-sol`, Anthropic(`/claude/v1/models`): `claude-sonnet-4.5` / `claude-opus-4.6` / `claude-haiku-4.5`, Gemini(`/v1beta/models`): `claude-sonnet-4.5` / `claude-opus-4.6` / `gpt-5.6-sol`. 따라서 목록에 있는 모델이라도 등급이 인가하지 않으면 `400`(`INVALID_MODEL_ID`)이 날 수 있고, 반대로 목록에 없는 id도 이름 매핑만 되면 정상 동작합니다. 이름 매핑이 해석해 내는 내부 모델 id는 정적 카탈로그 17종에 `auto`를 더한 18종이며, 관리 API `GET /api/admin/models`는 계정들의 업스트림 합집합(캐시)을 우선 반환하고 합집합이 비어 있을 때만 그 정적 17종으로 대체합니다.


### POST /openai/v1/responses

OpenAI Responses API입니다. Chat Completions 대신 최신 Responses 프로토콜을 요구하는 클라이언트(예: 2026년 2월부로 Chat Completions 지원을 중단한 **Codex CLI** — Codex CLI를 kiro2api에 연결하려면 이 엔드포인트가 필요합니다)를 위해 추가되었습니다. 텍스트, 스트리밍, 함수/도구 호출을 지원합니다.

**요청**:

```bash
curl -X POST http://localhost:8080/openai/v1/responses \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "input": "2+2는 얼마인가요?",
    "stream": false
  }'
```

**요청 본문**:

| 파라미터 | 타입 | 필수 | 설명 |
|---------|------|------|------|
| `model` | string | ✅ | 모델 ID (예: `claude-sonnet-4.5`) |
| `input` | string 또는 array | ✅ | 단일 문자열(사용자 메시지 하나를 축약한 형태), 또는 입력 항목 배열(아래 참고) |
| `instructions` | string | ❌ | 대화 앞에 붙는 시스템/개발자 사전 지시문(→ system) |
| `stream` | boolean | ❌ | 스트리밍 활성화 여부 (기본값: false) |
| `tools` | array | ❌ | 도구 호출용 함수 정의, **평면(flat) 구조**: `{"type":"function","name","description","parameters"}` (참고: Chat Completions의 중첩 구조 `{"type":"function","function":{...}}`와 다름) |
| `max_output_tokens` | integer | ❌ | **받기만 하고 무시됩니다** — 응답 길이를 제한하지 않습니다(위 「생성 파라미터에 대한 주의」 참고) |
| `tool_choice` | string 또는 object | ❌ | **받기만 하고 무시됩니다** — `required`나 `{"type":"function","name":"..."}`로 특정 도구를 강제할 수 없습니다(위 「생성 파라미터에 대한 주의」 참고) |

**`input` 배열 항목 유형**: 아래 **세 가지뿐**입니다. `type`을 생략하면 `role` 유무로 message 항목으로 판정하며, 그 밖의 `type` 문자열(예: `tool_result`)을 하나라도 넣으면 그 항목만 무시되는 것이 아니라 **요청 전체가 `400`으로 거부**됩니다.

- `{"type":"message","role":"user"|"assistant"|"system","content":[...]}` — 콘텐츠 파트는 `{"type":"input_text","text":...}`, `{"type":"output_text","text":...}`, `{"type":"input_image","image_url":"..."}` 세 가지. `content`에 문자열을 바로 넣어도 됩니다.
  - ⚠️ `input_image`의 `image_url`은 **`data:<mime>;base64,<데이터>` 형식만** 처리됩니다. 원격 `http(s)` URL을 넣으면 오류가 나지 않고 그 이미지 블록이 **조용히 버려져** 모델이 이미지를 보지 못합니다. 반드시 Base64 Data URI로 인라인하십시오.
- `{"type":"function_call","call_id","name","arguments"}` — 이전 어시스턴트의 도구 호출 턴 (멀티턴 히스토리는 직접 다시 전송). `arguments`는 **JSON 문자열**입니다.
- `{"type":"function_call_output","call_id","output"}` — 다시 전달하는 도구 실행 결과. Anthropic의 `tool_result`에 해당하지만, **여기서 `"tool_result"`라는 이름은 인식되지 않습니다**.

**지원하지 않음(조용히 무시하지 않고 명시적으로 오류 반환)**: `previous_response_id` — 이 서버는 서버 측 대화 상태를 유지하지 않습니다. 이 필드를 보내면 조용히 무시하는 대신 400 `invalid_request_error`를 반환합니다. 매 요청마다 전체 대화 내용을 `input`에 담아 다시 보내세요 (Codex CLI는 이미 이렇게 동작합니다).

**응답 (비스트리밍)**:

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
> 위가 응답 객체의 **전부**입니다. 공식 OpenAI 응답에는 있지만 이 서버가 **절대 내보내지 않는** 필드가 있으니 클라이언트에서 읽지 마십시오(`KeyError`가 납니다): `output_text` 파트의 `annotations`, `usage`의 `input_tokens_details` / `output_tokens_details`, 그리고 최상위의 `previous_response_id` / `instructions` / `error`. 실패는 별도 필드가 아니라 HTTP 상태 코드와 오류 본문으로 전달됩니다.
>
> 선택 필드는 `incomplete_details` 하나뿐이며, 업스트림이 응답을 잘랐을 때만 나타납니다 — 그때 `status`는 `"completed"`가 아니라 `"incomplete"`가 되고 `"incomplete_details": {"reason": "max_output_tokens"}`가 함께 실립니다.

**응답 (스트리밍)**: 각 이벤트마다 단조 증가하는 `sequence_number`를 포함하는, 공식 프로토콜 순서를 그대로 따르는 명명된 SSE 이벤트 시퀀스입니다. `data: [DONE]` 같은 종료 표시는 **없으며** (이는 Chat Completions 방식의 관례입니다) — 완료는 `response.completed`(실패 시 `response.failed`)로 알립니다:

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

도구 호출의 경우, `response.output_item.added`(타입 `function_call`) 다음에는 위의 텍스트 이벤트 대신 `response.function_call_arguments.delta` / `response.function_call_arguments.done` / `response.output_item.done`이 이어집니다.

**함수 호출 예시**:

```bash
curl -X POST http://localhost:8080/openai/v1/responses \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "input": "파리 날씨가 어때요?",
    "tools": [
      {
        "type": "function",
        "name": "get_weather",
        "description": "도시의 날씨를 조회합니다",
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
응답 `output`에는 `function_call` 항목이 포함됩니다:
```json
{"id": "fc_xxx", "type": "function_call", "status": "completed", "call_id": "call_xxx", "name": "get_weather", "arguments": "{\"city\": \"파리\"}"}
```

## Claude 호환 API

### POST /claude/v1/messages

Claude(Anthropic) 형식의 메시지 생성 API입니다. 내부적으로 **Anthropic Messages를 중추 모(母) 포맷**으로 사용하므로, 이 프로토콜이 중전(中転) 커널에 가장 직접적으로 매핑됩니다.

**요청**:

```bash
curl -X POST http://localhost:8080/claude/v1/messages \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의키" \
  -d '{
    "model": "claude-sonnet-4.5",
    "max_tokens": 1024,
    "messages": [
      {"role": "user", "content": "안녕하세요"}
    ],
    "stream": false
  }'
```

**요청 파라미터**:

| 파라미터 | 타입 | 필수 | 설명 |
|---------|------|------|------|
| `model` | string | ✅ | 모델 ID |
| `messages` | array | ✅ | 메시지 배열. `content`는 문자열 또는 블록 배열(`text`/`image`/`tool_use`/`tool_result`) |
| `system` | string 또는 array | ❌ | 시스템 프롬프트. 문자열과 콘텐츠 블록 배열(`[{"type":"text","text":"…"}]`) 양쪽 모두 받습니다 |
| `tools` | array | ❌ | 도구 정의 (`tool_use` 진짜 전달) |
| `stream` | boolean | ❌ | 스트리밍 응답 |
| `max_tokens` | integer | ❌ | 이 서버에서는 **필수가 아니며(생략 가능), 보내도 무시됩니다** — 응답 길이를 제한하지 않습니다. 다만 공식 Anthropic SDK는 클라이언트 쪽에서 이 필드를 요구하므로 SDK를 쓴다면 그대로 채워 보내면 됩니다(위 「생성 파라미터에 대한 주의」 참고) |
| `tool_choice` | object | ❌ | **받기만 하고 무시됩니다** — 특정 도구를 강제할 수 없습니다 |
| `temperature` | number | ❌ | **파싱조차 되지 않고 버려집니다** — 효과 없음 |

**응답**:

```json
{
  "id": "msg-xxx",
  "type": "message",
  "role": "assistant",
  "content": [
    {
      "type": "text",
      "text": "응답 텍스트"
    }
  ],
  "model": "claude-sonnet-4.5",
  "stop_reason": "end_turn",
  "usage": {
    "input_tokens": 10,
    "output_tokens": 50
  }
}
```

스트리밍은 Anthropic 표준 SSE입니다(`message_start` → `content_block_start` → `content_block_delta` → … → `message_stop`). 도구는 `tool_use` 블록과 `input_json_delta`를 사용합니다.

### GET /claude/v1/models

Claude(Anthropic) 형식 모델 목록. 베어 `/v1/models`가 OpenAI 형식을 반환하므로, Anthropic 형태 목록이 필요하면 이 경로를 사용합니다.

**요청**:

```bash
curl http://localhost:8080/claude/v1/models \
  -H "Authorization: Bearer sk-당신의키"
```

### POST /claude/v1/messages/count_tokens

Token 개수 추정(대략치)

**요청**:

```bash
curl -X POST http://localhost:8080/claude/v1/messages/count_tokens \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의키" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {"role": "user", "content": "안녕하세요"}
    ]
  }'
```

**응답**:

```json
{
  "input_tokens": 10
}
```

## Gemini 원생 API

### GET /gemini/v1beta/models

Gemini 모델 목록

**요청**:

```bash
curl http://localhost:8080/gemini/v1beta/models \
  -H "Authorization: Bearer sk-당신의키"
```

### POST /gemini/v1beta/models/{model}:generateContent

Gemini 형식의 콘텐츠 생성 (**전체 camelCase**)

**요청**:

```bash
curl -X POST http://localhost:8080/gemini/v1beta/models/claude-sonnet-4.5:generateContent \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의키" \
  -d '{
    "contents": [
      {
        "role": "user",
        "parts": [
          {"text": "안녕하세요"}
        ]
      }
    ]
  }'
```

`contents[]`(`parts[]`의 `text`/`inline_data`), `system_instruction?`, `tools[].function_declarations`, `toolConfig?`를 지원합니다. Gemini 네이티브 포맷 `{candidates[].content.parts, finishReason, usageMetadata}`을 반환하며, 도구 사용 시 `functionCall`이 실립니다.

> [!IMPORTANT]
> **`generationConfig`는 실질적으로 무시됩니다.** 이 구조체에서 파싱하는 필드는 `maxOutputTokens`(스네이크 표기 `max_output_tokens`도 인식) 하나뿐이고, 그마저도 업스트림 요청에 실리지 않아 응답 길이를 제한하지 못합니다. `temperature`, `topP`, `topK`, `candidateCount`, `stopSequences` 등 나머지는 필드 자체가 없어 조용히 버려집니다 — 오류는 나지 않지만 효과도 없습니다(위 「생성 파라미터에 대한 주의」 참고).
>
> `toolConfig`에서 실제로 효력이 있는 것은 `functionCallingConfig.mode: "NONE"` 하나입니다 — 이 경우 도구 명세를 업스트림에 내려보내지 않아 함수 호출이 확실히 억제됩니다. `"AUTO"`는 기본 동작과 같고, `"ANY"`(최소 1회 호출 강제)는 업스트림에 표현할 수단이 없어 기본 동작으로 처리됩니다.

### POST /gemini/v1beta/models/{model}:streamGenerateContent

스트리밍 콘텐츠 생성 (`?alt=sse` 형식, camelCase, `[DONE]` 없음)

**요청**:

```bash
curl -X POST "http://localhost:8080/gemini/v1beta/models/claude-sonnet-4.5:streamGenerateContent?alt=sse" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의키" \
  -d '{
    "contents": [
      {
        "role": "user",
        "parts": [{"text": "안녕하세요"}]
      }
    ]
  }'
```

> [!IMPORTANT]
> 인증 게이트는 다음 채널을 이 우선순위로 모두 받아들입니다: `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > 쿼리(`?api_key=` > `?token=` > `?key=`). Gemini 네이티브의 `x-goog-api-key` 헤더와 `?key=` 파라미터도 지원하므로 공식 `google-genai` SDK는 `base_url`만 바꾸면 그대로 동작합니다. 바뀌어야 하는 것은 **값**입니다 — 언제나 **본 서비스의** API Key를 넘기고, 실제 Google/OpenAI 벤더 키를 넘기지 마십시오.

## 관리 API

`/admin` 관리 패널은 `/api/admin/*` API로 구동됩니다. 아래 엔드포인트는 모두 `adminApiKey`(미설정 시 `apiKey`로 대체. 둘 다 비어 있으면 관리 API가 개방되므로 그 상태로 외부에 노출하지 마십시오)로 인증됩니다. 응답 본문은 **`GET /api/admin/config`와 `GET /api/admin/models`를 제외하면** camelCase입니다(이 둘은 패널의 데이터 모델에 맞춰 snake_case). 레거시 엔드포인트도 예외로, `GET /admin/api/config`는 `GET /api/admin/config`와 동일한 snake_case 뷰를 돌려주고 `GET /admin/api/stats`는 `summary`뿐 아니라 **응답 전체가** snake_case입니다(`accounts[]`의 각 항목도 `last_used_unix` / `in_cooldown` / `auth_method` / `expires_at_unix` / `has_profile_arn` 형태). 어느 쪽이든 **계정의 access/refresh 토큰은 절대 포함하지 않습니다**(`GET /api/admin/credentials`는 상태만 반환).

> [!WARNING]
> 관리 API 응답이 비밀 정보를 담지 않는 것은 **아닙니다**. `GET`/`POST /api/admin/api-keys`의 `key` 필드는 **완전한 평문**이고, `GET /api/admin/server-info`의 `masterApiKey`도 **완전한 평문**입니다. 마스킹되는 것은 `GET /api/admin/config/auth-keys`와 `GET /api/admin/config`뿐입니다. 읽기 전용 관리자 역할이 없으므로 관리 키를 가진 주체는 모든 key를 조회·생성·교체할 수 있습니다. 관리 API 응답은 비밀 정보로 취급하고 이슈·로그·서드파티 도구에 붙여넣지 마십시오.

### GET /api/admin/credentials

계정 풀 상태 개요 (암묵적 "로그인 확인" 면. 200이면 key 유효로 간주)

**요청**:

```bash
curl http://localhost:8080/api/admin/credentials \
  -H "Authorization: Bearer sk-당신의키"
```

**응답**:

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
      "successCount": 150,
      "healthStatus": "healthy",
      "throttleCount": 0
    }
  ]
}
```

> [!NOTE]
> - 풀은 요청마다 계정을 고르므로 "현재 계정"이라는 지속 상태가 존재하지 않습니다. `currentId`는 **항상 `-1`**, `isCurrent`는 **항상 `false`**입니다(장래의 고정 선택 모드를 위한 예약 필드) — 이 두 값으로 분기하지 마십시오.
> - `priority`는 별도 값이 아니라 풀 가중치(`weight`)를 그대로 반영하므로 언제나 `weight`와 같습니다.
> - `healthStatus`가 가질 수 있는 값은 `disabled` | `unhealthy`(쿨다운 중) | `warning`(실패 누적) | `healthy` 넷뿐입니다.

### POST /api/admin/credentials

자격 증명 1건을 풀에 추가하고 영속화

필수 필드는 `refreshToken` 하나뿐입니다(`authMethod`가 `idc`면 `clientId` + `clientSecret`도 필요). access token과 만료 시각은 **여기서 받지 않습니다** — 첫 자동 갱신 때 채워지며, 그 밖의 알 수 없는 키는 거부되지 않고 조용히 무시됩니다.

**요청**:

```bash
curl -X POST http://localhost:8080/api/admin/credentials \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의키" \
  -d '{
    "refreshToken": "...",
    "authMethod": "social",
    "profileArn": "arn:aws:codewhisperer:us-east-1:...:profile/..."
  }'
```

### PUT /api/admin/credentials/{id}

기존 자격 증명 업데이트

**요청**:

```bash
curl -X PUT http://localhost:8080/api/admin/credentials/12345 \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의키" \
  -d '{"weight": 2}'
```

### DELETE /api/admin/credentials/{id}

풀에서 자격 증명 제거

**요청**:

```bash
curl -X DELETE http://localhost:8080/api/admin/credentials/12345 \
  -H "Authorization: Bearer sk-당신의키"
```

### POST /api/admin/credentials/{id}/disabled

계정 수동 활성화/비활성화

**요청**:

```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/disabled \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의키" \
  -d '{"disabled": true}'
```

**응답**:

```json
{"success": true, "message": "..."}
```

### POST /api/admin/credentials/{id}/reset

실패 카운트 / 쿨다운 초기화

**요청**:

```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/reset \
  -H "Authorization: Bearer sk-당신의키"
```

### POST /api/admin/credentials/batch-import

일괄 가져오기. 본문은 반드시 `{"data": <페이로드>}` 형태여야 하며(`data`가 없으면 `422`), `<페이로드>`로는 배열 / KAM `{accounts:[...]}` 객체 / 단일 객체를 모두 받습니다. 행별로 정규화·검증·영속화하며 한 건이 실패해도 나머지는 계속 진행합니다.

행에서 읽는 키는 `refreshToken`(필수) · `clientId` · `clientSecret` · `region`/`authRegion`/`apiRegion` · `email` · `nickname` · `machineId` · `priority`뿐입니다(KAM의 `credentials{...}` 중첩도 인식). `authMethod`조차 본문에서 읽지 않고 `clientId`+`clientSecret`이 함께 있으면 `idc`, 아니면 `social`로 추론하며, `accessToken`/`expiresAt` 같은 나머지 키는 조용히 무시됩니다.

**요청**:

```bash
curl -X POST http://localhost:8080/api/admin/credentials/batch-import \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의키" \
  -d '{"data": [{"refreshToken":"...","email":"a@example.com"}]}'
```

### 대화형 로그인 (credentials.json 직접 편집 없이 새 Kiro 계정 추가)

Builder ID 디바이스 코드, IAM SSO 인가 코드, 소셜/SSO 토큰 3종 로그인 플로우를 제공합니다.

**요청 (Builder ID 디바이스 코드)**:

```bash
# 1) 시작 → 디바이스 코드 반환
curl -X POST http://localhost:8080/api/admin/login/builderid/start \
  -H "Authorization: Bearer sk-당신의키"

# 2) 폴링 → 완료 시 저장
curl -X POST http://localhost:8080/api/admin/login/builderid/poll \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의키" \
  -d '{"...": "..."}'
```

**응답 (poll)**:

```json
{
  "success": true,
  "completed": true,
  "status": "...",
  "credentialId": 12345,
  "email": "..."
}
```

**IAM SSO / SSO 토큰**:

- `POST /api/admin/login/iam-sso/start` → `POST /api/admin/login/iam-sso/complete` — start는 `{sessionId,authorizeUrl}`을 반환하고, complete는 콜백 URL을 소비(`state` 검증)한 뒤 저장합니다.
- `POST /api/admin/login/sso-token` — 원시 bearer/SSO 토큰 일괄 가져오기(한 줄에 하나). `{added,failed:[{lineIndex,error}]}` 반환.

### GET /api/admin/api-keys

발급된 대외 API Key 목록. 응답의 `key` 필드는 **완전한 평문**입니다(패널이 브라우저에서 마스킹해 표시하지만 "복사" 버튼에는 실제 값이 필요).

**요청**:

```bash
curl http://localhost:8080/api/admin/api-keys \
  -H "Authorization: Bearer sk-당신의키"
```

### POST /api/admin/api-keys

API Key 발급. 응답에는 새 key가 **완전한 평문**으로 담깁니다(한 번에 복사해 전달하는 용도).

**요청**:

```bash
curl -X POST http://localhost:8080/api/admin/api-keys \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의키" \
  -d '{"name": "내 Key"}'
```

### PUT /api/admin/api-keys/{id}

API Key 업데이트 (레이블/한도 등)

`{id}`는 `GET /api/admin/api-keys`가 돌려주는 **숫자** id입니다 — key 문자열이나 `key-1` 같은 값을 넣으면 `400 Invalid URL`이 납니다. 본문에 넣을 수 있는 필드는 `name`, `enabled`, `expiresAt`, `spendingLimit`, `limitUnit`, `durationDays`, `boundCredentialIds`이며, 생략한 필드는 그대로 유지되고 모르는 키는 거부되지 않고 무시됩니다(활성/비활성은 자격 증명 쪽의 `disabled`가 아니라 `enabled`).

**요청**:

```bash
curl -X PUT http://localhost:8080/api/admin/api-keys/7 \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의키" \
  -d '{"name": "새 레이블"}'
```

### DELETE /api/admin/api-keys/{id}

API Key 삭제

**요청**:

```bash
curl -X DELETE http://localhost:8080/api/admin/api-keys/7 \
  -H "Authorization: Bearer sk-당신의키"
```

### GET /api/admin/api-keys/usage

전체 API Key 사용량 (단일 key는 `.../{id}/usage`, 초기화는 `DELETE .../usage`, 페이지 기록은 `.../{id}/usage/records?page=&page_size=`)

**요청**:

```bash
curl http://localhost:8080/api/admin/api-keys/usage \
  -H "Authorization: Bearer sk-당신의키"
```

### GET /api/admin/usage/daily

일별 사용량 요약 (특정 날짜 기록은 `.../daily/{date}/records`, 계정별 기록은 `/api/admin/credentials/{id}/usage/records`·`.../usage/today`)

**요청**:

```bash
curl http://localhost:8080/api/admin/usage/daily \
  -H "Authorization: Bearer sk-당신의키"
```

### GET /api/admin/credentials/{id}/balance

계정 잔액 (5분 캐시)

**요청**:

```bash
curl http://localhost:8080/api/admin/credentials/12345/balance \
  -H "Authorization: Bearer sk-당신의키"
```

### GET /api/admin/credentials/{id}/failure-logs

최근 실패 이벤트 (스로틀 이벤트는 `.../throttle-logs`)

**요청**:

```bash
curl http://localhost:8080/api/admin/credentials/12345/failure-logs \
  -H "Authorization: Bearer sk-당신의키"
```

### GET /api/admin/rpm

실시간 RPM 스냅샷

**요청**:

```bash
curl http://localhost:8080/api/admin/rpm \
  -H "Authorization: Bearer sk-당신의키"
```

### GET /api/admin/config

마스킹된 설정 뷰 (불리언 / 비민감 필드만). 이 엔드포인트의 응답 필드명은 패널의 데이터 모델에 맞춰 **snake_case**입니다.

**요청**:

```bash
curl http://localhost:8080/api/admin/config \
  -H "Authorization: Bearer sk-당신의키"
```

**응답**:

```json
{
  "host": "0.0.0.0",
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

### GET /api/admin/config/load-balancing

부하 분산 모드 런타임 조회 (전환은 `PUT`, `priority` / `balanced`, `config.json`에 영속화)

**요청**:

```bash
curl http://localhost:8080/api/admin/config/load-balancing \
  -H "Authorization: Bearer sk-당신의키"

# 전환
curl -X PUT http://localhost:8080/api/admin/config/load-balancing \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의키" \
  -d '{"mode": "balanced"}'
```

### GET /api/admin/config/auth-keys

`apiKey` / `adminApiKey` 런타임 조회(마스킹) (교체는 `PUT`, 즉시 적용·재시작 불필요)

**요청**:

```bash
curl http://localhost:8080/api/admin/config/auth-keys \
  -H "Authorization: Bearer sk-당신의키"
```

### GET /api/admin/server-info

`{masterApiKey,version,kiroVersion,rustVersion,…}` 및 런타임 지표(`serverTime`, `serverTimeUnix`, `os`, `memoryUsedBytes`, `memoryTotalBytes`, `cpuPercent`, `runMode`, `pid`, `uptimeSecs`) 반환. `version`은 kiro2api 버전, `kiroVersion`은 위장 상류 UA 버전.

`masterApiKey`는 설정된 `apiKey`의 **완전한 평문**(미설정 시 `null`)이며 여기서는 **마스킹되지 않습니다** — 패널이 브라우저에서 마스킹해 보여주고 "복사" 버튼은 실제 값을 사용합니다. 마스킹된 값이 필요하면 `GET /api/admin/config/auth-keys`를 사용하십시오.

**요청**:

```bash
curl http://localhost:8080/api/admin/server-info \
  -H "Authorization: Bearer sk-당신의키"
```

### GET /api/admin/models

`display_name` / `type` / `max_tokens`를 포함한 모델 목록. 계정들의 업스트림 `ListAvailableModels` 합집합(캐시)을 우선 반환하고, 합집합이 비어 있으면 정적 17종 목록으로 대체합니다 — 즉 프로토콜의 `/v1/models`가 반환하는 고정 3종보다 **넓은 집합**입니다. 응답 필드명은 패널에 맞춰 **snake_case**입니다(`display_name`, `owned_by`, `max_tokens`).

**요청**:

```bash
curl http://localhost:8080/api/admin/models \
  -H "Authorization: Bearer sk-당신의키"
```

### GET /api/admin/logs/stream

실시간 로그 SSE 스트림 (`logCapacity > 0` 필요, 아니면 `503`)

먼저 history 이벤트, 이후 줄 단위 log 이벤트와 하트비트를 푸시합니다. EventSource는 헤더를 설정할 수 없으므로 `?api_key=<admin key>`로 인증합니다.

**요청**:

```bash
curl "http://localhost:8080/api/admin/logs/stream?api_key=sk-당신의키"
```

### GET /api/admin/logs/snapshot

현재 로그 버퍼를 JSON 배열로 반환 (`.txt` 첨부 다운로드는 `.../logs/download`)

**요청**:

```bash
curl http://localhost:8080/api/admin/logs/snapshot \
  -H "Authorization: Bearer sk-당신의키"
```

## 사용자 API

`/user` 사용자 패널은 `/api/user/*`로 구동됩니다. 이 엔드포인트들은 admin 게이트를 **거치지 않습니다** — 각 요청은 호출자 **자신의 API-KEY**로 인증되며, handler가 검증 후 데이터를 해당 key로 한정합니다. key는 헤더 3종에서 `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` 순으로 읽습니다(프로토콜 라우트와 달리 **query 파라미터는 받지 않습니다**). `POST /api/user/login`은 여기에 더해 body의 `{apiKey}`를 받으며, body 값이 헤더보다 **우선**합니다. key가 유효하지 않으면 `401`, 본문은 `{"error":"…"}`. 응답은 camelCase, `credits = cost / 0.72`.

### POST /api/user/login

key 검증

**요청**:

```bash
curl -X POST http://localhost:8080/api/user/login \
  -H "Content-Type: application/json" \
  -d '{"apiKey": "sk-당신의키"}'
```

**응답**:

```json
{
  "id": 7,
  "name": "내 Key",
  "spendingLimit": 100,
  "limitUnit": "credits",
  "totalCost": 12.3,
  "totalCredits": 17.08,
  "expiresAt": "2026-12-31T00:00:00Z",
  "durationDays": 30,
  "activatedAt": "2026-07-25T00:00:00Z"
}
```

### GET /api/user/usage

해당 key의 사용량 요약 (`byModel[]` 포함. 페이지 기록은 `.../usage/records?page=&page_size=`)

**요청**:

```bash
curl http://localhost:8080/api/user/usage \
  -H "x-api-key: sk-당신의키"
```

## 운영

### GET /health

헬스 체크 (Docker 프로브 호환, 인증 불필요)

**요청**:

```bash
curl http://localhost:8080/health
```

**응답**:

```json
{"service":"kiro2api","status":"ok","version":"0.3.0"}
```

### GET /v1/ping

탐활 (인증 불필요)

**요청**:

```bash
curl http://localhost:8080/v1/ping
```

**응답**:

```json
{"pong":true}
```

## 에러 코드

| 코드 | 설명 |
|------|------|
| 400 | 파라미터 오류. 성격이 다른 셋이 같은 코드로 옵니다: ①**모델명이 중계의 로컬 매핑에 걸리지 않음** — `message`는 `无法识别的模型名: <보낸 이름>`; ②**업스트림이 그 모델을 확정적으로 거부**(계정 구독 등급에 권한 없음) — `message`는 `Invalid model '<보낸 이름>': not available for the current account. …`이며 재시도도 계정 교체도 하지 않습니다; ③본문 파싱 실패, `previous_response_id` 지정(`previous_response_id is not supported`) 등 형식 오류. **어느 경우에도 응답 본문에 문자열 `INVALID_MODEL_ID`는 실리지 않습니다** — 그것은 중계가 ②를 판정하려고 업스트림 응답에서 찾는 내부 reason 코드일 뿐이며, 이 문서에서 `INVALID_MODEL_ID`라고 쓴 곳은 모두 ②의 상황을 가리키는 약칭입니다 |
| 401 | 미인증 (API Key 누락 또는 무효, `apiKey`가 설정된 경우) |
| 402 | 지출 한도 초과 (`{"type":"error","error":{"type":"billing_error",…}}`. 판정은 요청 진입 시점에 1건당 예상 비용(USD 1.0 ≈ 1.39 크레딧)을 예약해 두고 내리므로, 잔여 한도가 그보다 작아지면 한도를 다 쓰기 전에도 거부됩니다) |
| 403 | 금지 (권한 부족) |
| 404 | 찾을 수 없음 (엔드포인트 없음, 또는 관리 API에서 존재하지 않는 id 지정 — 예: `{"error":"api key not found","id":…}`, 만료된 로그인 세션) |
| 422 | 요청 본문 역직렬화 실패 (기본 `Json` 추출기가 그대로 거부 — 필수 필드 누락이나 타입 불일치. 예: `POST /api/admin/credentials/batch-import`를 `{"data": …}` 래퍼 없이 호출. `Json`을 직접 쓰는 `/api/admin/*`, `POST /api/user/login`, `POST /v1/messages/count_tokens`에서 발생합니다. 반면 프로토콜 4종의 **대화** 엔드포인트(`/v1/chat/completions`, `/v1/responses`, `/v1/messages`, `/v1beta/models/{m}:generateContent`)만은 같은 상황을 각 프로토콜 형태의 `400` 본문으로 변환해 돌려줍니다) |
| 429 | 업스트림 Kiro의 스로틀링(`ThrottlingException` 계열 예외를 변환한 결과). `MAX_RPM_PER_CREDENTIAL` 초과는 이 코드가 되지 않습니다 — 해당 계정이 선택 대상에서 빠져 다른 계정으로 넘어가고, 전부 빠졌을 때만 `503`입니다 |
| 500 | 서버 오류 (내부 오류) |
| 502 | 업스트림 Kiro 실패 |
| 503 | 서비스 사용 불가 (사용 가능한 계정 없음 — 전체 쿨다운/비활성화/RPM 초과, 또는 `logCapacity=0`인데 로그 엔드포인트 호출) |

## 에러 응답 형식

에러 본문은 프로토콜마다 다릅니다:

```json
// OpenAI / Responses 형태 (code는 null이거나 HTTP 상태 코드 숫자)
{
  "error": {
    "message": "오류 설명",
    "type": "error_type",
    "code": null
  }
}
```

```json
// Anthropic 형태
{"type": "error", "error": {"type": "...", "message": "..."}}
```

```json
// Gemini 형태
{"error": {"code": 400, "message": "...", "status": "..."}}
```

## 요청 예제

### Python (OpenAI SDK)

```python
from openai import OpenAI

client = OpenAI(
    api_key="sk-당신의키",
    base_url="http://localhost:8080/v1"
)

response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "안녕하세요"}],
    stream=True
)

for chunk in response:
    print(chunk.choices[0].delta.content or "", end="")
```

### Python (Anthropic SDK)

```python
import anthropic

client = anthropic.Anthropic(
    api_key="sk-당신의키",
    base_url="http://localhost:8080"
)

message = client.messages.create(
    model="claude-sonnet-4.5",
    max_tokens=1024,
    messages=[{"role": "user", "content": "안녕하세요"}]
)

print(message.content[0].text)
```

### Python (Gemini SDK)

```python
from google import genai

client = genai.Client(
    api_key="sk-당신의키",
    http_options={"base_url": "http://localhost:8080/v1beta"}
)

response = client.models.generate_content(
    model="claude-sonnet-4.5",
    contents="안녕하세요"
)

print(response.text)
```

### cURL (스트리밍)

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의키" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "안녕하세요"}],
    "stream": true
  }'
```

### JavaScript (fetch)

```javascript
const response = await fetch('http://localhost:8080/v1/chat/completions', {
  method: 'POST',
  headers: {
    'Content-Type': 'application/json',
    'Authorization': 'Bearer sk-당신의키'
  },
  body: JSON.stringify({
    model: 'claude-sonnet-4.5',
    messages: [{ role: 'user', content: '안녕하세요' }],
    stream: false
  })
});

const data = await response.json();
console.log(data.choices[0].message.content);
```

---

더 많은 정보는 [USAGE](USAGE.md), [DEPLOY](DEPLOY.md) 또는 [루트 README](../../README.md)를 참고하세요.
