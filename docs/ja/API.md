# API リファレンス

kiro2api の完全な API ドキュメントです。

kiro2api は Kiro（CodeWhisperer）をバックエンドとする多プロトコル AI 中継サービスで、1 つのサービスで OpenAI Chat / Anthropic Messages / OpenAI Responses / Gemini ネイティブの 4 種類の SDK 形式に対応し、統一して Claude 系モデルを提供します。

## 認証

すべてのプロトコルエンドポイントには認証が必要です。ゲートは **6 つ**のチャネルを受け付け、**最初に見つかった 1 つ**を採用します。優先順位は `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > `?api_key=` > `?token=` > `?key=` です（いずれも定数時間比較で照合されます）。

### 方法 1: Authorization ヘッダー（推奨）

```bash
curl -H "Authorization: Bearer sk-あなたのキー" \
  http://localhost:8080/v1/models
```

### 方法 2: x-api-key ヘッダー

```bash
curl -H "x-api-key: sk-あなたのキー" \
  http://localhost:8080/v1/models
```

### 方法 3: x-goog-api-key ヘッダー（Gemini ネイティブ）

```bash
curl -H "x-goog-api-key: sk-あなたのキー" \
  http://localhost:8080/v1beta/models
```

### 方法 4〜6: クエリパラメータ

ヘッダーを設定できないクライアント向けです（ブラウザの `EventSource` など）。公式 Gemini SDK は `?key=` を使います。

```bash
curl "http://localhost:8080/v1/models?api_key=sk-あなたのキー"
curl "http://localhost:8080/v1/models?token=sk-あなたのキー"
curl "http://localhost:8080/v1/models?key=sk-あなたのキー"
```

ヘッダーはクエリパラメータより常に優先され、同じグループ内では上記の並び順で決まります。`/health` と `/v1/ping` は死活監視用で認証不要です。

> **ヒント**: API Key は `.env` の `API_KEY`、`config.json` の `apiKey`、または管理パネルから確認できます。`apiKey`/`API_KEY` が空で、**かつ管理パネルで API-KEY を 1 件も作成していない**場合に限り、プロトコルエンドポイントは**開放アクセス**になります（起動時に警告が出ます）。API-KEY を 1 件でも作成すると、以降プロトコルエンドポイントは有効な API-KEY を要求します。外部公開時は必ず設定してください。

## 標準ベアパス

各プロトコルは 2 種類のパスに同時対応しています。

### プレフィックス付きパス

プロバイダーごとに明示的なパスを使用します（4 つのベンダーを明確に区別する場合）：

- OpenAI: `/openai/v1/chat/completions`、`/openai/v1/models`
- Claude: `/claude/v1/messages`、`/claude/v1/messages/count_tokens`、`/claude/v1/models`
- Gemini: `/gemini/v1beta/models/{model}:generateContent`、`:streamGenerateContent`

### 標準ベアパス

主要 SDK が `base_url` にサフィックス不要でそのまま動作します：

- **OpenAI**: `/v1/chat/completions`、`/v1/models`
- **Claude**: `/v1/messages`、`/v1/messages/count_tokens`
- **Gemini**: `/v1beta/models/{model}:generateContent`、`:streamGenerateContent`、`/v1beta/models`

> **重要**: ベアパス `/v1/models` は OpenAI 形式を返します（1 つのパスで 2 つの形式は返せません）。Claude 形式のモデル一覧が必要な場合は `/claude/v1/models` を使用してください。内部では **Anthropic Messages を中枢の母形式**とし、他プロトコルは双方向変換のうえ同一の中継カーネルを再利用します。

## OpenAI 互換 API

OpenAI SDK と互換性のあるエンドポイントです。

### GET /openai/v1/models

利用可能なモデル一覧を取得します。

**リクエスト:**

```bash
curl http://localhost:8080/openai/v1/models \
  -H "Authorization: Bearer sk-あなたのキー"
```

**レスポンス:**

```json
{
  "object": "list",
  "data": [
    {"id": "claude-sonnet-4.5", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "claude-opus-4.6", "object": "model", "created": 1700000000, "owned_by": "kiro2api"},
    {"id": "gpt-5.6-sol", "object": "model", "created": 1700000000, "owned_by": "kiro2api"}
  ]
}
```

> 💡 **モデル選択ガイド**：利用可能なモデルは**アカウントのサブスクリプション階層に依存します**。
> - 無料階層（KIRO FREE）は通常 `claude-sonnet-4.5` のみが許可されます。
> - `opus` / `GPT` などのモデルはより上位の階層が必要です。
> - サポートされていないモデルをリクエストすると、静かに失敗するのではなく明確に `400`（`INVALID_MODEL_ID`）を返します。
>
> ⚠️ ただし各プロトコルの `/models` が返すのは**固定の短いリスト**です（OpenAI/Gemini は `claude-sonnet-4.5` / `claude-opus-4.6` / `gpt-5.6-sol`、Anthropic は `claude-sonnet-4.5` / `claude-opus-4.6` / `claude-haiku-4.5`）。アカウントプールもサブスクリプション階層も参照しない静的な値なので、**list-then-use は利用可否の保証にはなりません**——ここに載っているモデルでも階層が足りなければ `400`（`INVALID_MODEL_ID`）になります。より広いカタログは `GET /api/admin/models` を参照してください（各アカウントの上流モデル一覧の**和集合**、なければ静的な 17 件にフォールバック）。なお中継が受け付けるモデル名はこれらのリストの完全一致に限りません——モデル名は小文字化して部分一致で内部 id に写像されるため、どのリストにも載らない綴りが通ることもあります。

### POST /openai/v1/chat/completions

チャット補完リクエストを送信します。

**リクエスト:**

```bash
curl -X POST http://localhost:8080/openai/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {"role": "user", "content": "こんにちは"}
    ],
    "stream": false,
    "temperature": 0.7,
    "max_tokens": 2048
  }'
