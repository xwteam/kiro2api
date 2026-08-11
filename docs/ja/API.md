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

3 プロトコル共通のカタログ（17 件）を定義順にそのまま返します。`object` は常に `"model"`、`created` は定数 `1700000000`、`owned_by` は定数 `"kiro2api"` です（いずれもハードコードで、時計も上流も読みません）。

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

> 💡 **モデル選択ガイド**：利用可能なモデルは**アカウントのサブスクリプション階層に依存します**。
> - 無料階層（KIRO FREE）は通常 `claude-sonnet-4.5` のみが許可されます。
> - `opus` / `GPT` などのモデルはより上位の階層が必要です。
> - サポートされていないモデルをリクエストすると、静かに失敗するのではなく明確に `400`（`INVALID_MODEL_ID`）を返します。
>
> ⚠️ 各プロトコルの `/models` が返すのは**バイナリに焼き込まれた共通カタログ**（17 件）です。`GET /v1/models`（= `/openai/v1/models`）・`GET /claude/v1/models`・`GET /v1beta/models`（= `/gemini/v1beta/models`）の 3 つは**同じ id を同じ並び順**で返し、違うのは各プロトコルの形式だけです。アカウントプールもサブスクリプション階層も参照せず、時計も上流も読みません。
>
> このカタログの id は**すべて中継側が解釈できる**ため、「一覧を引いて、返ってきた id をそのまま指定する」という標準的な流儀が成立します。ただし**利用可否の保証にはなりません**——名前の解決に成功しても、階層が足りないモデルは上流が拒否するため `400`（`INVALID_MODEL_ID`）になり得ます。`400` には別物が 2 つある点に注意してください：**中継が名前を解決できない**場合（メッセージはソース上の文字列そのままで `无法识别的模型名: <name>`）と、**名前は解決できたが当該アカウントの階層で提供されない**場合（上流の `INVALID_MODEL_ID`）です。カタログの id が返すのは常に後者だけです。
>
> 逆に、中継が受け付けるモデル名はカタログの完全一致に限りません——モデル名は小文字化して部分一致で内部 id に写像されるため、カタログに載らない綴り（`claude-3-5-sonnet-…` など）や、どの `/models` にも現れないルーティング別名 `auto` も通ります。**各アカウントの階層で実際に使える集合**が見たい場合は `GET /api/admin/models` を参照してください（上流モデル一覧の**和集合**。キャッシュが空のときはこの同じカタログにフォールバックします）。

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

### 履歴からのツール仕様の補完

上流は、メッセージに `toolUse` / `toolResult` コンテンツブロックが含まれる場合、`toolConfig` の存在を必須とします。無ければリクエスト全体が `TOOL_CONFIG_MISSING` で拒否されます。

一方でツールは、データプレーンに到達する前に正当に破棄されることがあります。Responses の組み込みツール(`web_search` / `local_shell` / `file_search`)は OpenAI 側のサービスが実行するもので、本中継の中枢には等価物が無いため変換時に破棄されます。あるターンでクライアントが**組み込みツールだけ**を送ると、`tools` は空配列になる一方、会話履歴のツール呼び出しは残ります。

その結果、リクエストは「ツール呼び出しはあるがツール定義が無い」形になります。これは**本サービス自身が作り出した**不正なリクエストであり、呼び出し側の誤りではありません。

**現在の挙動:** 上流へ送る前に、会話履歴に現れるすべてのツール名を収集し、現在の `tools` に宣言されていないものには最小限の仕様(空オブジェクトスキーマ)を補います。補うのはモデル自身が既に呼び出したツールであり、補完はリクエストを自己整合させるだけです。補わなければそのターンは丸ごと失敗します。クライアントが**宣言した**ツールが優先され、同名は上書きも重複もされません。

ツールが無く履歴にもツール呼び出しが無い場合、挙動は変わりません:`toolConfig` は送らず、タスク種別は `vibe` のままです。

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
- `{"type":"function_call_output","call_id","output"}` — 送り返すツールの実行結果（`type` はこの綴りのみ）

> ⚠️ 上記**以外**の `type`（`reasoning`、`local_shell_call` など Responses 側の生成物）は**その項目だけスキップ**され、リクエスト全体が拒否されることはありません。マルチターンではクライアントが直前の `output` をそのまま送り返すため、この種の項目は必ず含まれます。エラー扱いだと**一巡目は通り二巡目で必ず落ちる**状態でした（v0.7.1 で修正）。
> ⚠️ **`tools` 配列内の組み込みツールは破棄されます。** OpenAI 仕様では `tools` に `type:"function"` 以外に `web_search`、`local_shell`、`file_search` などの**組み込みツール**も含まれます。これらは OpenAI 側のサービスが実行するもので、**仕様上 `name` フィールドを持ちません**。本サーバーの中枢には等価物がなく代理実行もできないため、**解析したうえで破棄し WARN を記録**します（`responses_builtin_tool_dropped`、`tool_type` 付き）。リクエスト自体は失敗しません（v0.7.1 より前は `400 tools[N]: missing field name` となり、組み込みツール一つでターン全体が失敗していました）。**影響**: モデルはその組み込み機能（ウェブ検索など）を利用できません。`name` を持つ `function` / `custom` ツールは従来どおり有効で、`parameters` は省略可能（空オブジェクトスキーマとして扱われます）。


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

