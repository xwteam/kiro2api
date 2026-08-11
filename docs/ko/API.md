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

프로토콜 모델 목록 조회 (컴파일 시점에 고정된 카탈로그. 계정 풀·구독 등급으로 필터링되지 않습니다 — 아래 💡 참고)

**요청**:

```bash
curl http://localhost:8080/openai/v1/models \
  -H "Authorization: Bearer sk-당신의키"
```

**응답** (지면 관계로 앞 세 항목만 발췌. 실제로는 아래 「모델 카탈로그」 17종이 그 순서 그대로 실립니다):

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

`object`는 항상 `"model"`이고, `created`(`1700000000`)와 `owned_by`(`"kiro2api"`)는 **모든 항목에 똑같이 박히는 하드코딩 상수**입니다 — 실제 공개 시각이나 소유자가 아니므로 정렬·필터에 쓰지 마십시오.

**모델 카탈로그(세 프로토콜 공통)**:

`GET /v1/models`(=`/openai/v1/models`) · `GET /claude/v1/models` · `GET /v1beta/models`(=`/gemini/v1beta/models`) 세 목록은 **모두 같은 카탈로그를 같은 순서로** 반환합니다(형식만 프로토콜별로 다릅니다). 카탈로그의 id는 전부 이 서비스의 모델명 매핑이 받아들이는 값이므로, **「목록에서 고른 id를 그대로 호출한다」는 흐름이 그대로 성립합니다**.

| # | id | 표시 이름 | 컨텍스트 상한 |
|---|----|----------|--------------|
| 1 | `claude-sonnet-4.5` | Claude Sonnet 4.5 | 200,000 |
| 2 | `claude-sonnet-4.6` | Claude Sonnet 4.6 | 200,000 |
| 3 | `claude-sonnet-5` | Claude Sonnet 5 | 200,000 |
| 4 | `claude-opus-4.5` | Claude Opus 4.5 | 200,000 |
| 5 | `claude-opus-4.6` | Claude Opus 4.6 | 200,000 |
| 6 | `claude-opus-4.7` | Claude Opus 4.7 | 200,000 |
| 7 | `claude-opus-4.8` | Claude Opus 4.8 | 200,000 |
| 8 | `claude-haiku-4.5` | Claude Haiku 4.5 | 200,000 |
| 9 | `claude-fable-5` | Claude Fable 5 | 200,000 |
| 10 | `deepseek-3.2` | DeepSeek 3.2 | 128,000 |
| 11 | `glm-5` | GLM-5 | 128,000 |
| 12 | `qwen3-coder-next` | Qwen3 Coder Next | 256,000 |
| 13 | `minimax-m2.1` | MiniMax M2.1 | 192,000 |
| 14 | `minimax-m2.5` | MiniMax M2.5 | 192,000 |
| 15 | `gpt-5.6-terra` | GPT-5.6 Terra | 400,000 |
| 16 | `gpt-5.6-luna` | GPT-5.6 Luna | 400,000 |
| 17 | `gpt-5.6-sol` | GPT-5.6 Sol | 128,000 |

> 「표시 이름」은 `GET /claude/v1/models`의 `display_name`과 `GET /api/admin/models`의 `display_name`에만 실립니다(OpenAI·Gemini 목록에는 아예 없는 필드입니다). 「컨텍스트 상한」은 카탈로그가 들고 있는 메타데이터로 `GET /api/admin/models`의 `max_tokens`로만 노출되며, 요청에 넣는 `max_tokens`는 여전히 무시됩니다(위 「생성 파라미터에 대한 주의」 참고).

> 💡 **모델 선택 가이드**: 카탈로그에 있다고 호출이 반드시 통과하는 것은 아닙니다 — 실제 인가는 **Kiro 계정의 구독 등급**이 정하며, 이 서비스는 그 목록을 미리 알지 못합니다.
> - 등급이 낮을수록(무료 등급 `KIRO FREE` 등) 인가되는 모델 수가 적고, 상위 등급(`KIRO PRO+` 등)일수록 많습니다.
> - 어떤 계정에 무엇이 인가되어 있는지 실제로 알아보려면 `POST /api/admin/credentials/{id}/models/refresh`로 그 계정의 업스트림 목록을 가져온 뒤 `GET /api/admin/models`로 확인하십시오.
> - 인가되지 않은 모델을 요청하면 정적 실패가 아니라 명확히 `400`(`INVALID_MODEL_ID`)을 반환합니다 — 헛되이 재시도하지 않고 계정을 손상시키지도 않습니다.
>
> ⚠️ 프로토콜의 `/models`는 계정 풀이나 구독 등급으로 **필터링되지 않으며**, 목록을 만들려고 업스트림을 치지도 않습니다(컴파일 시점 카탈로그를 그대로 내보냅니다). 따라서 목록에 있는 모델이라도 등급이 인가하지 않으면 `400`(`INVALID_MODEL_ID`)이 날 수 있습니다. 반대로 카탈로그에 없는 표기라도 이름 매핑에 걸리면 정상 동작합니다(예: `gpt-5.6` → `gpt-5.6-sol`, `claude-3-5-sonnet` → `claude-sonnet-4.5`). 이름 매핑이 해석해 내는 내부 모델 id는 카탈로그 17종에 라우팅 별칭 `auto`를 더한 18종입니다. 관리 API `GET /api/admin/models`는 계정들의 업스트림 합집합(캐시)을 우선 반환하고, 합집합이 비어 있을 때 **바로 이 카탈로그 17종**으로 대체합니다.


### 이력에서 보완되는 도구 사양

업스트림은 메시지에 `toolUse` / `toolResult` 콘텐츠 블록이 있으면 `toolConfig`가 반드시 존재해야 한다고 요구합니다. 없으면 요청 전체가 `TOOL_CONFIG_MISSING`으로 거부됩니다.

그런데 도구는 데이터 플레인에 도달하기 전에 정당하게 폐기될 수 있습니다. Responses의 내장 도구(`web_search` / `local_shell` / `file_search`)는 OpenAI 자체 서비스가 실행하며 본 중계의 허브에는 등가물이 없어 변환 시 폐기됩니다. 클라이언트가 어떤 턴에서 **내장 도구만** 보내면 `tools`는 빈 배열이 되는 반면 대화 이력의 도구 호출은 그대로 남습니다.

그 결과 요청은 「도구 호출은 있는데 도구 정의는 없는」 형태가 됩니다. 이는 **본 서비스가 스스로 만들어낸** 잘못된 요청이며 호출자의 잘못이 아닙니다.

**현재 동작:** 업스트림으로 보내기 전에 대화 이력에 나타난 모든 도구 이름을 수집하고, 현재 `tools`에 선언되지 않은 것에는 최소 사양(빈 객체 스키마)을 보완합니다. 보완 대상은 모델이 이미 호출한 도구이며, 보완은 요청을 자기 일관되게 만들 뿐입니다. 보완하지 않으면 그 턴은 통째로 실패합니다. 클라이언트가 **선언한** 도구가 우선하며, 같은 이름은 덮어쓰이거나 중복되지 않습니다.