```

**リクエストパラメータ:**

| パラメータ | 型 | 必須 | 説明 |
|-----------|-----|------|------|
| `model` | string | ✅ | モデル名（例: `claude-sonnet-4.5`） |
| `messages` | array | ✅ | メッセージ配列。`content` は文字列またはオブジェクト配列（マルチモーダル対応） |
| `stream` | boolean | ❌ | ストリーミング有効（デフォルト: false） |
| `temperature` | number | ❌ | 受け付けますが**効果はありません**（下記参照） |
| `max_tokens` | integer | ❌ | 受け付けますが**効果はありません**（下記参照） |
| `top_p` | number | ❌ | 受け付けますが**効果はありません**（下記参照） |
| `tools` | array | ❌ | 関数定義配列（真透過） |
| `tool_choice` | string | ❌ | 受け付けますが**効果はありません**（下記参照） |

> **サンプリング系パラメータは上流へ渡りません**: バックエンドの Kiro データプレーンには `temperature` / `top_p` / `max_tokens` / `tool_choice` に相当するフィールドが存在しないため、これらは受理されても中継されません（SDK 互換のためエラーにはせず、黙って捨てます）。`temperature` / `top_p` はリクエスト構造体のフィールドですらなく、未知キーとして無視されます。出力長の上限は上流側の予算で決まり、そこに達した場合は `finish_reason: "length"`（Anthropic 形式なら `stop_reason: "max_tokens"`）で報告されます。

**マルチモーダル content 形式:**

`content` は文字列（テキストのみ）またはオブジェクト配列（テキストと画像対応）：

```json
{
  "role": "user",
  "content": [
    {"type": "text", "text": "これは何ですか"},
    {
      "type": "image_url",
      "image_url": {
        "url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
      }
    }
  ]
}
```

対応する content タイプ：
- `text`：プレーンテキストコンテンツ
- `image_url`：画像、Base64 Data URI（`data:image/...;base64,...`）

**メッセージ形式:**

```json
{
  "role": "user|assistant|system|tool",
  "content": "テキスト内容"
}
```

**レスポンス（非ストリーミング）:**

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
        "content": "こんにちは。何かお手伝いできることはありますか？"
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

ツール呼び出し時は `choices[0].message.tool_calls` を返し、`finish_reason` は `"tool_calls"` になります。

**レスポンス（ストリーミング）:**

先頭フレームは `delta.role`、末尾フレームは `finish_reason` を含み、`data: [DONE]` で終端します。

```
data: {"choices":[{"delta":{"content":"こんにちは"}}]}
data: {"choices":[{"delta":{"content":"。"}}]}
data: [DONE]
```

### POST /openai/v1/responses

OpenAI Responses API。Chat Completions ではなく新しい Responses プロトコルを必要とするクライアント（例: **Codex CLI**。2026 年 2 月に Chat Completions のサポートを終了したため、Codex CLI を kiro2api に接続するにはこのエンドポイントが必要）向けに提供されます。テキスト、ストリーミング、関数/ツール呼び出しに対応します。

**リクエスト:**

```bash
curl -X POST http://localhost:8080/openai/v1/responses \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{
    "model": "claude-sonnet-4.5",
    "input": "What is 2+2?",
    "stream": false
  }'