Anthropic 形式のモデル一覧を取得します（OpenAI の `/v1/models` との衝突を避けます）。中身は他の 2 プロトコルの `/models` と**同一の共通カタログ**（17 件・同じ並び順）で、違うのは形式だけです（[GET /openai/v1/models](#get-openaiv1models) の注記を参照）。`display_name` はカタログの表示名、`created_at` は全件に同じ定数 `2026-01-01T00:00:00Z` が入ります。`has_more` は常に `false`、`first_id` / `last_id` はこの一覧の先頭と末尾の id（それぞれ `claude-sonnet-4.5` と `gpt-5.6-sol`）です。

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
    {"type": "model", "id": "claude-sonnet-4.6", "display_name": "Claude Sonnet 4.6", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "claude-sonnet-5", "display_name": "Claude Sonnet 5", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "claude-opus-4.5", "display_name": "Claude Opus 4.5", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "claude-opus-4.6", "display_name": "Claude Opus 4.6", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "claude-opus-4.7", "display_name": "Claude Opus 4.7", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "claude-opus-4.8", "display_name": "Claude Opus 4.8", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "claude-haiku-4.5", "display_name": "Claude Haiku 4.5", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "claude-fable-5", "display_name": "Claude Fable 5", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "deepseek-3.2", "display_name": "DeepSeek 3.2", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "glm-5", "display_name": "GLM-5", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "qwen3-coder-next", "display_name": "Qwen3 Coder Next", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "minimax-m2.1", "display_name": "MiniMax M2.1", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "minimax-m2.5", "display_name": "MiniMax M2.5", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "gpt-5.6-terra", "display_name": "GPT-5.6 Terra", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "gpt-5.6-luna", "display_name": "GPT-5.6 Luna", "created_at": "2026-01-01T00:00:00Z"},
    {"type": "model", "id": "gpt-5.6-sol", "display_name": "GPT-5.6 Sol", "created_at": "2026-01-01T00:00:00Z"}
  ],
  "has_more": false,
  "first_id": "claude-sonnet-4.5",
  "last_id": "gpt-5.6-sol"
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