도구가 없고 이력에도 도구 호출이 없으면 동작은 그대로입니다: `toolConfig`를 보내지 않고 작업 유형은 `vibe`로 유지됩니다.

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

**`input` 배열 항목 유형**: 중계 허브로 매핑되는 것은 아래 **세 가지뿐**입니다. `type`을 생략하면 `role` 유무로 message 항목으로 판정합니다. 그 밖의 `type` 문자열(`reasoning`, `local_shell_call` 등 Responses 측 산출물)은 **해당 항목만 건너뛰며**, 더 이상 요청 전체를 거부하지 않습니다. 멀티턴에서는 클라이언트가 직전 `output`을 통째로 되돌려 보내므로 이런 항목이 반드시 포함되며, 오류로 처리하면 **첫 턴은 되고 둘째 턴에서 반드시 터졌습니다**(v0.7.1에서 수정).
> ⚠️ **`tools` 배열의 내장 도구는 폐기됩니다.** OpenAI 규격상 `tools`에는 `type:"function"` 외에 `web_search`, `local_shell`, `file_search` 같은 **내장 도구**도 들어갑니다. 이들은 OpenAI 자체 서비스가 실행하며 **규격상 `name` 필드 자체가 없습니다**. 본 서버의 허브에는 등가물이 없고 대신 실행할 수도 없으므로, **파싱한 뒤 폐기하고 WARN을 남깁니다**(`responses_builtin_tool_dropped`, `tool_type` 포함). 요청 자체가 실패하지는 않습니다(v0.7.1 이전에는 `400 tools[N]: missing field name`으로, 내장 도구 하나가 턴 전체를 무너뜨렸습니다). **영향**: 모델은 해당 내장 기능(웹 검색 등)을 사용할 수 없습니다. `name`이 있는 `function` / `custom` 도구는 종전대로 동작하며, `parameters`는 생략 가능하고 빈 객체 스키마로 처리됩니다.


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

Claude(Anthropic) 형식 모델 목록. 베어 `/v1/models`가 OpenAI 형식을 반환하므로, Anthropic 형태 목록이 필요하면 이 경로를 사용합니다. 내용물은 위 「모델 카탈로그」 17종으로 OpenAI·Gemini 목록과 **완전히 같고 순서도 같습니다** — 형식만 Anthropic 규격일 뿐, Claude 계열만 골라 담지 않습니다(`deepseek-3.2`, `glm-5`, `gpt-5.6-*` 등도 그대로 실립니다).

**요청**:

```bash
curl http://localhost:8080/claude/v1/models \
  -H "Authorization: Bearer sk-당신의키"
```

**응답** (앞 두 항목만 발췌):

```json
{
  "data": [
    {"type": "model", "id": "claude-sonnet-4.5", "display_name": "Claude Sonnet 4.5", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "claude-sonnet-4.6", "display_name": "Claude Sonnet 4.6", "created_at": "2026-01-01T00:00:00Z"}
  ],
  "has_more": false,
  "first_id": "claude-sonnet-4.5",
  "last_id": "gpt-5.6-sol"
}
```

> 이 엔드포인트만 Anthropic 공개 규격에 맞춰 **snake_case**입니다. `created_at`은 모든 항목에 똑같이 박히는 하드코딩 상수 `"2026-01-01T00:00:00Z"`이고, `has_more`는 **항상 `false`**(커서 페이지네이션 미구현)이며, `first_id`/`last_id`는 카탈로그의 첫·마지막 id입니다.

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

Gemini 모델 목록. 내용물은 위 「모델 카탈로그」 17종으로 OpenAI·Anthropic 목록과 같고 순서도 같으며, Gemini 규격대로 `name`에 `models/` 접두사가 붙습니다.

**요청**:

```bash
curl http://localhost:8080/gemini/v1beta/models \
  -H "Authorization: Bearer sk-당신의키"
```

**응답** (앞 두 항목만 발췌):

```json
{
  "models": [
    {"name": "models/claude-sonnet-4.5", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/claude-sonnet-4.6", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]}
  ]
}
```