```

**リクエストパラメータ:**

| パラメータ | 型 | 必須 | 説明 |
|-----------|-----|------|------|
| `model` | string | ✅ | モデル ID（例: `claude-sonnet-4.5`） |
| `input` | string または array | ✅ | プレーンな文字列（単一のユーザーメッセージの省略形）、または入力アイテムの配列（下記参照） |
| `instructions` | string | ❌ | 会話の先頭に付加されるシステム/開発者プリアンブル（→ system に変換） |
| `stream` | boolean | ❌ | ストリーミングを有効化（デフォルト: false） |
| `tools` | array | ❌ | ツール呼び出し用の関数定義、**フラット形式**: `{"type":"function","name","description","parameters"}`（注: Chat Completions のネストされた `{"type":"function","function":{...}}` 形式とは異なります） |
| `tool_choice` | string または object | ❌ | 受理されますが**上流へは渡らず効果はありません**（`auto` / `none` / `required` / `{"type":"function","name":"..."}` のいずれを送ってもモデルの挙動は変わりません） |

**`input` 配列アイテムの種類:**

- `{"type":"message","role":"user"|"assistant"|"system","content":[...]}` — content のパーツ: `{"type":"input_text","text":...}`、`{"type":"input_image","image_url":"..."}`、`{"type":"output_text","text":...}`
- `{"type":"function_call","call_id","name","arguments"}` — 直前のアシスタントによるツール呼び出しターン（複数ターンの履歴として自身で再送する場合に使用）
- `{"type":"function_call_output","call_id","output"}` — 送り返すツールの実行結果（`type` はこの綴りのみ。`"tool_result"` など他の値を送ると `400` になります）

**サポートされていません（暗黙の無視ではなく明示的エラー）:** `previous_response_id` — 本サーバーはサーバー側で会話状態を保持しません。指定した場合、黙って無視するのではなく 400 の `invalid_request_error` を返します。毎回のリクエストで会話全体を `input` に含めて送信してください（Codex CLI は既にこの方式で動作しています）。

**レスポンス（非ストリーミング）:**

```json
{
  "id": "resp_xxx",
  "object": "response",
  "created_at": 1715970000,
  "status": "completed",
  "model": "claude-sonnet-4.5",
  "output": [
    {
      "type": "message",
      "id": "msg_xxx",
      "status": "completed",
      "role": "assistant",
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

返るフィールドは上記がすべてです。`usage` は 3 つのカウンタのみで、`input_tokens_details` / `output_tokens_details` は**ありません**。`output_text` パーツに `annotations` は付かず、レスポンス直下の `previous_response_id` / `instructions` / `error` も**返しません**（これらで分岐しないでください）。`max_tokens` 到達などで切り詰められた場合のみ `status` が `"incomplete"` になり、`"incomplete_details": {"reason": "max_output_tokens"}` が追加されます。

**レスポンス（ストリーミング）:** 仕様に準拠した名前付き SSE イベントの並びで、各イベントは単調増加する `sequence_number` を持ちます。`data: [DONE]` のような終端マーカーは**ありません**（これは Chat Completions の慣習です）— 完了は `response.completed`（または `response.failed`）によって通知されます。

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

ツール呼び出しの場合、`response.output_item.added`（type が `function_call`）の後には、上記のテキストイベントの代わりに `response.function_call_arguments.delta` / `response.function_call_arguments.done` / `response.output_item.done` が続きます。

**関数呼び出しの例:**

```bash
curl -X POST http://localhost:8080/openai/v1/responses \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
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
レスポンスの `output` には `function_call` アイテムが含まれます:
```json
{"id": "fc_xxx", "type": "function_call", "status": "completed", "call_id": "call_xxx", "name": "get_weather", "arguments": "{\"city\": \"Paris\"}"}
```

## Claude 互換 API

Anthropic Claude SDK と互換性のあるエンドポイントです（内部の中枢母形式でもあります）。

### GET /claude/v1/models

Anthropic 形式のモデル一覧を取得します（OpenAI の `/v1/models` との衝突を避けます）。他のプロトコルの `/models` と同じく**バイナリに焼き込まれた固定リスト**です（[GET /openai/v1/models](#get-openaiv1models) の注記を参照）。OpenAI/Gemini 側が `gpt-5.6-sol` で終わるのに対し、Claude 形式のリストは `claude-haiku-4.5` で終わる点に注意してください。

**リクエスト:**

```bash
curl http://localhost:8080/claude/v1/models \
  -H "Authorization: Bearer sk-あなたのキー"
```

**レスポンス:**

```json
{
  "data": [
    {"type": "model", "id": "claude-sonnet-4.5", "display_name": "Claude Sonnet 4.5", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "claude-opus-4.6", "display_name": "Claude Opus 4.6", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "claude-haiku-4.5", "display_name": "Claude Haiku 4.5", "created_at": "2026-01-01T00:00:00Z"}
  ],
  "has_more": false,
  "first_id": "claude-sonnet-4.5",
  "last_id": "claude-haiku-4.5"
}
```

### POST /claude/v1/messages

メッセージを送信します。標準ベアパス `/v1/messages` でもアクセスできます（Anthropic SDK は `base_url` に自動で `/v1/messages` を補完します）。

**リクエスト:**

```bash
curl -X POST http://localhost:8080/v1/messages \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{
    "model": "claude-sonnet-4.5",
    "max_tokens": 1024,
    "messages": [
      {"role": "user", "content": "こんにちは"}
    ]
  }'
```

**リクエストパラメータ:**

| パラメータ | 型 | 必須 | 説明 |
|-----------|-----|------|------|
| `model` | string | ✅ | モデル名 |
| `max_tokens` | integer | ❌ | Anthropic 規範では必須ですが、本サービスは省略しても受理します。**上流へは渡らず効果もありません**（[OpenAI 側の注記](#post-openaiv1chatcompletions)と同じ） |
| `messages` | array | ✅ | メッセージ配列。`content` は文字列またはブロック配列（`text`/`image`/`tool_use`/`tool_result`） |
| `system` | string または配列 | ❌ | システムプロンプト（文字列、または `{"type":"text","text":…}` ブロック配列） |
| `tools` | array | ❌ | ツール定義配列（真透過） |
| `tool_choice` | object/string | ❌ | 受け付けますが**効果はありません**（上流に対応フィールドなし） |
| `temperature` | number | ❌ | 受け付けますが**効果はありません**（フィールドとして解析されず無視） |
| `stream` | boolean | ❌ | ストリーミング有効 |

**レスポンス:**

```json
{
  "id": "msg-xxx",
  "type": "message",
  "role": "assistant",
  "content": [
    {
      "type": "text",
      "text": "こんにちは。何かお手伝いできることはありますか？"
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

ストリーミングは Anthropic 標準の SSE です（`message_start` → `content_block_start` → `content_block_delta` → … → `message_stop`）。ツールは `tool_use` ブロックと `input_json_delta` で処理されます。

### POST /claude/v1/messages/count_tokens

トークン数を推定します。標準ベアパス `/v1/messages/count_tokens` でもアクセスできます。

**リクエスト:**

```bash
curl -X POST http://localhost:8080/v1/messages/count_tokens \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {"role": "user", "content": "こんにちは"}
    ]
  }'
```

**レスポンス:**

```json
{
  "input_tokens": 10
}
```

## Gemini 原生 API

Google Gemini API と互換性のあるエンドポイントです。**すべて camelCase** です。

### GET /gemini/v1beta/models

モデル一覧を取得します。他のプロトコルの `/models` と同じく**バイナリに焼き込まれた固定リスト**です（[GET /openai/v1/models](#get-openaiv1models) の注記を参照）。各エントリが持つのは `name` と `supportedGenerationMethods` だけで、`displayName` は出力されず、`description` / `inputTokenLimit` / `outputTokenLimit` といったフィールドはありません。

**リクエスト:**

```bash
curl http://localhost:8080/gemini/v1beta/models \
  -H "Authorization: Bearer sk-あなたのキー"
```

**レスポンス:**

```json
{
  "models": [
    {"name": "models/claude-sonnet-4.5", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/claude-opus-4.6", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/gpt-5.6-sol", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]}
  ]
}
```

### POST /gemini/v1beta/models/{model}:generateContent

コンテンツを生成します。

**リクエスト:**

```bash
curl -X POST http://localhost:8080/gemini/v1beta/models/claude-sonnet-4.5:generateContent \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{
    "contents": [
      {
        "role": "user",
        "parts": [
          {"text": "こんにちは"}
        ]
      }
    ],
    "generationConfig": {
      "temperature": 0.7,
      "maxOutputTokens": 2048
    }
  }'
```

`contents[]`（`parts[]` は text / `inline_data`）、`system_instruction?`、`tools[].function_declarations` に対応します（いずれも camelCase / snake_case の両方の綴りを受け付けます）。`generationConfig` で解析されるのは `maxOutputTokens` だけで、しかもそれを含め**サンプリング系は上流へ渡りません**（`temperature` などは未知キーとして無視されます。[OpenAI 側の注記](#post-openaiv1chatcompletions)と同じ）。

**レスポンス:**

```json
{
  "candidates": [
    {
      "content": {
        "role": "model",
        "parts": [
          {
            "text": "こんにちは。何かお手伝いできることはありますか？"
          }
        ]
      },
      "finishReason": "STOP"
    }
  ],
  "usageMetadata": {
    "promptTokenCount": 10,
    "candidatesTokenCount": 20,
    "totalTokenCount": 30
  }
}
```

ツール呼び出し時は `parts[]` に `functionCall` が含まれます。

### POST /gemini/v1beta/models/{model}:streamGenerateContent

ストリーミングでコンテンツを生成します（`?alt=sse` 形式、camelCase、`[DONE]` なし）。

**リクエスト:**

```bash
curl -X POST "http://localhost:8080/gemini/v1beta/models/claude-sonnet-4.5:streamGenerateContent?alt=sse" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{
    "contents": [
      {
        "role": "user",
        "parts": [{"text": "詩を書いてください"}]
      }
    ]
  }'
```

**レスポンス（SSE）:**

```
data: {"candidates":[{"content":{"parts":[{"text":"春の"}]}}]}
data: {"candidates":[{"content":{"parts":[{"text":"夜"}]}}]}
```

> 認証情報はゲートが受け付けるいずれのチャネルで渡しても構いません。優先順位は `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > クエリ（`?api_key=` > `?token=` > `?key=`）です。Gemini ネイティブの `x-goog-api-key` ヘッダーと `?key=` パラメータも**サポートされている**ため、公式の `google-genai` SDK は `base_url` を差し替えるだけで動作します。変更が必要なのは**値**のほうです——常に**本サービスの** API Key を渡してください（Google / OpenAI 本家のベンダーキーではありません）。

## 管理 API

`/admin` 管理パネル（静的、rust-embed 埋め込み）は `/api/admin/*` API で駆動されます。以下のエンドポイントはすべて `adminApiKey`（未設定時は `apiKey` にフォールバック。両方とも未設定なら管理 API はオープンになります——この状態で外部に公開しないでください）で認証されます。認証の渡し方はプロトコルゲートと同じ 6 チャネル・同じ優先順位です（`Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > `?api_key=` > `?token=` > `?key=`。ヘッダーを設定できない SSE ログストリームはクエリ、通例 `?api_key=` を使います）。レスポンス本体は原則 camelCase ですが、**`GET /api/admin/config` と `GET /api/admin/models` はパネルのデータモデルに合わせて snake_case** です（旧 `/admin/api/stats` のサマリーも同様）。いずれのレスポンスも**アカウントの access/refresh トークンは一切含みません**（`GET /api/admin/credentials` は状態のみ）。

> [!WARNING]
> 管理 API のレスポンスは**秘密情報を含まないわけではありません**。`GET`/`POST /api/admin/api-keys` の `key` フィールドは**完全な平文**、`GET /api/admin/server-info` の `masterApiKey` も**完全な平文**です。マスキングされるのは `GET /api/admin/config/auth-keys` と `GET /api/admin/config` だけです。読み取り専用の管理者ロールは存在しないため、管理キーを持つ者はすべての key を閲覧・作成・ローテーションできます。管理 API のレスポンスは秘密情報として扱い、issue やログ、サードパーティのツールに貼り付けないでください。

### GET /api/admin/credentials

アカウントプールの状態を取得します（暗黙の「ログイン確認」面でもあり、200 が返れば key は有効とみなされます）。

**リクエスト:**

```bash
curl http://localhost:8080/api/admin/credentials \
  -H "Authorization: Bearer sk-あなたのキー"
```

**レスポンス:**

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
      "lastUsedAt": "2026-07-25T10:30:00Z",
      "healthStatus": "healthy",
      "throttleCount": 0
    }
  ]
}
```

> **注意**: プールはリクエストごとにアカウントを選ぶため、「現在のアカウント」という永続的な状態は存在しません。`currentId` は常に `-1`、各行の `isCurrent` は常に `false` を返します（どちらも将来のスティッキー選択モード用の予約フィールドです）。この 2 つで分岐しないでください。

### POST /api/admin/credentials

新しいアカウント認証情報をプールに追加して永続化します。

必須は `refreshToken` だけです（`authMethod` が `idc` の場合は `clientId` + `clientSecret` も必要）。access token と有効期限は**このエンドポイントでは受け付けません**——初回の自動リフレッシュ時に補完されます。未知のキーは拒否されず、黙って無視されます。

**リクエスト:**

```bash
curl -X POST http://localhost:8080/api/admin/credentials \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{
    "refreshToken": "...",
    "authMethod": "social",
    "profileArn": "arn:aws:codewhisperer:us-east-1:...:profile/..."
  }'
```

**レスポンス:**

```json
{
  "success": true,
  "message": "credential added",
  "credentialId": 12345,
  "email": "a@example.com",
  "duplicate": false
}
```

`refreshToken` がすでにプールにある場合は新規追加されず、`message` が `"credential already exists"`、`duplicate` が `true`、`credentialId` は**既存アカウントの id** になります（`email` は不明なら省略されます）。

### PUT /api/admin/credentials/{id}

既存のアカウント認証情報を更新します。

### DELETE /api/admin/credentials/{id}

プールからアカウント認証情報を削除します。

**リクエスト:**

```bash
curl -X DELETE http://localhost:8080/api/admin/credentials/12345 \
  -H "Authorization: Bearer sk-あなたのキー"
```

### POST /api/admin/credentials/{id}/disabled

アカウントの有効/無効を切り替えます。

**リクエスト:**

```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/disabled \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{"disabled": true}'
```

**レスポンス:**

```json
{
  "success": true,
  "message": "credential disabled"
}
```

### POST /api/admin/credentials/{id}/priority

アカウントの優先度を設定します（`balanced` 負荷分散で使用）。優先度は**プール内の重みそのもの**で、大きいほど多くのトラフィックが割り当てられます。1 未満の値は 1 に丸められます。

ボディは `{"priority": <整数>}` のみで、`priority` は必須です（省略すると `422`）。このエンドポイントに独立した `weight` フィールドは**なく**、余分なキーは黙って無視されます。重みを明示的に設定したい場合は `PUT /api/admin/credentials/{id}` に `{"weight": N}` を渡してください。

**リクエスト:**

```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/priority \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{"priority": 2}'
```

### POST /api/admin/credentials/{id}/reset

アカウントの失敗カウント/クールダウンをクリアします。

### POST /api/admin/credentials/batch-import

認証情報を一括インポートします。ボディは必ず `data` キーで包む必要があり、裸の配列を POST すると `422` になります。`data` の中身は配列、KAM 形式の `{accounts: [...]}` オブジェクト、単一オブジェクトのいずれでも構いません。各行を個別に正規化/検証/永続化し、行ごとの結果と件数を返します。各行で有効なのは `refreshToken`（必須。空なら当該行は失敗）、`clientId`/`clientSecret`（`idc` 用。片方だけだとその行は失敗）、`email`（無ければ `nickname` で代替）、`nickname`、`machineId`、`priority`（アカウントの `weight` として保存、1 未満は 1 に丸め）、`region`/`authRegion`/`apiRegion`（`apiRegion` > `authRegion`/`region` の順に採用、いずれも無ければ `us-east-1`）です。KAM 形式の `credentials: {…}` 入れ子があれば、その中の `refreshToken`/`clientId`/`clientSecret`/`region` が最優先されます。`accessToken` / `expiresAt` / `profileArn` は正規化時に無視されます。`authMethod` も読まれません——auth は `clientId` と `clientSecret` が揃っていれば `idc`、そうでなければ `social` と自動判定されます。

**リクエスト:**

```bash
curl -X POST http://localhost:8080/api/admin/credentials/batch-import \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-管理端のキー" \
  -d '{
    "data": [
      {"refreshToken": "...", "email": "a@example.com", "priority": 2}
    ]
  }'
```

### 対話型ログイン / インポート

`credentials.json` を手で編集せずに新しい Kiro アカウントを取り込みます。

**AWS Builder ID（デバイスコードフロー）:**

```bash
# 1. 開始してデバイスコードを取得（body は JSON 必須。region は任意、既定 us-east-1）
curl -X POST http://localhost:8080/api/admin/login/builderid/start \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{}'

# 2. ユーザーが認可を完了するまでポーリング（start が返した sessionId が必須）
curl -X POST http://localhost:8080/api/admin/login/builderid/poll \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{"sessionId": "..."}'
```

start は `{sessionId,userCode,verificationUri,interval}` を返します。poll は `{success,completed,status,interval?,credentialId?,email?}` を返し、成功時に自動保存します。どちらも `Json` 抽出器を使うため、`Content-Type: application/json` と JSON body なしで送ると本文を読む前に弾かれます（poll は `sessionId` 必須、欠けると `422`）。

**IAM Identity Center（SSO フロー）:**

```bash
# 1. 開始して認可 URL を取得（startUrl は必須。空文字なら 400、キーごと欠けると 422）
curl -X POST http://localhost:8080/api/admin/login/iam-sso/start \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{"startUrl": "https://あなたのポータル.awsapps.com/start", "region": "us-east-1"}'
# → {"sessionId":"...","authorizeUrl":"..."}

# 2. コールバック URL を渡して完了（state を検証して保存）
curl -X POST http://localhost:8080/api/admin/login/iam-sso/complete \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{"sessionId": "...", "callbackUrl": "..."}'
```

**生の bearer / SSO トークンの一括インポート（1 行 1 件）:**

```bash
curl -X POST http://localhost:8080/api/admin/login/sso-token \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{"bearerToken": "token1\ntoken2", "region": "us-east-1"}'
```

`bearerToken` は改行区切りのテキスト**全体**を 1 つの文字列として渡すフィールドで、サーバー側が 1 行ずつ分割して処理します（上限 200 行）。必須のため、省略すると `422` になります。`region` は任意（既定は `us-east-1`）で、この 2 つ以外のキーは無視されます。

`{added,failed:[{lineIndex,error}]}` を返します。

### API キー管理

呼び出し側に渡す対外 key を管理します。一覧 / 作成のレスポンスは `key` フィールドを**完全な平文**で返します（パネルは表示時にクライアント側でマスクしますが、コピーボタンには実値が必要なため）。

| メソッド | エンドポイント | 機能 |
|---------|--------------|------|
| GET | `/api/admin/api-keys` | 一覧（`key` は完全な平文） |
| POST | `/api/admin/api-keys` | 作成（`key` は完全な平文） |
| PUT | `/api/admin/api-keys/{id}` | 更新 |
| DELETE | `/api/admin/api-keys/{id}` | 削除 |
| GET | `/api/admin/api-keys/usage` | 全 key の使用量 |
| GET | `/api/admin/api-keys/{id}/usage` | 単一 key の使用量 |
| DELETE | `/api/admin/api-keys/{id}/usage` | 単一 key の使用量をリセット |
| GET | `/api/admin/api-keys/{id}/usage/records` | ページ分割された使用量記録（`?page=&page_size=`） |

### 使用量・統計

| メソッド | エンドポイント | 機能 |
|---------|--------------|------|
| GET | `/api/admin/credentials/{id}/usage/records` | アカウント別のページ分割使用量記録 |
| GET | `/api/admin/credentials/{id}/usage/today` | アカウントの当日サマリー |
| GET | `/api/admin/credentials/{id}/failure-logs` | 直近の失敗イベント |
| GET | `/api/admin/credentials/{id}/throttle-logs` | 直近のスロットルイベント |
| GET | `/api/admin/credentials/{id}/balance` | アカウント残高（5 分キャッシュ） |
| GET | `/api/admin/usage/daily` | 日次使用量サマリー |
| GET | `/api/admin/usage/daily/{date}/records` | 指定日の記録 |
| GET | `/api/admin/rpm` | リアルタイム RPM スナップショット |

### GET /api/admin/config

マスキングされた設定ビューを取得します（ブール/非機密フィールドのみ）。

**リクエスト:**

```bash
curl http://localhost:8080/api/admin/config \
  -H "Authorization: Bearer sk-あなたのキー"
```

**レスポンス**（このエンドポイントのフィールド名は snake_case です）:

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

`display_name` / `type` / `max_tokens` を含むモデル一覧を取得します（フィールド名はパネルに合わせて snake_case）。各アカウントの上流モデル一覧の**和集合**（キャッシュ）を返し、キャッシュが空なら静的な 17 モデルのカタログにフォールバックします。プロトコル側の `/v1/models` などが返す固定 3 件とは**別物で、こちらのほうが広い集合**です。

### 負荷分散モードの読み取り / 切り替え

実行時に負荷分散モード（`priority` / `balanced`）を読み取り / 切り替えます。`config.json` に永続化されます（再起動不要）。

**リクエスト:**

```bash
# 現在のモードを読み取り
curl http://localhost:8080/api/admin/config/load-balancing \
  -H "Authorization: Bearer sk-あなたのキー"

# モードを切り替え
curl -X PUT http://localhost:8080/api/admin/config/load-balancing \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{"mode": "balanced"}'
```

### 認証キーの読み取り / ローテーション

`apiKey` と `adminApiKey` を実行時に読み取り（マスキング） / ローテーションします。即時反映（再起動不要）。

```bash
curl http://localhost:8080/api/admin/config/auth-keys \
  -H "Authorization: Bearer sk-あなたのキー"

curl -X PUT http://localhost:8080/api/admin/config/auth-keys \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{"apiKey": "sk-新しいキー"}'
```

### GET /api/admin/server-info

サーバー情報を取得します。

**レスポンス:**

```json
{
  "masterApiKey": "sk-マスターキーの平文",
  "version": "0.3.1",
  "kiroVersion": "0.11.107",
  "rustVersion": "1.90.0",
  "runMode": "Docker",
  "uptimeSecs": 3600
}
```

`masterApiKey` は設定済み `apiKey` の**完全な平文**（未設定なら `null`）で、ここでは**マスキングされません**——パネルがブラウザ側でマスクして表示し、コピーボタンは実値を使います。マスキング済みの値が必要なら `GET /api/admin/config/auth-keys` を使ってください。`version` は kiro2api のバージョン、`kiroVersion` は偽装した上流 UA のバージョン、`rustVersion` はビルド時の rustc バージョンです。ほかに実行時メトリクス（`serverTime`、`serverTimeUnix`、`os`、`memoryUsedBytes`、`memoryTotalBytes`、`cpuPercent`、`runMode`、`pid`、`uptimeSecs`）も含まれます。

### リアルタイムログ

`logCapacity > 0` が必要です（そうでなければ `503`）。

| メソッド | エンドポイント | 機能 |
|---------|--------------|------|
| GET | `/api/admin/logs/stream` | SSE ストリーム（最初に history イベント、続いて行ごとの log イベントとハートビート）。EventSource はヘッダーを設定できないため `?api_key=<admin key>` で認証 |
| GET | `/api/admin/logs/snapshot` | 現在のバッファを JSON 配列で |
| GET | `/api/admin/logs/download` | バッファを `.txt` 添付として |

**リクエスト:**

```bash
curl "http://localhost:8080/api/admin/logs/stream?api_key=sk-あなたのキー"
```

### 旧管理エンドポイント（後方互換のため保持）

| メソッド | エンドポイント | 機能 |
|---------|--------------|------|
| GET | `/admin/api/stats` | `{accounts:[…], summary:{total,active,disabled,in_cooldown}}` |
| GET | `/admin/api/config` | マスキングされた設定 |
| POST | `/admin/api/accounts/{id}/enable` | 手動での有効化（メモリ上のみ、再起動でファイルの値にリセット） |
| POST | `/admin/api/accounts/{id}/disable` | 手動での無効化（メモリ上のみ、再起動でファイルの値にリセット） |

## ユーザー API

`/user` ユーザーパネル（静的、rust-embed 埋め込み）は `/api/user/*` で駆動されます。これらのエンドポイントは admin ゲートを**通りません**——各リクエストは呼び出し側**自身の API-KEY** で認証され、handler が検証後にデータをその key に限定します。key の取り出しはプロトコル側と同じヘッダー優先順（`Authorization: Bearer` > `x-api-key` > `x-goog-api-key`）で、`/api/user/*` ではクエリパラメータは受け付けません。`POST /api/user/login` だけは body の `{apiKey}` が最優先で、空/未指定なら上記ヘッダーに回ります。key が無効なら `401`、本体は `{"error":"…"}`。レスポンスは camelCase、`credits = cost / 0.72`。

### POST /api/user/login

key を検証します。

**リクエスト:**

```bash
curl -X POST http://localhost:8080/api/user/login \
  -H "Content-Type: application/json" \
  -d '{"apiKey": "sk-あなたのキー"}'
```

**レスポンス:**

```json
{
  "id": 7,
  "name": "マイキー",
  "spendingLimit": 100.0,
  "limitUnit": "usd",
  "totalCost": 12.5,
  "totalCredits": 17.36,
  "expiresAt": "2026-12-31T00:00:00Z",
  "durationDays": 30,
  "activatedAt": "2026-07-25T10:30:00Z"
}
```

### GET /api/user/usage

その key の使用量サマリー（`byModel[]` を含む）を取得します。

**リクエスト:**

```bash
curl http://localhost:8080/api/user/usage \
  -H "x-api-key: sk-あなたのキー"
```

### GET /api/user/usage/records

その key の使用量記録をページ分割で取得します（`?page=&page_size=`、新しい順）。

**リクエスト:**

```bash
curl "http://localhost:8080/api/user/usage/records?page=1&page_size=20" \
  -H "x-api-key: sk-あなたのキー"
```

## 運用

### GET /health

ヘルスチェック（認証不要）。

**リクエスト:**

```bash
curl http://localhost:8080/health
```

**レスポンス:**

```json
{
  "service": "kiro2api",
  "status": "ok",
  "version": "0.3.1"
}
```

### GET /v1/ping

疎通確認（認証不要）。

**リクエスト:**

```bash
curl http://localhost:8080/v1/ping
```

**レスポンス:**

```json
{
  "pong": true
}
```

## エラーコード

API エラーは以下のコードで返されます。

| コード | 説明 | 対応 |
|--------|------|------|
| 400 | パラメータエラー / マッピングされていないモデル（`INVALID_MODEL_ID`） | リクエストパラメータとモデル名を確認 |
| 401 | 未認証（key がない、誤った key、無効化/期限切れのストア key） | API Key を確認 |
| 402 | ストア管理の API-KEY が消費上限に到達（本体は `{"type":"error","error":{"type":"billing_error","message":"…"}}`）。判定には在途分の予約（USD 単位で `1.0`、`credits` 単位で約 `1.39`）が含まれるため、**残りが 1 回分の見積を下回った時点で**上限を使い切る前に拒否が始まります | key の上限を引き上げるか、使用量をリセット |
| 403 | 禁止 | 権限がない |
| 404 | 見つからない（パスが存在しない。または管理エンドポイントにプールへ存在しないアカウント / API-KEY / ログインセッションの id を渡した） | id とパスを確認 |
| 422 | リクエストボディのデシリアライズ失敗（必須フィールドの欠落や型不一致）。4 プロトコルの対話エンドポイントと `/v1/messages/count_tokens` は自前で拒否を受け取り、それぞれの形状の `400` に変換するため、`422` は主に `/api/admin/*` と `/api/user/login` のように `Json` 抽出器を直接使うエンドポイントで発生します | ボディの形状を確認 |
| 429 | 上流 Kiro のスロットリング（`ThrottlingException` 系の例外を変換）。`MAX_RPM_PER_CREDENTIAL` の超過はこのコードにはならず、他アカウントへローテーションされ、全滅した場合のみ `503` になります | しばらく待機 |
| 502 | 上流エラー | 上流の Kiro が失敗 |
| 503 | 利用不可 | 利用可能なアカウントがない（全てクールダウン中/無効化/RPM 超過）、またはログ機能無効 |

**エラーレスポンス例:**

エラーボディはプロトコルによって異なります。ただし認証ゲートが返す `401` / `402` は、どのプロトコルのパスでも Anthropic 形式（`{"type":"error","error":{"type":"authentication_error"|"billing_error","message":"…"}}`）で返ります。

```json
// Anthropic 形式
{"type": "error", "error": {"type": "invalid_request_error", "message": "..."}}

// OpenAI / Responses 形式
{"error": {"message": "upstream request failed", "type": "api_error", "code": null}}
// ↑ 中枢エラーの `code` は常に null。上流の例外を変換した場合だけ `code` に数値の状態コードが入ります。

// Gemini 形式
{"error": {"code": 400, "message": "...", "status": "INVALID_ARGUMENT"}}
```

## レート制限

`MAX_RPM_PER_CREDENTIAL` を設定すると、アカウントごとに 1 分あたりのリクエスト上限が適用されます（`0` = 無制限）。上限に達したアカウントは分級クールダウンに入り、リクエストは他のアカウントへ自動的にローテーションされます。すべてのアカウントが利用不可の場合は `503` を返します。

## タイムアウト

出站 HTTP クライアントにはタイムアウトのハードキャップが設定されています。長時間の処理にはストリーミング（SSE）を使用してください。`stream:false` の場合でも、サービス内部では AWS eventstream を解码し、収集完了後に完全な JSON を一度に返します。

---

> 📖 関連ドキュメント：[README](README.md) · [USAGE](USAGE.md) · [DEPLOY](DEPLOY.md) · [ルート README](../../README.md)

<div align="center">
  <sub>Built with Rust + axum + tokio | Powered by Kiro (CodeWhisperer)</sub>
</div>