モデル一覧を取得します。中身は他の 2 プロトコルの `/models` と**同一の共通カタログ**（17 件・同じ並び順）で、違うのは形式だけです（[GET /openai/v1/models](#get-openaiv1models) の注記を参照）。各エントリが持つのは `name`（`models/` を前置した id）と `supportedGenerationMethods`（全件で同じ 2 要素の定数）だけで、`displayName` は出力されず（カタログに表示名はありますが Gemini 形式では常に省略されます）、`description` / `inputTokenLimit` / `outputTokenLimit` といったフィールドもありません。

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
    {"name": "models/claude-sonnet-4.6", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/claude-sonnet-5", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/claude-opus-4.5", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/claude-opus-4.6", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/claude-opus-4.7", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/claude-opus-4.8", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/claude-haiku-4.5", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/claude-fable-5", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/deepseek-3.2", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/glm-5", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/qwen3-coder-next", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/minimax-m2.1", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/minimax-m2.5", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/gpt-5.6-terra", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
    {"name": "models/gpt-5.6-luna", "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]},
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
      "statusReason": "none",
      "throttleCount": 0
    }
  ]
}
```

> **`failureCount` は累計失敗数、`throttleCount` はスロットルイベント数です。** 以前は入れ違っており、利用停止アカウントが「スロットル 1、失敗 0」と表示されていました。

`statusReason` は**直近の失敗理由**を示します(`none` / `banned` = 上流による利用停止 / `quota` / `token_expired` / `throttled` / `refresh_denied`)。

> **`banned` はアカウントを実際にプールから外します**。他の理由は表示にのみ影響します。クールダウンはタイマーであり時間が経てば自動的に戻りますが、利用停止は上流が下した判断(原文は「アカウントをロックしました。本人確認のためサポートへご連絡ください」)であり、待っても解除されません。タイマーだけで復帰させると、クールダウンが明けた瞬間に再び選ばれ、再び失敗し、再びクールダウン——実リクエストを消費し続ける一方で `available` はそれを利用可能として数えます。パネルは「利用停止」と表示し、カウントは問題なしと言う、互いに矛盾する二つの数字です。したがって利用停止アカウントは選択されず、`available` にも数えられず、`healthStatus` は `unhealthy` を返します。自然回復はしません(ラベルを消すはずの成功が永遠に訪れないため)。**唯一の復帰手段はパネルの「リセット」**です(`POST /api/admin/credentials/{id}/reset`、この判断も併せてクリアされます)。他の理由は従来どおり次回の成功時にクリアされます。この判断は `credentials.json`(`statusReason` キー)に永続化され、起動時に復元されます。メモリ上だけに置くと、デプロイのたびに消えてアカウントが黙ってプールへ戻ってしまうためです。**strike 数とクールダウン期限は引き続き永続化しません**。あれらはタイマーであり、ゼロから始まってもアカウントを少し早く再試行するだけです。判断は違います——プールに入れるかどうかを決めるものだからです。



> **v0.17.0:3 つのプロトコルの契約変更**
>
> - **OpenAI / Gemini / Responses で extended thinking が有効化できるようになりました**
>   (従来はハブ要求側で強制的に無効化されており、何を送っても効きませんでした)。
>   有効化は各プロトコル本来の書き方に従います:OpenAI の `reasoning_effort`、
>   Gemini の `thinkingConfig`、Responses の `reasoning`。
> - **応答の新フィールド**:OpenAI は `choices[].delta.reasoning_content`(`content` と並ぶ独立
>   フィールド。知らないクライアントは無視するだけ)、Gemini は `thought: true` の part、
>   Responses は `reasoning` 出力項目と `response.reasoning_summary_text.delta`。
>   **有効化だけして除去しないのは有害です** —— 思考が本文に混入するため、両者は必ず一組で。
> - **この 3 プロトコルにセッション識別子が付きました**。多輪の対話が上流から見て毎回
>   新規セッションに見えることはなくなります。
> - **Gemini ストリームに SSE キープアライブ**:上流の初回バイトが遅いときに中間のリバース
>   プロキシへ切断されなくなります。
> - **CORS 層を追加**:ブラウザ上のクロスオリジンクライアントから各プロトコル端点を直接
>   呼べます。
> - **`KIRO_API_KEY` 環境変数を追加**:Kiro API キー 1 本だけでサービスを起動できます
>   (マウントボリュームに認証情報ファイルは不要。起動時にアカウントプールへ取り込み永続化、
>   同じキーは二重に取り込みません)。
>
> **v0.17.1**:上記 3 つのストリーム出口が**応答の末尾を飲み込む**ことがありました
> (末尾が `<` の場合。コード中の `<div` など)。修正済み。ネイティブ Anthropic 出口は無影響。

> **v0.16.0:管理側の 2 つの挙動変更**
> - **優先度の変更が即座に反映されます。** 従来は固定選択(スティッキー)下で再選択が起きず、
>   現在のアカウントが使えなくなるまで変更が効きませんでした。
> - **全アカウントが停止扱いになってもプールが自己回復します。** 上流の一時的な不調で全部が
>   停止扱いになっても、誰かが再起動するまで完全に使用不能、という状態にはなりません。
>   自己回復は恒久的に無効化・BAN・枠の復帰時刻前のアカウントを復活させることはありません。
> **v0.15.0:2 つの挙動変更**
> - **枠切れを復帰時刻つきで永続化**(認証情報に `quotaResetUnix` を追加)。従来はメモリ上
>   だけで、再起動のたびに忘れ、利用者のリクエストで再発見していました(最初の数回が失敗)。
>   現在は再起動後も記憶し、**時刻が来れば自動でプールへ復帰**します。復帰時刻は残高 API の
>   `nextResetAt` を優先。管理画面の「リセット」でこの印は消えます。
> - **サーバー側組み込み検索 `web_search` が実際に動作します**。このツールだけを宣言した場合、
>   本サービスがリクエストを横取りして上流 MCP 端点を呼び、`server_tool_use` と
>   `web_search_tool_result` の 2 つのコンテンツブロックを返します。他のツールと混在する場合は
>   横取りしません(どれを呼ぶかはモデルが決めるため)。検索失敗時は 5xx ではなく空結果を返します。
> **v0.14.0**:`tlsBackend` を設定で切り替えられます(`native-tls` 既定 / `rustls`、再起動で反映)。
> 自己署名 CA のプロキシ配下ではどちらか一方しかハンドシェイクできないことが多いためです。
> 認識できない値は警告のうえ既定へフォールバックし、起動を妨げません。
> **v0.13.0**:`thinking` が実際に機能するようになりました(上流が解する指示へ変換し、応答では
> 独立した `thinking` ブロックとして返します。ストリーミングは `thinking_delta`。他の 3 プロトコル
> では思考内容を本文へ統合し、破棄しません)。token 推定は文字種別で重み付けするようになり
> (従来は中国語を約 3 倍過小評価)、ストリーミングの入力トークンも従来の **0 固定**から推定値へ。
> モデル一覧に **`context_window`** を追加し、`max_tokens` と分離しました。
> **v0.12.0:選択の優先度と、異なる階層の混在** —— `priority`(小さいほど優先)が**実際に
> 選択へ影響するようになりました**(従来は `weight` の別名で無効)。**インポートしたアカウント
> は一律 `999`**(最低)、必要なら手動で設定します。階層が混在する場合、`/v1/models` は全
> アカウントの**和集合**を返します。本サービスは非対応のアカウントを自動的に読み飛ばし、
> **どのアカウントでも提供されない**場合にのみ `400` を返します。
> **v0.11.0 でのツール契約の変更:** `tools[].type` を受け付けます(サーバー側組み込みツールは
> **v0.11.1:ツールの `description` は送信時に必ず非空になります。** 上流は空の説明に対し `400 Invalid tool use format / REQUEST_BODY_INVALID` を返し、**リクエスト全体**を拒否します。説明が無い場合はツール名で補います。この reason は**決定的**な誤りとして分類され、そのまま `400` を返します。
> `input_schema` を持たず、同フィールドが必須だったため整リクエストが 400 になっていました)。
> `input_schema` は上流が確実に受け取れる形へ**正規化**します(形のみ、意味は変更しません)。
> `name` が **63** 文字を超える場合は短縮して送り、レスポンスでは宣言どおりの名前へ復元します。
> `description` は常に文字列です。ほかに `POST /api/admin/credentials/{id}/refresh` を追加。
> **プロキシ関連フィールドは v0.10.1 から実際に機能します。** 以前は `proxyUrl` /
> `proxyUsername` / `proxyPassword` を API が受け取っても**保存されず**、`hasProxy` は常に
> `false` でした。現在の優先度は **資格情報 > グローバル > 直結**。資格情報側に `"direct"`
> を指定するとそのアカウントは明示的に直結します。`http://` / `https://` / `socks5://` に対応。
> 同一アカウントの**データプレーン、トークン更新、残量照会、モデル一覧、バックグラウンド更新は
> すべて同じ出口**を使います。
> **v0.10.0 以降、使用不可と判定されたアカウントは自動的にはプールへ戻りません。** 以前は
> `banned` のみがそうでした:枠の使い切り(402)は 30 分、明確な失効シグナルのない 401/403 が
> 2 回続いた場合は 5 分のクールダウンを経て、いずれもローテーションへ復帰していました。
> その結果、**すでに上流で停止された**アカウントが 5 分ごとに再び使われ続け、枠を使い切った
> アカウントは 1 日 48 回、必ず失敗する壁に当たっていました。現在この 2 種類は `banned` と
> 同様に使用を停止します。`banned` との違いは**永続性**です:こちらはメモリ上のみで、再起動
> または「リセット」で復活します(上流の一時的な権限の揺らぎを恒久的な損失にしないため)。
> `banned` は `statusReason` としてディスクに保存されます。
> 429 はこの対象外です — 一時的なスロットリングとして再分類されました(バックオフして再試行、
> アカウントへのペナルティなし)。