> `supportedGenerationMethods`는 전 항목 공통의 하드코딩 상수 2종입니다(`countTokens`는 여기에 넣지 않습니다). `displayName`은 채우지 않으므로 **응답에 아예 나타나지 않습니다** — 표시 이름이 필요하면 `GET /claude/v1/models` 또는 `GET /api/admin/models`를 쓰십시오.

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
      "statusReason": "none",
      "throttleCount": 0
    }
  ]
}
```

> **`failureCount`는 누적 실패 수, `throttleCount`는 제한 이벤트 수입니다.** 예전에는 뒤바뀌어 있어 정지된 계정이 「제한 1, 실패 0」으로 표시되었습니다.

`statusReason`은 **마지막 실패 사유**를 기록합니다(`none` / `banned` = 업스트림이 계정을 정지 / `quota` / `token_expired` / `throttled` / `refresh_denied`).

> **`banned`는 계정을 실제로 풀에서 제외합니다.** 나머지 사유는 표시에만 영향을 줍니다. 쿨다운은 타이머라 시간이 지나면 스스로 풀리지만, 정지는 업스트림이 내린 결론(원문: 「계정을 잠갔습니다. 신원 확인을 위해 지원팀에 문의하세요」)이며 기다린다고 해제되지 않습니다. 타이머만 보고 복귀시키면 쿨다운이 끝나는 순간 다시 선택되고, 다시 실패하고, 다시 쿨다운 — 실제 요청을 계속 소모하면서도 `available`은 여전히 사용 가능으로 셉니다. 패널은 「정지」라고 표시하는데 카운트는 문제없다고 말하는, 서로 모순되는 두 숫자입니다. 따라서 정지된 계정은 선택되지 않고 `available`에도 포함되지 않으며 `healthStatus`는 `unhealthy`를 반환합니다. 스스로 회복하지 않습니다(라벨을 지울 성공이 영영 오지 않으므로). **유일한 복귀 수단은 패널의 「초기화」**입니다(`POST /api/admin/credentials/{id}/reset`, 이 결론도 함께 지웁니다). 나머지 사유는 종전대로 다음 성공 시 초기화됩니다. 이 결론은 `credentials.json`(`statusReason` 키)에 함께 저장되고 기동 시 복원됩니다. 메모리에만 두면 배포할 때마다 지워져 계정이 조용히 풀로 돌아가기 때문입니다. **strike 수와 쿨다운 마감 시각은 여전히 저장하지 않습니다**. 그것들은 타이머라 처음부터 시작해도 계정을 조금 일찍 재시도할 뿐입니다. 결론은 다릅니다 — 계정이 풀에 들어갈 수 있는지를 결정하니까요.



> **v0.17.0: 세 프로토콜의 계약 변경**
>
> - **OpenAI / Gemini / Responses에서 extended thinking을 켤 수 있습니다**(이전에는 허브 요청에서
>   하드코딩으로 꺼져 있어 무엇을 보내도 적용되지 않았습니다). 각 프로토콜 고유의 방식을 사용합니다:
>   OpenAI의 `reasoning_effort`, Gemini의 `thinkingConfig`, Responses의 `reasoning`.
> - **응답 신규 필드**: OpenAI는 `choices[].delta.reasoning_content`(`content`와 나란한 독립 필드로,
>   모르는 클라이언트는 그냥 무시), Gemini는 `thought: true`인 part, Responses는 `reasoning` 출력 항목과
>   `response.reasoning_summary_text.delta`. **켜기만 하고 분리하지 않으면 해롭습니다** —
>   사고 내용이 본문에 섞이므로 둘은 반드시 함께 씁니다.
> - **이 세 프로토콜에 세션 식별자가 붙습니다.** 다중 턴 대화가 상류에서 매번 새 세션으로 보이지 않습니다.
> - **Gemini 스트리밍에 SSE 킵얼라이브**: 상류 첫 바이트가 느릴 때 중간 프록시가 연결을 끊지 않습니다.
> - **CORS 계층 추가**: 브라우저의 교차 출처 클라이언트가 프로토콜 엔드포인트를 직접 호출할 수 있습니다.
> - **`KIRO_API_KEY` 환경 변수 추가**: Kiro API 키 하나만으로 서비스를 기동합니다(마운트 볼륨에 자격 증명
>   파일 불필요. 시작 시 계정 풀에 병합·저장되며 동일 키는 중복 가져오지 않음).
>
> **v0.17.1**: 위 세 스트리밍 출구가 **응답 끝부분을 삼킬** 수 있었습니다(끝이 `<`일 때, 코드의 `<div` 등).
> 수정되었습니다. 네이티브 Anthropic 출구는 영향 없음.

> **v0.16.0: 관리 측 두 가지 동작 변경**
> - **우선순위 변경이 즉시 반영됩니다.** 이전에는 고정 선택(스티키) 하에서 재선택이 일어나지
>   않아, 현재 계정이 쓸 수 없게 될 때까지 변경이 효과가 없었습니다.
> - **모든 계정이 정지된 뒤 풀이 자가 복구합니다.** 상류의 일시적 장애로 전부 정지되어도 누군가
>   재시작할 때까지 완전히 사용 불능이 되지 않습니다. 자가 복구는 영구 비활성화·차단·한도 복구
>   시각 이전의 계정을 되살리지 않습니다.
> **v0.15.0: 두 가지 동작 변경**
> - **한도 소진을 복구 시각과 함께 저장**(자격 증명에 `quotaResetUnix` 추가). 이전에는 메모리에만
>   있어 재시작할 때마다 잊고 사용자 요청으로 다시 발견했습니다(처음 몇 번은 실패). 이제는
>   재시작 후에도 기억하며 **시각이 되면 자동으로 풀에 복귀**합니다. 복구 시각은 잔액 API의
>   `nextResetAt`을 우선 사용합니다. 관리 화면의 "재설정"은 이 표시를 지웁니다.
> - **서버 내장 검색 `web_search`가 실제로 동작합니다**. 이 도구만 선언한 경우 본 서비스가 요청을
>   가로채 상류 MCP 엔드포인트를 호출하고 `server_tool_use`와 `web_search_tool_result` 두 개의
>   콘텐츠 블록을 반환합니다. 다른 도구와 섞여 있으면 가로채지 않습니다(무엇을 호출할지는 모델이
>   결정하므로). 검색 실패 시 5xx가 아니라 빈 결과를 반환합니다.
> **v0.14.0**: `tlsBackend`를 설정에서 전환할 수 있습니다(`native-tls` 기본 / `rustls`, 재시작 시 적용).
> 자체 서명 CA 프록시 뒤에서는 보통 한쪽만 핸드셰이크에 성공하기 때문입니다.
> 인식할 수 없는 값은 경고 후 기본값으로 폴백하며 기동을 막지 않습니다.
> **v0.13.0**: `thinking`이 실제로 동작합니다(업스트림이 이해하는 지시로 변환하고, 응답에서는
> 독립적인 `thinking` 블록으로 반환합니다. 스트리밍은 `thinking_delta`. 다른 세 프로토콜에서는
> 사고 내용을 본문에 통합하며 버리지 않습니다). 토큰 추정은 문자 종류별 가중치를 적용하며
> (이전에는 중국어를 약 3배 과소평가), 스트리밍 입력 토큰도 기존의 **0 고정**에서 추정값으로
> 바뀌었습니다. 모델 목록에 **`context_window`**를 추가해 `max_tokens`와 분리했습니다.
> **v0.12.0: 선택 우선순위와 혼합 등급 풀** —— `priority`(숫자가 작을수록 우선)가 **실제로
> 선택에 반영됩니다**(이전에는 `weight`의 별칭이라 무효). **가져온 계정은 일괄 `999`**(최저)
> 이며 필요하면 수동으로 설정합니다. 등급이 섞인 경우 `/v1/models`는 전체 계정의 **합집합**을
> 반환하며, 릴레이는 해당 모델을 제공하지 않는 계정을 건너뛰고 **어떤 계정도 제공하지 않을
> 때만** `400`을 반환합니다.
> **v0.11.0의 도구 계약 변경:** `tools[].type`를 받습니다(서버 측 내장 도구는 `input_schema`가
> **v0.11.1: 도구 `description`은 전송 시 항상 비어 있지 않습니다.** 업스트림은 빈 설명에 `400 Invalid tool use format / REQUEST_BODY_INVALID`로 응답하며 **요청 전체**를 거부합니다. 설명이 없으면 도구 이름으로 대체합니다. 이 reason은 **결정적** 오류로 분류되어 곧바로 `400`을 반환합니다.
> 없는데 해당 필드가 필수여서 요청 전체가 400이었습니다). `input_schema`는 업스트림이 확실히
> 받아들이는 형태로 **정규화**됩니다(형태만, 의미는 변경 없음). `name`이 **63**자를 넘으면
> 축약해 전송하고 응답에서는 선언한 이름으로 복원합니다. `description`은 항상 문자열입니다.
> 그 외 `POST /api/admin/credentials/{id}/refresh` 추가.
> **프록시 필드는 v0.10.1부터 실제로 동작합니다.** 이전에는 `proxyUrl` / `proxyUsername` /
> `proxyPassword`를 API가 받아도 **저장하지 않았고**, `hasProxy`는 항상 `false`였습니다.
> 이제 우선순위는 **자격 증명 > 전역 > 직결**이며, 자격 증명에 `"direct"`를 넣으면 해당 계정은
> 명시적으로 직결합니다. `http://` / `https://` / `socks5://`를 지원합니다.
> 한 계정의 **데이터 플레인, 토큰 갱신, 잔량 조회, 모델 목록, 백그라운드 갱신은 모두 같은
> 출구**를 사용합니다.
> **v0.10.0부터, 사용 불가로 판정된 계정은 스스로 풀에 복귀하지 않습니다.** 이전에는 `banned`만
> 그러했습니다: 한도 소진(402)은 30분, 명확한 무효화 신호가 없는 401/403이 두 번 연속되면 5분의
> 쿨다운을 거쳐 모두 로테이션으로 돌아왔습니다. 그 결과 **이미 업스트림에서 정지된** 계정이
> 5분마다 다시 사용되기를 멈추지 않았고, 한도를 소진한 계정은 하루 48번 반드시 실패하는 벽에
> 부딪혔습니다. 이제 이 두 종류는 `banned`와 마찬가지로 사용이 중단됩니다.
> `banned`와의 차이는 **지속성**입니다: 이쪽은 메모리 상태여서 재시작하거나 「초기화」하면
> 복구되며, 업스트림의 일시적 권한 흔들림이 영구 손실이 되지 않습니다. `banned`는
> `statusReason`으로 디스크에 저장됩니다. 429는 이 대상이 아닙니다 — 일시적 스로틀링으로
> 재분류되었습니다(백오프 후 재시도, 계정에 불이익 없음).

