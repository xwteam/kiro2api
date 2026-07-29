# 使用ガイド

kiro2api の Web パネルとクライアント接続方法について説明します。

## Web 管理パネル

kiro2api は、ブラウザベースの管理パネル（`/admin`）とユーザーパネル（`/user`）を提供しています。パネルは静的アセットとしてバイナリに埋め込まれており（rust-embed）、追加のフロントエンドサーバーは不要です。

### アクセス方法

ブラウザで以下の URL にアクセス：

```
http://localhost:8080/admin
```

または、リモートサーバーの場合：

```
http://サーバーIP:8080/admin
```

### ログイン

初回アクセス時、API Key の入力を求められます。

1. `.env` または `config.json` から `adminApiKey`（未設定の場合は `apiKey`）を確認
2. パネルに入力してサインイン

> **ヒント**: kiro2api の統一認証は 6 チャネルに対応し、最初に見つかった 1 つを採用します（優先順位は `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > `?api_key=` > `?token=` > `?key=`）。管理 API（`/api/admin/*`）は `adminApiKey`（未設定時は `apiKey`）を設定して初めて保護されます——どちらも未設定なら無認証で誰でもアクセスできるため、公開環境では必ず `ADMIN_API_KEY` を設定してください。なおパネル本体（`/admin`・`/user`）に認証はなく、ログイン画面は入力したキーをブラウザに保存して API 呼び出しに使うだけです。

## パネル機能

### ダッシュボード

メインページには以下の情報が表示されます：

| 項目 | 説明 |
|------|------|
| 運行時間 | サービス起動からの経過時間（リアルタイム更新） |
| 全体残高 | 全アカウントの残クレジット（共有キャッシュ） |
| 二次元コード | 微信・スポンサーシップ QR コード（リモート設定から取得、クリックで拡大） |
| システム情報 | バージョン、Rust、OS、メモリ、CPU、PID、実行モード |
| 更新チェック | GitHub Release との比較で新バージョンを検出（ダイアログでローカライズされたリリースノート + コピー可能なアップグレードコマンドを表示） |
| アカウント状態 | 各アカウントの健全性、重み、最後の使用時刻 |

> **更新チェック**: ダッシュボードを開くと、パネルはバックグラウンドで自動的に更新を確認します。GitHub 上に新しいリリースがある場合、「更新チェック」ボタンが「vX に更新」とハイライトされます。クリックすると「サービスを vX に更新」ダイアログが開き、スクロール可能なボックスに**現在の UI 言語のリリースノート**が表示され、あわせてアップグレードコマンド `docker compose pull && docker compose up -d` とワンクリックのコピーボタンが提供されます。ダイアログは情報の表示のみで、アップグレードを自動実行することはありません。

### アカウント管理

Kiro（CodeWhisperer）の認証情報プールを管理します。

**機能:**

- **アカウント追加**: 対話型の 3 種類のログインフロー、またはトークンの一括インポート
- **アカウント削除**: 不要なアカウントを削除
- **有効/無効の切替**: アカウントの一時停止・再有効化・冷却リセット
- **優先度/重み編集**: 負荷分散のための `priority` / `weight` を調整
- **健全性チェック**: 各アカウントの状態・失敗/スロットル数・残高を確認

**操作例:**

1. 左側メニューから「認証情報（Credentials）」を選択
2. 「新規追加」ボタンをクリック
3. **Builder ID**（デバイスコード）/ **IAM Identity Center（SSO）**（認可コード）/ **ソーシャルトークン** のいずれかを選択
4. 画面の指示に従って認可を完了

> `credentials.json` を手で触らずに、対話型フローや一括インポート（1 行 1 件の bearer/SSO トークン、または貼り付けた認証情報の配列 / `{accounts}` オブジェクト）でアカウントを取り込めます。一括インポートでは**アカウントを 1 件ずつ追加**し、追加直後にそのアカウントの残高を 1 回照会（上流への実際の `getUsageLimits` 呼び出し）して**疎通を検証**します。有効なアカウントは保持し、無効なアカウントは自動的にロールバック/削除して除外します。さらに `refreshToken` による**重複排除**を行い、すでにプールにあるアカウントはスキップするため、同一アカウントが二重に取り込まれることはありません（同じアカウントを重複させると 2 つの認証情報が同一のローテーショントークンを奪い合い、相互失効・クォータ浪費・上流のリスク管理を招きます）。
>
> インポートダイアログはこの処理を**リアルタイム表示**します：プログレスバーと「アカウント i/N を処理中」の表示、成功/重複/失敗の集計、そして各アカウントのステータスリストが逐次更新されます（待機中 → 確認中 → 検証中 → 検証済み（使用量付き）/ 重複 / 失敗（除外））。**検証済みアカウントは即座に保存される**ため、途中で中断してもすでに成功した分は保持されます。インポート中はダイアログを閉じることはできません。

### API-KEY 管理

呼び出し側に渡す対外 key を一元管理します。

**操作:**

1. 「API-KEY 管理」を開く
2. 「新規追加」をクリック
3. 上限額 / 有効期限 / ラベルを設定
4. 「保存」をクリック

**機能:**

- key の発行 / 無効化 / ラベル変更
- key ごとの使用量の確認とリセット
- ページ分割されたリクエスト記録の閲覧

> **消費上限の適用範囲**: key に設定した上限額は 4 つのプロトコルフロントエンド（Anthropic / OpenAI / OpenAI-Responses / Gemini）**すべて**で有効です。どのエンドポイントを使っても課金はその key に紐づけて計上され、上限を超える見込みになった時点で `402` を返します。使用量統計にも 4 プロトコル分がまとめて反映されます。
>
> **上限ぴったりまでは使い切れません**: 実際のコストはレスポンスが返るまで確定しないため、認証ゲートはリクエストごとに **1 回分の名目見積（1 USD、`credits` 単位なら 1 USD ÷ 0.72 ≒ 1.39 クレジット）を在途分として先に予約**します。判定式は `実績 + 予約中 + 見積 > 上限` で `402`。つまり残りが 1 リクエスト分の見積を下回った時点で、実績が上限に達していなくても以後のリクエストは全部 `402` になります（ゲートは handler の手前にあるため、`GET /v1/models` のような無課金の呼び出しも同様に弾かれます）。予約はリクエスト完了時に解放され、確定コストで記帳し直されます。
>
> この仕様上、**見積より小さい上限（例：`credits` で 1.0、USD で 0.5）を設定した key は最初から 1 回も通りません**。上限は 1 リクエスト分の見積より十分大きい値にしてください。なおユーザーパネルの残量バッジは実績（`実績 >= 上限`）だけを見ているため、この帯域では「正常」と緑表示のまま全リクエストが `402` になります。

### リアルタイムログ

API リクエストのログをリアルタイムで表示します。

**機能:**

- **方向フィルタ**: リクエスト/レスポンスを個別に表示
- **テキスト検索**: ログ内容から検索
- **ページネーション**: 構造化テーブルでページ分割表示
- **SSE 実時推送**: サーバーログをリアルタイムでストリーミング
- **ログ管理**: スナップショット表示 / `.txt` ダウンロード（`logCapacity > 0` が必要）

### 使用統計

API 使用状況の統計情報を表示します。

**表示項目:**

| 項目 | 説明 |
|------|------|
| 日次サマリー | 日ごとのリクエスト数と使用量 |
| アカウント別サマリー | 各アカウントの使用量（アカウントラベル付き） |
| クライアント IP | リクエスト元の IP を記録 |
| 失敗/スロットルログ | 分類済みの失敗と限流の記録 |
| リアルタイム RPM | アカウントごとの毎分リクエスト数 |

**下钻:**

- 日次で下钻して各日の詳細を確認できます。

### モデルテスト

利用可能な任意のモデルへ、中継を通してテストリクエストを直接送信し、生の結果を表示します。アカウント/モデルが実際に動作するかを確認するのに便利です。

**操作:**

1. 左側メニューから「モデルテスト」を開く
2. モデルを選択（任意でエンドポイントも指定）
3. 「送信」をクリックして結果を確認

**機能:**

- 中継を通してテストリクエストを送信し、生のレスポンスを表示
- 作成済みの API-KEY のいずれかで中継エンドポイントを呼び出し
- **カスタム key が未作成の場合は、マスター API キー（`adminApiKey` / `apiKey`）へ自動的にフォールバック**するため、初期状態でもそのままテスト可能

> key はブラウザ（`localStorage`）にのみ保存され、中継エンドポイントの呼び出しにのみ使用されます。

### 設定

サービスの動作パラメータを実行時に変更できます。**変更は即座に反映され、再起動は不要です。**

**設定カテゴリ:**

| カテゴリ | 設定項目 |
|---------|---------|
| 負荷分散 | `priority`（等権ラウンドロビン）/ `balanced`（重み加権）の実行時切替 |
| 認証キー | `apiKey` / `adminApiKey` のローテーション（即時反映） |
| 集成示例 | プロトコル × 言語のコピー可能なコード片 |
| サービス | ワンクリック再起動、更新チェック |

> [!WARNING]
> **マスター API キーはマスキングされません**: マスター API キー（`apiKey`）は「API キー」ページの「サービス接続情報」カードに表示されますが、マスクしているのは**ブラウザ側の表示だけ**です。値の取得元である `GET /api/admin/server-info` の `masterApiKey` は**完全な平文**を返し（コピーボタンがその実値を使うため）、サーバー側でマスキングは一切行われません。マスキング済みの値を返すのは `GET /api/admin/config/auth-keys` だけです。したがって `server-info` のレスポンスを issue やログ、サードパーティのツールに貼り付けないでください。

> **再起動 / 停止時の挙動**: 優雅な停止では在途リクエストの排出を待ちますが、その待機時間には**上限（8 秒）**があります。実時ログの SSE のような無期限の長時間接続が残っていても停止処理が止まることはなく、最後の統計フラッシュが必ず実行されるため、使用量と課金の記録が再起動で失われることはありません。

### 多言語切替

右上の地球アイコンから言語を切り替えられます。

**対応言語:**

- 简体中文（簡体中国語）
- 繁體中文（繁体中国語）
- English（英語）
- 日本語
- 한국어（韓国語）

### 右上コントロールバー

| アイコン | 機能 |
|---------|------|
| 🟢 | 運行状態バッジ |
| 🐙 | GitHub リポジトリ |
| 🔄 | サービス再起動 |
| 🌙/☀️ | ダークモード/ライトモード切替 |

## 画像入力

kiro2api はマルチモーダルコンテンツをサポートしており、画像の入力が可能です。4 つのプロトコル形式での画像転送に対応しています。

### OpenAI 形式

`messages` 配列で `image_url` タイプを使用します。対応するのは **Base64 Data URI（`data:image/...;base64,...`）のみ**です。`http(s)://` のリモート URL は上流 Kiro がインライン base64 しか受け取らないため、**黙って無視されるのではなく `400`（`invalid_request_error`）で拒否されます**——画像は自分でダウンロードして Data URI に変換してから送ってください（Anthropic 形式の `{"type":"image","source":{"type":"url",…}}` も同じく `400` です）。

**Base64 画像の例**：

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
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
    ]
  }'
```

### Claude（Anthropic）形式

`content` 配列で `image` タイプを使用します。

```bash
curl -X POST http://localhost:8080/v1/messages \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{
    "model": "claude-sonnet-4.5",
    "max_tokens": 1024,
    "messages": [
      {
        "role": "user",
        "content": [
          {"type": "text", "text": "これは何ですか"},
          {
            "type": "image",
            "source": {
              "type": "base64",
              "media_type": "image/png",
              "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
            }
          }
        ]
      }
    ]
  }'
```

### Gemini ネイティブ形式

`parts` 配列で `inlineData` を使用します。

```bash
curl -X POST "http://localhost:8080/v1beta/models/claude-sonnet-4.5:generateContent" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{
    "contents": [
      {
        "role": "user",
        "parts": [
          {"text": "これは何ですか"},
          {
            "inlineData": {
              "mimeType": "image/png",
              "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
            }
          }
        ]
      }
    ]
  }'
```

## 対応モデル

kiro2api は後端の Kiro（CodeWhisperer）が提供する Claude 系モデルを統一して対外提供します。クライアントから渡されたモデル名は、**小文字の部分一致**で Kiro 内部モデルにマッピングされます。

| モデル名 | 説明 |
|---------|------|
| `claude-sonnet-4.5` | 無料階層（KIRO FREE）でも通常利用可能な標準モデル |
| （その他） | opus / GPT 系など、より上位のサブスクリプション階層で認可されるモデル |

> **重要**: **利用可能なモデルはアカウントのサブスクリプション階層に依存します**。無料階層（KIRO FREE）は通常 `claude-sonnet-4.5` のみを認可します。サポートされていないモデルを要求すると、明確に `400`（`INVALID_MODEL_ID`）が返されます（静かに失敗したり、無駄な再試行でアカウントを傷つけたりはしません）。

**モデルの確認**：`GET /v1/models`（または `/claude/v1/models`、`/v1beta/models`）は**固定 3 件**の短いリストを返すだけで、アカウントプールもサブスクリプション階層も参照しません。したがって「一覧に載っている＝使える」とは限らず、階層が足りなければ `400`（`INVALID_MODEL_ID`）になります。

```bash
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-あなたのキー"
```

より広いカタログは管理 API で確認できます（各アカウントの上流モデル一覧の**和集合**、なければ静的な 17 件にフォールバック）。

```bash
curl http://localhost:8080/api/admin/models \
  -H "Authorization: Bearer sk-管理端のキー"
```

## サードパーティクライアント接続

kiro2api は 4 つのプロトコルフロントエンドを同時に提供しているため、多くのクライアントから直接接続できます。base URL は各社の**標準裸前缀**（OpenAI = `{host}/v1`、Anthropic = `{host}`、Gemini = `{host}/v1beta`）、または明示的なベンダー前缀（`/openai/v1`、`/claude/v1`、`/gemini/v1beta`）を使用できます。

### ChatGPT-Next-Web

1. ChatGPT-Next-Web を起動
2. 設定 → API 設定
3. API URL を入力：

```
http://サーバーIP:8080/v1
```

4. API Key を入力：

```
sk-あなたのキー
```

5. 保存して使用開始

### LobeChat

1. LobeChat を起動
2. 設定 → 言語モデル
3. プロバイダを「OpenAI」に設定
4. API URL：

```
http://サーバーIP:8080/v1
```

5. API Key を入力
6. モデル（`claude-sonnet-4.5`）を選択して使用

### OpenCat

1. OpenCat を起動
2. 設定 → API 設定
3. API エンドポイント：

```
http://サーバーIP:8080/v1
```

4. API Key を入力
5. 使用開始

### cURL コマンド

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {"role": "user", "content": "こんにちは"}
    ]
  }'
```

### Python SDK

```python
from openai import OpenAI

client = OpenAI(
    api_key="sk-あなたのキー",
    base_url="http://localhost:8080/v1"
)

response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[
        {"role": "user", "content": "Python で Hello World を出力してください"}
    ]
)

print(response.choices[0].message.content)
```

### Node.js SDK

```javascript
import OpenAI from "openai";

const client = new OpenAI({
  apiKey: "sk-あなたのキー",
  baseURL: "http://localhost:8080/v1",
});

const message = await client.chat.completions.create({
  model: "claude-sonnet-4.5",
  messages: [
    { role: "user", content: "JavaScript で Hello World を出力してください" },
  ],
});

console.log(message.choices[0].message.content);
```

## トークン自己修復とアカウントプール

kiro2api は Kiro（CodeWhisperer）の認証情報プールを管理し、トークンの期限切れや上流の障害を自動的に処理します。手動でのトークン差し替えは不要です。

### トークンの自動更新

- Kiro のアクセストークンは `expiresAt` に基づいて期限切れになります。
- token が期限切れになると、kiro2api が**自動的にメモリ内でリフレッシュ**します（single-flight 協調により、並行リフレッシュによる 401 の連鎖を防ぎます）。
- リフレッシュが成功すると、新しいトークンは `credentials.json` に**アトミックに書き戻され**ます。

### エンドポイント回退とアカウント間再試行

- 上流エンドポイントは **Kiro IDE → CodeWhisperer → AmazonQ** の順に自動回退します。`429` やネットワークエラーで次のエンドポイントへ切り替えます。
- アカウントレベルの失敗は**アカウント間で自動的に再試行**されます。
- **body-aware 失敗分類**：本当に認証情報が失効した場合のみ永久無効化し、配額 / 風控 / 限流はすべて分級冷却で自己修復します。確定的なリクエストエラー（サポートされていないモデルの `INVALID_MODEL_ID` など）は再試行せず、上流の原因をそのままクライアントに返します。

### アカウントの追加

管理パネルの「認証情報」タブから、対話型の 3 種類のログインフロー（**Builder ID** デバイスコード / **IAM Identity Center（SSO）** 認可コード / **ソーシャルトークン**）でアカウントを追加できます。詳細は [DEPLOY.md](DEPLOY.md) を参照してください。

## 会話コンテキスト

kiro2api は複数ターンの会話をサポートしています。

### コンテキスト管理

クライアント側で `messages` 配列に会話履歴を含めると、コンテキストが保持されます。本サービスには**サーバー側のセッション記憶はありません**。リクエストのたびに完全な会話履歴を含めてください。

```python
messages = [
    {"role": "user", "content": "Python とは何ですか？"},
    {"role": "assistant", "content": "Python は..."},
    {"role": "user", "content": "その特徴を教えてください"},
]

response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=messages
)
```

> **注意**: OpenAI Responses（`/v1/responses`）で `previous_response_id` を指定すると `400` が返されます。サーバー側にセッション記憶がないため、毎回完全な履歴を送信してください。

## ストリーミング応答

リアルタイムで応答を受け取ることができます。4 つのプロトコルすべてでストリーミングに対応しています。

### Python での例

```python
response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "長編小説を書いてください"}],
    stream=True
)

for chunk in response:
    if chunk.choices[0].delta.content:
        print(chunk.choices[0].delta.content, end="", flush=True)
```

### cURL での例

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "詩を書いてください"}],
    "stream": true
  }'
```

> OpenAI Chat の流式は `chat.completion.chunk` 形式の行を返し、`data: [DONE]` で終端します。Gemini の流式は `:streamGenerateContent`（`?alt=sse`、camelCase）を使います。`stream:false` の場合でも、サービス内部では上流のイベントストリームをデコードし、収集完了後に完全な JSON を一度に返します。

> **エラーと切り詰めの扱い**: 上流がエラーを返した場合、および**ストリーム途中で伝送が中断した場合**（接続リセット / 読み取りタイムアウト / chunked ボディの未終端）は、そのプロトコル規範のエラーイベントでストリームを終端します（Anthropic は `error` イベント、OpenAI はエラー chunk で `[DONE]` を付けない、Responses は `response.failed`、Gemini はエラーブロックで `STOP` を返さない）。**正常終了として報告されることはありません**ので、クライアント側の再試行ロジックが正しく発火します（タイムアウトは `504`、その他は `502`）。また `max_tokens` に達した場合やコンテキストが尽きた場合は、非ストリーミングと同じ切り詰め理由（`max_tokens` / `length` / `MAX_TOKENS` / `incomplete`）で報告します。

## 関数呼び出し（Function Calling）

モデルに特定のタスクを実行させることができます。ツール呼び出しは 4 つのプロトコル間で**真透传**されます（Anthropic `tool_use` / OpenAI `tool_calls` / Gemini `functionCall`）——模擬はしません。

```python
response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[
        {"role": "user", "content": "東京の天気を調べてください"}
    ],
    tools=[
        {
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "指定都市の天気を取得",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "city": {"type": "string", "description": "都市名"}
                    },
                    "required": ["city"]
                }
            }
        }
    ]
)

# モデルが関数呼び出しを提案
if response.choices[0].message.tool_calls:
    for tool_call in response.choices[0].message.tool_calls:
        print(f"関数: {tool_call.function.name}")
        print(f"引数: {tool_call.function.arguments}")
```

## トラブルシューティング

### 接続エラー

**症状**: `Connection refused`

**解決方法**:

1. サービスが起動しているか確認：

```bash
docker compose ps
```

2. ポートが正しいか確認：

```bash
curl http://localhost:8080/health
# {"service":"kiro2api","status":"ok","version":"0.7.3"}
```

### 認証エラー

**症状**: `401 Unauthorized`

**解決方法**:

1. API Key が正しいか確認
2. ヘッダーが正しいか確認：

```bash
# 正しい
curl -H "Authorization: Bearer sk-xxx"

# 間違い
curl -H "Authorization: sk-xxx"
```

### モデルが見つからない

**症状**: `400 INVALID_MODEL_ID`

**解決方法**:

1. より広いモデルカタログを確認（管理 API。上流の和集合、なければ静的な 17 件。`/v1/models` は固定 3 件を返すだけで階層フィルタは掛かりません）：

```bash
curl http://localhost:8080/api/admin/models \
  -H "Authorization: Bearer sk-管理端のキー"
```

2. アカウントのサブスクリプション階層を確認（無料階層は通常 `claude-sonnet-4.5` のみ認可。opus/GPT 等はより上位の階層が必要）

### アカウントが冷却中

**症状**: 配額超過 / 限流 / 風控によりアカウントが一時的に使用不可

**解決方法**:

1. 管理パネルの「認証情報」タブでアカウントの健全性・冷却状態を確認
2. 分級冷却により自動的に自己修復します。複数アカウントを追加しておくとアカウント間再試行で可用性が向上します