> **注意**: プールはリクエストごとにアカウントを選ぶため、「現在のアカウント」という永続的な状態は存在しません。`currentId` は常に `-1`、各行の `isCurrent` は常に `false` を返します（どちらも将来のスティッキー選択モード用の予約フィールドです）。この 2 つで分岐しないでください。

### POST /api/admin/credentials

新しいアカウント認証情報をプールに追加して永続化します。

必須は `refreshToken` だけです（`authMethod` が `idc` の場合は `clientId` + `clientSecret` も必要）。access token と有効期限は**このエンドポイントでは受け付けません**——初回の自動リフレッシュ時に補完されます。未知のキーは拒否されず、黙って無視されます。

**Kiro API Key(`ksk_…`)による登録**はもう一つの経路です:`refreshToken` の**代わりに** `kiroApiKey`(別名 `ksk`)を送ります。この種の資格情報では**キー自体がデータプレーンの bearer** であり、交換も更新も期限もなく OAuth 経路を一切通らないため、`refreshToken`・`clientId`・`clientSecret`・`expiresAt` はいずれも不要です。

```bash
curl -X POST http://localhost:8080/api/admin/credentials \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-管理キー" \
  -d '{"kiroApiKey": "ksk_xxx"}'
```

ディスク上の形(`credentials.json` を直接編集しても構いません):

```json
{"kiroApiKey": "ksk_xxx", "authMethod": "api_key"}
```

**`kiroApiKey` があれば `authMethod` の記載に関わらず API Key として扱います** —— `idc` を宣言しつつキーを持つ場合、「clientId と clientSecret は必須」の検証に落ち、本来完全な資格情報が不備と判定されてしまうためです。

逆に `authMethod: api_key` を宣言しながら `kiroApiKey` が無い資格情報は自己矛盾です:提示できる bearer が無いのに API Key 資格情報と判定されるため更新もされず、アカウントを跨ぐ再試行の中で毎回同じ箇所で失敗し続けます。この種の資格情報は**読み込み時に無効化**され、**「リセット」でも復活しません** —— リセットは strike・クールダウン・判定を消すだけで設定自体は変わらず、復活しても同じ失敗に戻るだけです。設定を修正して再起動してください。


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
| GET | `/api/admin/credits/global` | 全アカウントの残り積分合計（キャッシュのみ） |
| GET | `/api/admin/usage/daily` | 日次使用量サマリー |
| GET | `/api/admin/usage/daily/{date}/records` | 指定日の記録 |
| GET | `/api/admin/usage/summary` | 時間窓の使用量集計＋分桶系列＋稼働指標 |
| GET | `/api/admin/rpm` | リアルタイム RPM スナップショット |

> **ページングの共通規約**: ページ分割される管理エンドポイントはすべてクエリ `?page=&page_size=`（**snake_case**。`pageSize` ではありません）を取り、既定は `page=1` / `page_size=20` です。値はサーバー側で丸められます——`page_size` は最低 `1`、`page` は `[1, totalPages]` にクランプされ、空集合では `page=1` / `totalPages=0` になります。レスポンスは `{records, total, page, pageSize, totalPages}`（本体は camelCase）で共通です。`{id}` にプールに無いアカウントを渡しても**これらの読み取り系は `404` にはならず**、空ページ（`total: 0`）が `200` で返ります（数値でない id は `0` として扱われます）。

### GET /api/admin/credentials/{id}/usage/today

アカウントの**当日（CST / UTC+8）**使用量サマリーを取得します。クエリパラメータはありません。

**リクエスト:**

```bash
curl http://localhost:8080/api/admin/credentials/12345/usage/today \
  -H "Authorization: Bearer sk-管理端のキー"
```

**レスポンス:**

```json
{
  "date": "2026-07-26",
  "credentialId": 12345,
  "totalRequests": 42,
  "totalInputTokens": 1200,
  "totalOutputTokens": 3400,
  "totalCost": 0.85,
  "totalCredits": 1.18
}
```

`date` は CST の暦日キー（`YYYY-MM-DD`）で、日境界は UTC+8 の 0 時です。`credentialId` はパスの id を数値化したもので、**数値でない id は `0` になります**。未知のアカウントでも `404` にはならず、全項目 0 のサマリーが `200` で返ります。`totalCreditsSaved` フィールドは実装上の予約枠で、現状**常に省略されます**（Phase 1 の集計は値を生成しません）。

### GET /api/admin/credentials/{id}/throttle-logs

アカウントの直近のスロットル（上流 `429`）イベントをページ分割で取得します（新しい順）。`failure-logs`（`401`/`403`）と**同じ形状**です。

**リクエスト:**