> [!NOTE]
> - 풀은 요청마다 계정을 고르므로 "현재 계정"이라는 지속 상태가 존재하지 않습니다. `currentId`는 **항상 `-1`**, `isCurrent`는 **항상 `false`**입니다(장래의 고정 선택 모드를 위한 예약 필드) — 이 두 값으로 분기하지 마십시오.
> - `priority`는 별도 값이 아니라 풀 가중치(`weight`)를 그대로 반영하므로 언제나 `weight`와 같습니다.
> - `healthStatus`가 가질 수 있는 값은 `disabled` | `unhealthy`(쿨다운 중 또는 업스트림 정지) | `warning`(실패 누적) | `healthy` 넷뿐입니다.

### POST /api/admin/credentials

자격 증명 1건을 풀에 추가하고 영속화

필수 필드는 `refreshToken` 하나뿐입니다(`authMethod`가 `idc`면 `clientId` + `clientSecret`도 필요). access token과 만료 시각은 **여기서 받지 않습니다** — 첫 자동 갱신 때 채워지며, 그 밖의 알 수 없는 키는 거부되지 않고 조용히 무시됩니다.

**Kiro API Key(`ksk_…`)로 가져오기**는 또 다른 경로입니다: `refreshToken` **대신** `kiroApiKey`(별칭 `ksk`)를 보냅니다. 이런 자격 증명에서는 **키 자체가 데이터 플레인 bearer**이며 교환·갱신·만료가 없고 OAuth 경로를 전혀 거치지 않으므로 `refreshToken`·`clientId`·`clientSecret`·`expiresAt` 모두 필요 없습니다.

```bash
curl -X POST http://localhost:8080/api/admin/credentials \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-관리자키" \
  -d '{"kiroApiKey": "ksk_xxx"}'
```

디스크 형태(`credentials.json`을 직접 편집해도 됩니다):

```json
{"kiroApiKey": "ksk_xxx", "authMethod": "api_key"}
```

**`kiroApiKey`가 있으면 `authMethod`에 무엇이 적혀 있든 API Key로 처리합니다** —— `idc`를 선언하면서 키를 지닌 경우 「clientId와 clientSecret 필수」 검증에 걸려 사실은 완전한 자격 증명이 미비로 판정되기 때문입니다.

반대로 `authMethod: api_key`를 선언하면서 `kiroApiKey`가 없는 자격 증명은 자기모순입니다: 제시할 bearer가 없는데도 API Key 자격 증명으로 판정되어 갱신되지 않고, 계정 간 재시도 속에서 선택될 때마다 같은 지점에서 실패합니다. 이런 자격 증명은 **로드 시 비활성화**되며 **「초기화」로도 되살아나지 않습니다** —— 초기화는 strike·쿨다운·결론만 지울 뿐 설정을 바꾸지 못하므로 되살려도 같은 실패로 되돌아갑니다. 설정을 고치고 재시작하세요.


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

일별(CST=UTC+8) 사용량 요약, 날짜 내림차순. 특정 날짜의 개별 기록은 아래 `.../daily/{date}/records`, 계정 단위는 `/api/admin/credentials/{id}/usage/records`와 `.../usage/today` 항목을 보십시오.

**요청**:

```bash
curl http://localhost:8080/api/admin/usage/daily \
  -H "Authorization: Bearer sk-당신의키"
```

### GET /api/admin/usage/daily/{date}/records

특정 CST 날짜의 사용량 기록 페이지 조회

`{date}`는 `YYYY-MM-DD`(CST=UTC+8) 형식입니다. 최신순(내림차순)으로 정렬한 뒤 **2000건까지 자르고 나서** 페이지를 나누므로, `total`은 그날의 전체 건수가 아니라 잘린 뒤의 건수입니다. 존재하지 않는 날짜나 빈 저장소도 `404`가 아니라 빈 페이지(`total: 0`, `totalPages: 0`, `page: 1`)를 200으로 돌려줍니다.

**페이지 파라미터(관리 API 공통)**: `page`(기본 `1`) / `page_size`(기본 `20`). 이름은 응답과 달리 **snake_case**입니다. 범위를 벗어난 `page`는 `[1, totalPages]`로 조여지고 `page_size=0`은 1로 올라갑니다.

**요청**:

```bash
curl "http://localhost:8080/api/admin/usage/daily/2026-07-25/records?page=1&page_size=20" \
  -H "Authorization: Bearer sk-당신의키"
```

**응답**:

```json
{
  "records": [
    {
      "model": "claude-sonnet-4.5",
      "inputTokens": 1200,
      "outputTokens": 340,
      "estimatedCost": 0.0123,
      "creditsUsed": 0.25,
      "cacheReadInputTokens": 0,
      "cacheCreationInputTokens": 0,
      "createdAt": "2026-07-25T09:12:33Z",
      "credentialId": 12345,
      "credentialLabel": "a@example.com",
      "clientIp": "203.0.113.7"
    }
  ],
  "total": 1,
  "page": 1,
  "pageSize": 20,
  "totalPages": 1
}
```

> `creditsUsed` / `cacheReadInputTokens` / `cacheCreationInputTokens` / `credentialLabel` / `clientIp`는 값이 없으면 `null`이 아니라 **필드 자체가 빠집니다**. `creditsUsed`는 업스트림이 보고한 실제 적립금 소비량이지 `estimatedCost`에서 환산한 값이 **아닙니다**(업스트림이 주지 않으면 필드가 없고, 이 API는 비용에서 역산하지 않습니다). `credentialLabel`은 저장된 값이 아니라 계정 풀에서 닉네임 → 이메일 → `#{id}` 순으로 만들어 붙이는 표시용 이름입니다. `creditsSaved`는 이 저장소가 산출하지 않으므로 **항상 빠집니다**.

### GET /api/admin/usage/summary

시간 창 단위 사용량 집계 + 운영 건강 지표 (전 계정 합산)