```bash
curl "http://localhost:8080/api/admin/credentials/12345/throttle-logs?page=1&page_size=20" \
  -H "Authorization: Bearer sk-管理端のキー"
```

**レスポンス:**

```json
{
  "records": [
    {
      "credentialId": 12345,
      "requestType": "api",
      "statusCode": 429,
      "responseBody": "ThrottlingException: ...",
      "createdAt": "2026-07-26T10:30:00Z"
    }
  ],
  "total": 1,
  "page": 1,
  "pageSize": 20,
  "totalPages": 1
}
```

`statusCode` はこのエンドポイントでは**常に定数 `429`**（記録時にハードコードされます）、`requestType` は中継の記録経路が固定文字列 `"api"` を渡すため実運用では常に `"api"` です。`responseBody` は**200 文字に切り詰められます**（`failure-logs` 側は 2000 文字）。`createdAt` は RFC3339（UTC・秒精度・`Z` 終端）。イベントログはアカウントごとに上限のある LRU なので、古いものから捨てられます（＝ここに見えるのは直近分だけです）。

### GET /api/admin/usage/daily/{date}/records

指定した**CST の暦日**（パス `{date}` は `YYYY-MM-DD`）の使用量記録をページ分割で取得します（新しい順）。

**リクエスト:**

```bash
curl "http://localhost:8080/api/admin/usage/daily/2026-07-26/records?page=1&page_size=20" \
  -H "Authorization: Bearer sk-管理端のキー"
```

**レスポンス:**

```json
{
  "records": [
    {
      "model": "claude-sonnet-4.5",
      "inputTokens": 120,
      "outputTokens": 340,
      "estimatedCost": 0.0051,
      "creditsUsed": 0.0071,
      "createdAt": "2026-07-26T10:30:00Z",
      "credentialId": 12345,
      "credentialLabel": "a@example.com",
      "clientIp": "203.0.113.9"
    }
  ],
  "total": 1,
  "page": 1,
  "pageSize": 20,
  "totalPages": 1
}
```

ページングの前に**その日の新しい順で 2000 件に切り詰められます**（それより古い記録はこのエンドポイントからは見えません）。`credentialLabel` はプールのスナップショットから解決した表示名（ニックネーム → メール → `#{id}` の優先順）で、プールに該当 id が無ければ省略されます。`creditsUsed` / `cacheReadInputTokens` / `cacheCreationInputTokens` / `clientIp` は値が無ければ省略されます。`creditsSaved` は予約枠で**常に省略されます**。日付の綴りが暦日キーと一致しなければ（不正な日付を含め）空ページが `200` で返ります。

### GET /api/admin/usage/summary

時間窓を指定して全アカウント横断の使用量を集計し、グラフ用の時系列分桶と稼働健全性の指標を返します。

**クエリパラメータ:**

| パラメータ | 型 | 必須 | 説明 |
|-----------|-----|------|------|
| `range` | string | ❌ | `6h` / `24h` / `3d` / `7d` / `30d` のいずれか。指定時は `hours` より**優先** |
| `hours` | integer | ❌ | 任意の正整数の時間数（`range` 省略時に使用） |

両方とも省略した場合は `24h`。`range` が上記以外の値なら `400`（`{"error":"invalid range","allowed":[…],"hint":"…"}`）、`hours=0` も `400`（`{"error":"hours must be a positive integer"}`）です。

**リクエスト:**

```bash
curl "http://localhost:8080/api/admin/usage/summary?range=24h" \
  -H "Authorization: Bearer sk-管理端のキー"
```

**レスポンス:**

```json
{
  "range": "24h",
  "windowSecs": 86400,
  "sinceUnix": 1785060000,
  "untilUnix": 1785146400,
  "bucketSecs": 3600,
  "totalRequests": 128,
  "totalInputTokens": 40960,
  "totalOutputTokens": 81920,
  "totalCost": 2.45,
  "totalCredits": 3.4,
  "dailyFallbackApplied": false,
  "series": [
    {"bucketStartUnix": 1785060000, "totalRequests": 12, "totalCost": 0.21, "totalCredits": 0.29}
  ],
  "successfulRequests": 128,
  "failedRequests": 3,
  "errorRate": 0.0228,
  "avgLatencyMs": 1840.5,
  "rotationSuccessRate": 0.9771
}
```

- `range` は正規化後のラベルの**エコー**です（`hours=5` を渡した場合は `"5h"`）。`untilUnix` は現在時刻、`sinceUnix` は `untilUnix - windowSecs`。
- `bucketSecs` は窓幅から自動決定されます：24 時間以下なら `3600`（1 時間ごと）、それより長ければ `86400`（1 日ごと）。`series` は桶の開始時刻の昇順で、活動が無ければ空配列です。
- `dailyFallbackApplied` は、**1 日を超える窓**で生記録の欠落分を日次ロールアップで補填したかどうかを示します（生記録はアカウントごとに上限があり古いものが淘汰されるため）。補填されるのは `totalRequests` / `totalCost` / `totalCredits` だけで、**トークン数には日次集計が無いため補填されません**——`true` のときトークン合計は過小になり得ます。
- `successfulRequests` は窓内の使用量記録の件数、`failedRequests` は窓内の失敗ログ（`401`/`403`）＋スロットルログ（`429`）の件数です。イベントログは LRU 上限があるので `failedRequests` は**下界**であり、`errorRate` は過小に出る側に倒れます。
- `errorRate` = `failedRequests / (successfulRequests + failedRequests)`、`rotationSuccessRate` = `1 - errorRate`（この近似では両者の和は常に 1）。分母が 0 のときは `errorRate = 0.0` / `rotationSuccessRate = 1.0` です。`rotationSuccessRate` は「最終的に成功記録が残ったか」を成功シグナルとする**近似**で、アカウント間リトライの実回数を数えたものではありません。
- `avgLatencyMs` は `latency_ms` を持つ成功記録の平均です（この項目を持たない古い記録は分母に入りません。サンプルが無ければ `0.0`）。なお `latency_ms` 自体は使用量記録のレスポンスには出力されません。
- 数値は丸めずに f64 の精度のまま返します。ストレージが空／窓内に活動が無い場合も `500` ではなく全 0 ＋空 `series` の `200` です。

### GET /api/admin/credits/global

プール全体の**残り積分の合計**を取得します。クエリパラメータはありません。

**リクエスト:**

```bash
curl http://localhost:8080/api/admin/credits/global \
  -H "Authorization: Bearer sk-管理端のキー"
```

**レスポンス:**

```json
{
  "globalCredits": 4820.5,
  "cachedCount": 12,
  "totalCount": 15,
  "oldestCacheUnix": 1785146000
}
```

**共有の残高キャッシュを読むだけで、上流は一切叩きません**。キャッシュにある**すべて**のスナップショットを合計します（**TTL では絞り込みません**）。TTL（5 分）は「上流に問い合わせ直すべきか」に答えるものであり、「表示すべきか」を決めるものではありません。新鮮なものだけを合計していた頃は、アカウント画面を 5 分開かないだけでグローバル残高が空欄になり、全アカウントの残高がディスク上にあるにもかかわらず手動更新を強いていました——それはこのキャッシュが避けるためにある上流呼び出しそのものです。キャッシュが無いアカウントは従来どおり素通しされ、ここでは補充されません。`oldestCacheUnix` は合計に使ったキャッシュのうち最も古い取得時刻（Unix 秒）で、「◯分前時点」の表示に使えます。1 件も無い場合は `globalCredits: 0`、`cachedCount: 0`、`oldestCacheUnix: null` です。

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

`display_name` / `type` / `max_tokens` を含むモデル一覧を取得します（フィールド名はパネルに合わせて snake_case）。各アカウントの上流モデル一覧の**和集合**（キャッシュ）を返し、キャッシュが空なら**プロトコル側の `/models` と同じ共通カタログ**（17 件）にフォールバックします。したがってフォールバック時の id 集合はプロトコル側と一致し、違うのは形式だけです（こちらは `display_name` / `type` / `max_tokens` を持ちます）。和集合が非空のときだけ**上流が実際に返した集合**（＝アカウント階層ごとの真の可用性）になり、カタログより広いことも狭いこともあります。

`type` は常に `"chat"`、`created` は定数 `1700000000`。`rate_multiplier` は上流の値がある場合のみ現れます（カタログへのフォールバック時は常に省略）。`max_tokens` は上流が `0`／未提供のとき `200000` に丸められます。和集合が空だった場合、応答はブロックせずに返しつつ、バックグラウンドで上流の実取得を 1 回だけ起動します（プロセス単位のシングルフライト＋ 60 秒クールダウンで制御されるため、同時アクセスで何度も走ることはありません。次回以降のリクエストで動的な一覧に切り替わります）。

### POST /api/admin/credentials/{id}/models/refresh

指定アカウントの上流モデル一覧を実取得してキャッシュに書き戻します（ボディなし）。

**リクエスト:**

```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/models/refresh \
  -H "Authorization: Bearer sk-管理端のキー"
```

**レスポンス:**

```json
{
  "success": true,
  "id": "12345",
  "count": 18
}
```

`id` は**パスに渡した文字列そのまま**（数値化されません）、`count` は今回キャッシュに格納したモデル件数です。プールに存在しない id は `404`（`{"error":"account not found","id":"…"}`）。**無効化済みのアカウントは除外されません**——プールに居れば見つかり、そのまま上流へ問い合わせます。上流の取得に失敗した場合は `502` で、`error` に上流の状態コードと説明を含む文字列がそのまま入ります。

```json
{
  "success": false,
  "id": "12345",
  "error": "models upstream HTTP 403: ..."
}
```

### POST /api/admin/credentials/models/refresh

サブスクリプション階層ごとに代表アカウントを 1 件ずつ選んでモデル一覧を実取得し、キャッシュに書き戻します（ボディなし）。全アカウントを舐めるわけではありません：無効化済みアカウントは飛ばし、階層が既知のものは階層ごとに 1 件だけ、階層が不明なものは**有界の探索**（和集合が 3 回連続で増えない／成功が 12 件に達する／試し尽くす、のいずれかで打ち切り）を行います。

**リクエスト:**

```bash
curl -X POST http://localhost:8080/api/admin/credentials/models/refresh \
  -H "Authorization: Bearer sk-管理端のキー"
```

**レスポンス:**

```json
{
  "success": true,
  "refreshed": 2,
  "failed": 1,
  "errors": [
    {"id": 12345, "error": "models upstream HTTP 403: ..."}
  ],
  "tiers": ["KIRO FREE", "KIRO PRO+"]
}
```