쿼리 파라미터는 둘 중 하나입니다 — `range`(`6h` | `24h` | `3d` | `7d` | `30d`, 이쪽이 우선) 또는 `hours`(양의 정수). 둘 다 없으면 **24h**로 봅니다. 열거값 밖의 `range`는 `400` + `{"error":"invalid range","allowed":["6h","24h","3d","7d","30d"],"hint":"use ?range=<enum> or ?hours=<positive int>"}`, `hours=0`은 `400` + `{"error":"hours must be a positive integer"}`. 빈 저장소나 활동 없는 창은 200 + 전부 0입니다.

**요청**:

```bash
curl "http://localhost:8080/api/admin/usage/summary?range=24h" \
  -H "Authorization: Bearer sk-당신의키"
```

**응답**:

```json
{
  "range": "24h",
  "windowSecs": 86400,
  "sinceUnix": 1753400000,
  "untilUnix": 1753486400,
  "bucketSecs": 3600,
  "totalRequests": 128,
  "totalInputTokens": 240000,
  "totalOutputTokens": 61000,
  "totalCost": 1.2345,
  "totalCredits": 32.0,
  "dailyFallbackApplied": false,
  "series": [
    {"bucketStartUnix": 1753400000, "totalRequests": 12, "totalCost": 0.11, "totalCredits": 0.15}
  ],
  "successfulRequests": 128,
  "failedRequests": 3,
  "errorRate": 0.022900763358778626,
  "avgLatencyMs": 1843.5,
  "rotationSuccessRate": 0.9770992366412213
}
```

> - `range`는 입력을 정규화해 되돌려 준 라벨입니다(`hours=N`으로 물었으면 `"<N>h"`). 창은 `[untilUnix - windowSecs, untilUnix]`이고 `untilUnix`는 요청 처리 시각입니다.
> - `bucketSecs`는 창이 24시간 이하면 3600(시간별), 그보다 길면 86400(일별)입니다. `series`는 버킷 시작 오름차순이며 활동이 없으면 빈 배열입니다.
> - 창이 **1일보다 길면** 원본 기록의 계정당 상한(10000건) 탓에 오래된 건이 이미 밀려났을 수 있어, 창에 통째로 들어가는 CST 하루마다 일별 집계와 대조해 `max(원본, 일별)`의 차액으로 requests/cost/credits만 메웁니다. 그렇게 메웠으면 `dailyFallbackApplied: true`가 되며, **토큰 합계는 메우지 않으므로 낮게 나올 수 있습니다**.
> - `failedRequests`는 창 안의 실패 로그(401/403) + 스로틀 로그(429) 건수입니다. 이벤트 로그는 계정당 500건 LRU 상한이 있어 이 값은 **하한**이고, 따라서 `errorRate`는 보수적으로(낮게) 나옵니다.
> - `avgLatencyMs`는 지연이 기록된 성공 건만의 평균이며(옛 기록에는 지연이 없습니다) 표본이 하나도 없으면 `0.0`입니다.
> - `rotationSuccessRate`는 `1 − errorRate`인 **근사치**입니다 — 계정 교체 재시도 자체를 따로 계측하지 않고 "성공 기록이 남았는가"를 성공 신호로 삼습니다. 창 안에 활동이 전혀 없으면 `errorRate: 0.0` / `rotationSuccessRate: 1.0`.
> - `totalCredits`는 각 기록에 실린 **업스트림 보고 적립금 소비량의 합**입니다 — `totalCost`에서 환산한 값이 아니며, 적립금 값이 없는 기록은 0으로 칩니다.
> - 수치는 전부 반올림하지 않은 원본이므로 자릿수는 클라이언트가 알아서 다듬으십시오.

### GET /api/admin/credentials/{id}/usage/today

계정 1건의 **오늘**(CST=UTC+8) 사용량 요약

쿼리 파라미터 없음. 알 수 없는 id도 `404`가 아니라 전부 0인 요약을 200으로 돌려줍니다. `credentialId`는 경로의 id를 u32로 파싱한 값이며, 숫자가 아니면 `0`이 됩니다.

**요청**:

```bash
curl http://localhost:8080/api/admin/credentials/12345/usage/today \
  -H "Authorization: Bearer sk-당신의키"
```

**응답**:

```json
{
  "date": "2026-07-25",
  "credentialId": 12345,
  "totalRequests": 42,
  "totalInputTokens": 81000,
  "totalOutputTokens": 20500,
  "totalCost": 0.4321,
  "totalCredits": 10.5
}
```

> `totalCreditsSaved`는 이 저장소가 산출하지 않아 **항상 빠집니다**. `totalCredits`는 업스트림이 보고한 적립금 소비량의 합이며 `totalCost`에서 환산한 값이 아닙니다. 집계 대상은 살아 있는 원본 기록뿐이므로, 오늘 하루 요청이 계정당 상한(10000건)을 넘길 만큼 많으면 밀려난 만큼 낮게 나옵니다.

### GET /api/admin/credentials/{id}/balance

계정 잔액 (5분 캐시)

**요청**:

```bash
curl http://localhost:8080/api/admin/credentials/12345/balance \
  -H "Authorization: Bearer sk-당신의키"
```

### GET /api/admin/credits/global

전 계정 잔여 적립금 합계 (**캐시만 읽고 업스트림은 절대 호출하지 않음**). 캐시에 있는 **모든** 스냅샷을 합산하며 **TTL로 걸러내지 않습니다**. TTL(5분)은 「업스트림에 다시 조회할지」에 답하는 것이지 「표시할지」를 정하는 것이 아닙니다. 신선한 항목만 합산하던 때에는 계정 페이지를 5분만 열지 않아도 홈 화면의 전체 적립금이 공백이 되었고, 모든 계정의 잔액이 디스크에 있는데도 수동 새로고침을 강요했습니다 —— 그 새로고침이야말로 이 캐시가 피하려던 업스트림 호출입니다. 캐시 자체가 없는 계정은 종전대로 건너뛰며 여기서 채우지 않습니다. `oldestCacheUnix`는 합산에 사용된 스냅샷 중 가장 오래된 취득 시각으로, UI가 데이터의 나이를 표시하는 데 씁니다.

풀에 있는 각 계정의 잔액 캐시(5분 TTL) 중 **아직 신선한 것만** 골라 `remaining`을 더합니다. 캐시가 없거나 만료된 계정은 그냥 건너뜁니다 — 이 엔드포인트는 새 값을 가져오지 않으며, 캐시를 채우는 쪽은 위 `GET /api/admin/credentials/{id}/balance`입니다. 쿼리 파라미터 없음, 항상 200.

**요청**:

```bash
curl http://localhost:8080/api/admin/credits/global \
  -H "Authorization: Bearer sk-당신의키"
```

**응답**:

```json
{
  "globalCredits": 1234.5,
  "cachedCount": 3,
  "totalCount": 5,
  "oldestCacheUnix": 1753486000
}
```