個々のアカウントが失敗しても呼び出し自体は成功扱いで、常に `200` と `success: true` を返します（失敗は `failed` 件数と `errors[]` に出ます）。`errors[].id` は**数値**です（解析できない id は `0`）。階層名は残高キャッシュの `subscriptionTitle` そのもの（例: `KIRO FREE` / `KIRO PRO+`）で、残高が未取得・期限切れのアカウントは「階層不明」として探索側に回ります。`tiers` は今回カバーできた階層名の一覧で、探索段階でも階層を特定できなかった場合は `"unknown"` が混ざります。既知の階層が 1 つも無く探索も空振りなら `refreshed` は `0` になり得ます。

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
  "version": "0.16.0",
  "kiroVersion": "0.11.107",
  "rustVersion": "1.90.0",
  "runMode": "Docker",
  "uptimeSecs": 3600
}
```

`masterApiKey` は設定済み `apiKey` の**完全な平文**（未設定なら `null`）で、ここでは**マスキングされません**——パネルがブラウザ側でマスクして表示し、コピーボタンは実値を使います。マスキング済みの値が必要なら `GET /api/admin/config/auth-keys` を使ってください。`version` は kiro2api のバージョン、`kiroVersion` は偽装した上流 UA のバージョン、`rustVersion` はビルド時の rustc バージョンです。ほかに実行時メトリクス（`serverTime`、`serverTimeUnix`、`os`、`memoryUsedBytes`、`memoryTotalBytes`、`cpuPercent`、`runMode`、`pid`、`uptimeSecs`）も含まれます。

### GET /api/admin/check-update

GitHub の最新リリースを引いて、現在のバージョンと比較します。クエリパラメータはありません。

**リクエスト:**

```bash
curl http://localhost:8080/api/admin/check-update \
  -H "Authorization: Bearer sk-管理端のキー"
```

**レスポンス:**

```json
{
  "current": "0.4.0",
  "latest": "0.4.1",
  "hasUpdate": true,
  "updateUrl": "https://github.com/xwteam/kiro2api/releases/tag/v0.4.1",
  "releaseNotes": "..."
}
```

`current` はビルドに焼き込まれた kiro2api のバージョン、`latest` はリポジトリ `xwteam/kiro2api` の `releases/latest` の `tag_name` から先頭の `v` を除いたものです。`hasUpdate` は 2 つの文字列が**一致しないこと**だけを見ます（セマンティックバージョンの大小比較はしません）。`updateUrl` はリリースの `html_url`、無ければリリース一覧ページ。

**この端点は失敗しません**——ネットワークエラー、リリースが 1 件も無い、プライベートリポジトリで `404`、といった場合はすべて保守的に `hasUpdate: false` / `latest = current` / `releaseNotes: ""` / `updateUrl` = リリース一覧ページ、で `200` を返します（エラーで UI を止めないため）。

### POST /api/admin/update

更新手順を返します。**サーバー上で何かを実行するわけではありません**——実行すべきコマンド文字列を返すだけで、プロセスもコンテナも触りません（パネルはこれをコピーボタン付きで表示します）。リクエストボディは不要です。

**リクエスト:**

```bash
curl -X POST http://localhost:8080/api/admin/update \
  -H "Authorization: Bearer sk-管理端のキー"
```

**レスポンス:**

```json
{
  "status": "ok",
  "message": "请在服务器上执行以下命令完成更新:",
  "command": "docker compose pull && docker compose up -d"
}
```

3 フィールドとも**ハードコードされた定数**で、入力にも実行環境にも依存しません（`message` は上記の中国語の文字列がそのまま返ります）。常に `200`。

### POST /api/admin/restart

プロセスを終了させ、コンテナ／プロセス監視の再起動ポリシーに拾わせます。**誤操作防止のためクエリ `?confirm=true` が必須**です。リクエストボディは不要です。

**リクエスト:**

```bash
curl -X POST "http://localhost:8080/api/admin/restart?confirm=true" \
  -H "Authorization: Bearer sk-管理端のキー"
```

**レスポンス:**

```json
{
  "status": "ok",
  "message": "Server restarting..."
}
```

`confirm` が無い／`true` でない場合は再起動せず `400` を返します：

```json
{
  "error": {
    "message": "重启需二次确认,请带查询参数 ?confirm=true",
    "type": "confirmation_required"
  }
}
```

レスポンスは**先に**返り、その後バックグラウンドで 0.5 秒待ってから遅延書き込み中の状態（使用量統計・API-KEY ストア・残高キャッシュ・失敗/スロットルイベントログ）をディスクへフラッシュし、`exit(0)` します。したがって直前に行った API-KEY の削除や作成は取り消されません。コンテナは `restart: unless-stopped` で動いていれば自動的に起動し直されますが、**守護プロセスの無いベアメタル運用ではこれは単なる停止と等価**です（systemd / supervisor 等での保活が前提）。

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

`POST /admin/api/accounts/{id}/disable` は [`POST /api/admin/credentials/{id}/disabled`](#post-apiadmincredentialsiddisabled) に `{"disabled": true}` を送るのと**同じプール操作**の旧綴りです（`…/enable` は同じく `{"disabled": false}` 相当）。新旧で違うのは呼び出し方と応答の形だけなので、ここでは重複して仕様を書きません——新しいほうを使ってください。

旧側の固有の作法：リクエストボディを取らず（`Content-Type` も不要）、`disabled` の値はパスの動詞で決まります。応答は `{success, message}` ではなく次の形で、`id` は**パスに渡した文字列がそのまま**返ります。

```json
{
  "ok": true,
  "id": "12345",
  "disabled": true
}
```

プールに存在しない id は `404`（`{"error":"account not found","id":"…"}`）。なお「メモリ上のみ」という性質は新旧どちらも同じで（両者とも同一のプール操作を呼ぶだけでディスクへは書きません）、旧側だけの制限ではありません。

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

その key の使用量記録をページ分割で取得します（`?page=&page_size=`、新しい順）。管理側と違い、**既定の `page_size` は `50`** です（`page` の既定は同じく `1`）。クエリ名は snake_case（`pageSize` ではありません）で、丸めの規約は管理側と同じ——`page_size` は最低 `1`、`page` は `[1, totalPages]` にクランプされます。

**リクエスト:**

```bash
curl "http://localhost:8080/api/user/usage/records?page=1&page_size=20" \
  -H "x-api-key: sk-あなたのキー"
```

**レスポンス:**

```json
{
  "records": [
    {
      "model": "claude-sonnet-4.5",
      "inputTokens": 120,
      "outputTokens": 340,
      "estimatedCost": 0.0051,
      "creditsUsed": 0.0071,
      "createdAt": "2026-07-26T10:30:00Z",
      "clientIp": "203.0.113.9"
    }
  ],
  "total": 1,
  "page": 1,
  "pageSize": 50,
  "totalPages": 1
}
```

返るのは**その key に紐づく記録だけ**です。1 件のレコードが持ち得るのは `model` / `inputTokens` / `outputTokens` / `estimatedCost` / `createdAt` と、値がある場合のみ現れる `creditsUsed` / `cacheReadInputTokens` / `cacheCreationInputTokens` / `clientIp` です。管理側の同種レスポンスと違い**`credentialId` は含まれません**（どのアカウントで処理されたかは利用者側には出しません）。`creditsSaved` と `credentialLabel` はこのエンドポイントでは解決されず**常に省略されます**。key が有効で記録が 1 件も無ければ `500` ではなく空ページ（`total: 0`、`totalPages: 0`、`page: 1`）が `200` で返ります。key が無効／停用／期限切れなら `401`（`{"error":"…"}`）。

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
  "version": "0.16.0"
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
| 400 | パラメータエラー / 中継が解決できないモデル名 / 上流がアカウント階層で提供しないモデル（`INVALID_MODEL_ID`）。管理エンドポイント固有では `POST /api/admin/restart` に `?confirm=true` が無い場合と、`GET /api/admin/usage/summary` に未知の `range` または `hours=0` を渡した場合 | リクエストパラメータとモデル名を確認。③**リクエストボディが上流の長さ上限を超過**——上流の reason コードは `CONTENT_LENGTH_EXCEEDS_THRESHOLD`。「Input is too long…」に加え、この誤りは自然回復しないこと(クライアントは毎ターン全履歴を再送するため次はさらに長くなる)と、コンテキストを削るか会話を新規に始める必要があることを示します。**この種別も再試行せず、アカウントを傷つけません**(v0.7.12 以前は一時的エラーと誤判定し、アカウントを跨いで再試行して触れた全アカウントに失敗を記録していました)。④**メッセージにツール呼び出しがあるのにツール定義が無い**——上流の reason コードは `TOOL_CONFIG_MISSING`。通常は発生しません:中継が会話履歴に現れたツール名から最小限の仕様を補って送信します。この区分は保険であり、同様に再試行せずアカウントも傷つけません |
| 401 | 未認証（key がない、誤った key、無効化/期限切れのストア key） | API Key を確認 |
| 402 | ストア管理の API-KEY が消費上限に到達（本体は `{"type":"error","error":{"type":"billing_error","message":"…"}}`）。判定には在途分の予約（USD 単位で `1.0`、`credits` 単位で約 `1.39`）が含まれるため、**残りが 1 回分の見積を下回った時点で**上限を使い切る前に拒否が始まります | key の上限を引き上げるか、使用量をリセット |
| 403 | 禁止 | 権限がない |
| 404 | 見つからない（パスが存在しない。または管理エンドポイントにプールへ存在しないアカウント / API-KEY / ログインセッションの id を渡した） | id とパスを確認 |
| 422 | リクエストボディのデシリアライズ失敗（必須フィールドの欠落や型不一致）。4 プロトコルの対話エンドポイントと `/v1/messages/count_tokens` は自前で拒否を受け取り、それぞれの形状の `400` に変換するため、`422` は主に `/api/admin/*` と `/api/user/login` のように `Json` 抽出器を直接使うエンドポイントで発生します | ボディの形状を確認 |
| 429 | 上流 Kiro のスロットリング（`ThrottlingException` 系の例外を変換）。`MAX_RPM_PER_CREDENTIAL` の超過はこのコードにはならず、他アカウントへローテーションされ、全滅した場合のみ `503` になります | しばらく待機 |
| 502 | 上流エラー | 上流の Kiro が失敗。**すべての上流リクエストに `Connection: close` を付与し、クライアントは接続を再利用しません。** 各リクエストは**異なるアカウント**のトークンを運び、user-agent 内の machineId もアカウントごとに変わるため、接続を再利用すると一つの TCP/TLS 上に数十の異なる身元が順に現れます —— 実際のクライアントには不可能であり、アカウント共有の最も直接的な証拠です。一時的失敗(ネットワーク/5xx/スロットリング)はアカウント切替前に 200ms→2s の指数バックオフ+ジッタ、アカウント級の失敗は待機しません 上流接続は **HTTP/1.1** に固定します(実クライアントは h2 へ昇格せず、`Connection: close` は 1.1 のヘッダで h2 では禁止されているため、固定しなければ飾りにすぎません)。TLS バックエンドは既定で **native-tls(OpenSSL)** とし、実クライアントの ClientHello 指紋に合わせます —— この指紋は HTTP の内容が送られる**前**に露出します。 |
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