> `cachedCount`(합계에 실제로 참여한 계정 수)가 `totalCount`(풀 전체 계정 수)보다 작으면 그 차이만큼 **합계가 과소 집계된 상태**라는 뜻이므로 그대로 "전체 잔액"이라고 표시하지 마십시오. `oldestCacheUnix`는 합계에 참여한 캐시 중 가장 오래된 것의 취득 시각(Unix 초)이며, 참여한 계정이 하나도 없으면 **`null`**입니다(필드는 빠지지 않고 항상 나옵니다).

### GET /api/admin/credentials/{id}/failure-logs

최근 실패 이벤트(401/403). 429 스로틀 이벤트는 아래 `.../throttle-logs` 항목을 보십시오.

**요청**:

```bash
curl http://localhost:8080/api/admin/credentials/12345/failure-logs \
  -H "Authorization: Bearer sk-당신의키"
```

### GET /api/admin/credentials/{id}/throttle-logs

계정 1건의 429 스로틀 이벤트 페이지 조회

페이지 파라미터는 관리 API 공통(`page` 기본 `1` / `page_size` 기본 `20`)이고, 응답 모양도 위 `.../failure-logs`와 같습니다. 알 수 없는 id나 빈 저장소도 `404`가 아니라 빈 페이지를 200으로 돌려줍니다.

**요청**:

```bash
curl "http://localhost:8080/api/admin/credentials/12345/throttle-logs?page=1&page_size=20" \
  -H "Authorization: Bearer sk-당신의키"
```

**응답**:

```json
{
  "records": [
    {
      "credentialId": 12345,
      "requestType": "api",
      "statusCode": 429,
      "responseBody": "{\"message\":\"Too many requests\"}",
      "createdAt": "2026-07-25T09:12:33Z"
    }
  ],
  "total": 1,
  "page": 1,
  "pageSize": 20,
  "totalPages": 1
}
```

> `statusCode`는 이 로그에서 **항상 429**입니다(기록 시점에 상수로 박습니다). `requestType`도 현재 중계가 남기는 값이 `"api"` 하나뿐입니다. `responseBody`는 업스트림 응답 본문을 **200자**에서 자른 것입니다(실패 로그 쪽은 2000자). 이벤트는 계정당 500건 LRU 상한이라 오래된 것부터 밀려납니다.

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

### GET /api/admin/check-update

GitHub Releases의 최신 버전과 현재 실행 중인 버전 비교 (읽기 전용, 아무것도 바꾸지 않음)

`https://api.github.com/repos/xwteam/kiro2api/releases/latest`를 조회해 `tag_name`에서 앞의 `v`를 떼고 빌드에 박힌 버전과 **문자열 비교**합니다. 네트워크 실패·릴리스 없음·비공개 저장소 404 등은 오류로 만들지 않고 보수적으로 `hasUpdate: false` / `latest = current` / `updateUrl = https://github.com/xwteam/kiro2api/releases` / `releaseNotes = ""`로 답합니다 — **항상 200**이므로 "확인에 실패했는지"는 이 응답만으로 구분할 수 없습니다.

**요청**:

```bash
curl http://localhost:8080/api/admin/check-update \
  -H "Authorization: Bearer sk-당신의키"
```

**응답**:

```json
{
  "current": "0.4.0",
  "latest": "0.4.1",
  "hasUpdate": true,
  "updateUrl": "https://github.com/xwteam/kiro2api/releases/tag/v0.4.1",
  "releaseNotes": "릴리스 노트 본문"
}
```

> `hasUpdate`는 `latest != current`라는 단순 문자열 불일치이므로 태그가 되돌아가면 다운그레이드도 `true`가 됩니다(의미 기반 버전 비교가 아닙니다). `updateUrl`은 릴리스의 `html_url`이고 그게 없으면 저장소 릴리스 목록 주소로 대체하며, `releaseNotes`는 릴리스 본문 원문(없으면 빈 문자열)입니다.

### POST /api/admin/update

업데이트 **명령문만** 돌려줍니다 — 서버는 아무것도 실행하지 않습니다

요청 본문 없음. 세 필드 모두 하드코딩된 상수이고 실제 업데이트는 운영자가 서버에서 직접 실행해야 합니다(패널은 이 문자열을 복사 버튼과 함께 보여줄 뿐입니다). 항상 200.

**요청**:

```bash
curl -X POST http://localhost:8080/api/admin/update \
  -H "Authorization: Bearer sk-당신의키"
```

**응답**:

```json
{
  "status": "ok",
  "message": "请在服务器上执行以下命令完成更新:",
  "command": "docker compose pull && docker compose up -d"
}
```

> `message`는 서버가 내보내는 **문자 그대로의 값**이며 현재 중국어로 하드코딩되어 있습니다(뜻: "서버에서 다음 명령을 실행해 업데이트를 완료하세요:"). `status`도 상수 `"ok"`라 성공/실패 신호로 쓸 수 없습니다.

### POST /api/admin/restart

프로세스를 종료해 재기동을 유도합니다 (**파괴적 · 2차 확인 필수**)

`?confirm=true`가 없으면 아무 일도 하지 않고 `400`입니다. 확인이 있으면 먼저 200을 돌려준 뒤, 백그라운드에서 0.5초 기다렸다가 디바운스 저장소(사용량 통계 · API Key · 잔액 캐시 · 실패/스로틀 이벤트 로그)를 전부 디스크에 내리고 `exit(0)` 합니다. **되살리는 주체는 이 서비스가 아니라 실행 환경입니다** — `restart: unless-stopped`로 도는 컨테이너면 곧바로 다시 뜨지만, 감시자 없는 베어메탈에서는 그냥 정지이므로 systemd/supervisor 같은 보호 장치가 필요합니다.

**요청**:

```bash
curl -X POST "http://localhost:8080/api/admin/restart?confirm=true" \
  -H "Authorization: Bearer sk-당신의키"
```

**응답**:

```json
{"status": "ok", "message": "Server restarting..."}
```

두 값 모두 하드코딩된 상수입니다. 확인 없이 부르면 `400`:

```json
{"error": {"message": "重启需二次确认,请带查询参数 ?confirm=true", "type": "confirmation_required"}}
```

> 이 `message`도 서버가 내보내는 문자 그대로의 값입니다(뜻: "재시작에는 2차 확인이 필요합니다. 쿼리 파라미터 `?confirm=true`를 붙이십시오"). 분기에는 문구가 아니라 `type: "confirmation_required"`를 보십시오.

### GET /api/admin/models

`display_name` / `type` / `max_tokens`를 포함한 모델 목록. 동작은 그대로입니다 — 계정들의 업스트림 `ListAvailableModels` 합집합(캐시)이 비어 있지 않으면 그것을 반환하고, 비어 있으면 세 프로토콜의 `/models`가 쓰는 것과 **똑같은 카탈로그 17종**으로 대체합니다. 즉 캐시가 식어 있는 동안에는 프로토콜 목록과 내용이 같고, 캐시가 채워지면 계정 등급이 실제로 인가한 집합을 반영합니다(합집합에만 있는 모델도, 카탈로그에만 있고 합집합에는 없는 모델도 생길 수 있습니다). 응답 필드명은 패널에 맞춰 **snake_case**입니다(`display_name`, `owned_by`, `max_tokens`, 그리고 `type`).

합집합이 비어 있을 때는 이번 응답을 정적 카탈로그로 돌려주면서 뒤에서 회수를 한 번 시도합니다(응답을 막지 않으며, 단일 비행 + 60초 쿨다운 + 실패 상한이 걸려 있습니다). 즉시 채우고 싶으면 아래 `POST /api/admin/credentials/models/refresh`를 쓰십시오.

**요청**:

```bash
curl http://localhost:8080/api/admin/models \
  -H "Authorization: Bearer sk-당신의키"
```

**응답** (앞 한 항목만 발췌):

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

> `created`(`1700000000`)와 `type`(`"chat"`)은 하드코딩 상수입니다. `owned_by`는 정적 카탈로그로 대체할 때 `"kiro2api"`이고, 업스트림 합집합을 낼 때는 업스트림 값이 아니라 **id에서 추론한 제공자**(`anthropic` / `openai` / `deepseek` / `minimax` / `glm` / `qwen` / `kiro`, 어디에도 안 걸리면 `unknown`)가 들어갑니다. 합집합 경로의 `display_name`은 업스트림 값이며 업스트림이 주지 않으면 id를 그대로 씁니다. `rate_multiplier`는 업스트림이 주지 않으면 **필드 자체가 빠집니다**(정적 대체 경로에서는 항상 빠집니다). 업스트림 항목의 `max_tokens`가 0이면 200000으로 채워 내보냅니다.

### POST /api/admin/credentials/models/refresh

구독 등급별 대표 계정만 골라 모델 목록 캐시를 채웁니다 (**업스트림 실호출**)

요청 본문 없음. 전 계정을 훑지 않습니다 — 비활성 계정을 뺀 뒤 잔액 캐시가 등급을 알고 있는 계정들에 대해 **등급마다 대표 1개**만 갱신하고(등급이 다르면 서비스되는 모델도 다르므로 그 합집합이 전 등급을 덮습니다), 등급을 아직 모르는 계정에는 **한계가 걸린 탐색**을 돌립니다: 합집합 크기가 연속 3회 늘지 않거나, 성공이 12건에 도달하거나, 후보가 떨어지면 멈춥니다. 개별 계정 실패는 `errors[]`에 모으고 나머지는 계속 진행하므로 **전부 실패해도 HTTP는 200 + `success: true`**입니다 — 성패는 `failed`와 `errors[]`로 판단하십시오.

**요청**:

```bash
curl -X POST http://localhost:8080/api/admin/credentials/models/refresh \
  -H "Authorization: Bearer sk-당신의키"
```

**응답**:

```json
{
  "success": true,
  "refreshed": 2,
  "failed": 1,
  "errors": [
    {"id": 12346, "error": "models upstream HTTP 403: Your User ID is suspended"}
  ],
  "tiers": ["KIRO FREE", "KIRO PRO+"]
}
```

> 「등급」은 잔액 캐시에 담긴 구독 이름(`KIRO FREE`, `KIRO PRO+` 등)이며, 캐시가 신선(5분 TTL)할 때만 인정됩니다. `tiers`는 이번 호출이 실제로 덮은 등급 목록이고, 끝내 등급을 알 수 없는 계정을 갱신했으면 `"unknown"`이 섞입니다. `errors[].id`는 계정 id를 **숫자로** 파싱한 값이라 숫자가 아닌 id는 `0`이 됩니다(아래 단건 엔드포인트는 같은 id를 문자열로 되돌려 주므로 형이 다릅니다). 재기동 직후처럼 신선한 잔액 캐시가 하나도 없으면 알려진 등급이 0개라 탐색만 돌게 되고, 그 탐색까지 전부 실패하면 `refreshed: 0`으로 끝날 수도 있습니다.

### POST /api/admin/credentials/{id}/models/refresh

계정 1건의 모델 목록을 업스트림에서 실제로 가져와 캐시에 채웁니다

요청 본문 없음. 풀에 없는 id면 `404` + `{"error":"account not found","id":"…"}`이고, 업스트림 호출이 실패하면 `502`입니다. 비활성 계정이라는 이유로 거부하지는 않습니다.

**요청**:

```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/models/refresh \
  -H "Authorization: Bearer sk-당신의키"
```

**응답**:

```json
{"success": true, "id": "12345", "count": 18}
```

`count`는 업스트림 응답을 정규화하고 중복 id를 제거한 뒤 캐시에 넣은 모델 개수입니다(계정 등급에 따라 달라집니다). 업스트림 실패 시 `502`:

```json
{"success": false, "id": "12345", "error": "models upstream HTTP 403: Your User ID is suspended"}
```

> `error`에는 상태 코드와 업스트림 설명이 그대로 실려 화면에 진짜 원인을 띄울 수 있습니다. 토큰류는 포함되지 않습니다.

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

### POST /admin/api/accounts/{id}/disable · POST /admin/api/accounts/{id}/enable (레거시 별칭)

계정 수동 비활성화/활성화. 위 `POST /api/admin/credentials/{id}/disabled`와 **같은 일**(풀에 있는 그 계정의 `disabled` 플래그를 뒤집기)을 하는 구 경로이므로 계약을 여기서 되풀이하지 않습니다. 다만 **모양이 다릅니다**:

- 요청 본문이 없습니다 — 켜고 끄는 것을 본문의 `disabled`가 아니라 **경로**(`/enable` 대 `/disable`)로 정합니다.
- 성공 응답이 `{success,message}`가 아니라 `{ok,id,disabled}`이고, `id`는 경로에 넣은 문자열 그대로입니다.
- 없는 id일 때 `404` + `{"error":"account not found","id":"…"}`인 것은 양쪽이 같습니다.

새로 붙이는 통합에는 `/api/admin/credentials/{id}/disabled` 쪽을 쓰십시오. 두 경로 모두 같은 admin 게이트 뒤에 있습니다.

**요청**:

```bash
curl -X POST http://localhost:8080/admin/api/accounts/12345/disable \
  -H "Authorization: Bearer sk-당신의키"

curl -X POST http://localhost:8080/admin/api/accounts/12345/enable \
  -H "Authorization: Bearer sk-당신의키"
```

**응답**:

```json
{"ok": true, "id": "12345", "disabled": true}
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

해당 key의 사용량 요약 (`byModel[]` 포함). 개별 기록 페이지는 아래 `.../usage/records` 항목을 보십시오.

**요청**:

```bash
curl http://localhost:8080/api/user/usage \
  -H "x-api-key: sk-당신의키"
```

### GET /api/user/usage/records

해당 key의 사용량 기록 페이지 조회 (최신순)

`page`(기본 `1`) / `page_size`(**기본 `50`** — 관리 API 쪽 기본값 20과 다릅니다). 인증은 이 절 머리말대로 헤더 3종에서만 읽으며 쿼리로는 key를 넘길 수 없고, key가 유효하지 않으면 `401` + `{"error":"…"}`입니다. 유효한 key인데 기록이 없으면 빈 페이지를 200으로 돌려줍니다.

**요청**:

```bash
curl "http://localhost:8080/api/user/usage/records?page=1&page_size=50" \
  -H "x-api-key: sk-당신의키"
```

**응답**:

```json
{
  "records": [
    {
      "model": "claude-sonnet-4.5",
      "inputTokens": 1200,
      "outputTokens": 340,
      "estimatedCost": 0.0123,
      "creditsUsed": 0.25,
      "cacheReadInputTokens": 0,
      "cacheCreationInputTokens": 0,
      "createdAt": "2026-07-25T09:12:33Z",
      "clientIp": "203.0.113.7"
    }
  ],
  "total": 1,
  "page": 1,
  "pageSize": 50,
  "totalPages": 1
}
```

> 관리 API의 같은 이름 응답과 달리 `credentialId`라는 필드가 **아예 없고**(어느 계정으로 중계됐는지는 사용자 면에 노출하지 않습니다), `credentialLabel`과 `creditsSaved`도 채우지 않으므로 **항상 빠집니다**. `creditsUsed` / `cacheReadInputTokens` / `cacheCreationInputTokens` / `clientIp`도 값이 없으면 필드째 빠집니다.
>
> 이 절 머리말의 `credits = cost / 0.72`는 **요약**(`POST /api/user/login`·`GET /api/user/usage`의 `totalCredits`)에만 해당하는 환산식입니다. 여기 기록 하나하나의 `creditsUsed`는 업스트림이 보고한 실제 소비량이라 `estimatedCost / 0.72`와 일치하지 않는 것이 정상이며, 둘을 더해 비교하지 마십시오.

## 운영

### GET /health

헬스 체크 (Docker 프로브 호환, 인증 불필요)

**요청**:

```bash
curl http://localhost:8080/health
```

**응답**:

```json
{"service":"kiro2api","status":"ok","version":"0.17.1"}
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
| 400 | 파라미터 오류. 성격이 다른 셋이 같은 코드로 옵니다: ①**모델명이 중계의 로컬 매핑에 걸리지 않음** — `message`는 `无法识别的模型名: <보낸 이름>`; ②**업스트림이 그 모델을 확정적으로 거부**(계정 구독 등급에 권한 없음) — `message`는 `Invalid model '<보낸 이름>': not available for the current account. …`이며 재시도도 계정 교체도 하지 않습니다; ③본문 파싱 실패, `previous_response_id` 지정(`previous_response_id is not supported`) 등 형식 오류. **어느 경우에도 응답 본문에 문자열 `INVALID_MODEL_ID`는 실리지 않습니다** — 그것은 중계가 ②를 판정하려고 업스트림 응답에서 찾는 내부 reason 코드일 뿐이며, 이 문서에서 `INVALID_MODEL_ID`라고 쓴 곳은 모두 ②의 상황을 가리키는 약칭입니다; ③**요청 본문이 업스트림 길이 상한을 초과** —— 업스트림 reason 코드는 `CONTENT_LENGTH_EXCEEDS_THRESHOLD`. 「Input is too long…」과 함께, 이 오류가 스스로 회복되지 않으며(클라이언트가 매 턴 전체 대화를 다시 보내므로 다음 턴은 더 길어짐) 컨텍스트를 줄이거나 대화를 새로 시작해야 함을 알립니다. **이 유형 역시 재시도하지 않으며 계정을 손상시키지 않습니다**(v0.7.12 이전에는 일시적 오류로 오분류되어 계정을 넘나들며 재시도하고 거친 모든 계정에 실패를 기록했습니다); ④**메시지에 도구 호출이 있는데 도구 정의가 없음** —— 업스트림 reason 코드는 `TOOL_CONFIG_MISSING`. 정상적으로는 발생하지 않습니다: 중계가 대화 이력에 나타난 도구 이름으로 최소 사양을 보완해 전송합니다. 이 유형은 안전장치이며 마찬가지로 재시도하지 않고 계정을 손상시키지 않습니다 |
| 401 | 미인증 (API Key 누락 또는 무효, `apiKey`가 설정된 경우) |
| 402 | 지출 한도 초과 (`{"type":"error","error":{"type":"billing_error",…}}`. 판정은 요청 진입 시점에 1건당 예상 비용(USD 1.0 ≈ 1.39 크레딧)을 예약해 두고 내리므로, 잔여 한도가 그보다 작아지면 한도를 다 쓰기 전에도 거부됩니다) |
| 403 | 금지 (권한 부족) |
| 404 | 찾을 수 없음 (엔드포인트 없음, 또는 관리 API에서 존재하지 않는 id 지정 — 예: `{"error":"api key not found","id":…}`, 만료된 로그인 세션) |
| 422 | 요청 본문 역직렬화 실패 (기본 `Json` 추출기가 그대로 거부 — 필수 필드 누락이나 타입 불일치. 예: `POST /api/admin/credentials/batch-import`를 `{"data": …}` 래퍼 없이 호출. `Json`을 직접 쓰는 `/api/admin/*`, `POST /api/user/login`에서 발생합니다. 반면 프로토콜 4종의 **대화** 엔드포인트(`/v1/chat/completions`, `/v1/responses`, `/v1/messages`, `/v1beta/models/{m}:generateContent`)와 `POST /v1/messages/count_tokens`는 같은 상황을 각 프로토콜 형태의 `400` 본문으로 변환해 돌려줍니다) |
| 429 | 업스트림 Kiro의 스로틀링(`ThrottlingException` 계열 예외를 변환한 결과). `MAX_RPM_PER_CREDENTIAL` 초과는 이 코드가 되지 않습니다 — 해당 계정이 선택 대상에서 빠져 다른 계정으로 넘어가고, 전부 빠졌을 때만 `503`입니다 |
| 500 | 서버 오류 (내부 오류) |
| 502 | 업스트림 Kiro 실패. **모든 업스트림 요청에 `Connection: close`를 붙이고 클라이언트는 연결을 재사용하지 않습니다.** 각 요청은 *서로 다른 계정*의 토큰을 싣고 user-agent의 machineId도 계정마다 달라, 연결을 재사용하면 하나의 TCP/TLS 위에 수십 개의 신원이 차례로 나타납니다 —— 실제 클라이언트로는 불가능하며 계정 공유의 가장 직접적인 증거입니다. 일시적 실패(네트워크/5xx/스로틀링)는 계정 전환 전 200ms→2s 지수 백오프+지터, 계정 수준 실패는 대기하지 않습니다 업스트림 연결은 **HTTP/1.1**로 고정됩니다(실제 클라이언트는 h2로 올리지 않으며, `Connection: close`는 1.1 헤더로 h2에서는 금지되어 고정하지 않으면 장식일 뿐입니다). TLS 백엔드는 기본값이 **native-tls(OpenSSL)**로, 실제 클라이언트의 ClientHello 지문에 맞춥니다 —— 이 지문은 HTTP 내용이 전송되기 **전에** 노출됩니다. |
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
