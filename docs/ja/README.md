<div align="center">

<img src="../logo.png" width="128" height="128" alt="kiro2api">

<h1>kiro2api</h1>
<h3>マルチプロトコル AI 中継 · Kiro バックエンド</h3>
<p>単一コードベースで OpenAI / Anthropic / OpenAI-Responses / Gemini の 4 大 AI SDK に同時対応、Kiro（CodeWhisperer）バックエンドが Claude 系モデルを統一提供、純非同期 Rust アーキテクチャ、Docker で高速デプロイ。</p>

<p>
  <img src="https://img.shields.io/badge/Rust-2024-orange?style=flat-square&logo=rust&logoColor=white" alt="Rust">
  <img src="https://img.shields.io/badge/axum-0.8-000000?style=flat-square&logo=rust&logoColor=white" alt="axum">
  <img src="https://img.shields.io/badge/tokio-async-4E9A06?style=flat-square&logo=rust&logoColor=white" alt="tokio">
  <img src="https://img.shields.io/badge/Docker-20.10+-2496ED?style=flat-square&logo=docker&logoColor=white" alt="Docker">
  <img src="https://img.shields.io/badge/arch-amd64%20%7C%20arm64-4285F4?style=flat-square&logo=linux&logoColor=white" alt="Arch">
  <img src="https://img.shields.io/badge/License-MIT-green?style=flat-square" alt="License">
  <img src="https://img.shields.io/badge/version-v0.2.0-success?style=flat-square" alt="Version">
</p>

<p>
  <a href="#-最近の更新">最近の更新</a> &bull;
  <a href="#-主な機能">主な機能</a> &bull;
  <a href="#-システム要件">システム要件</a> &bull;
  <a href="#-クイックデプロイ">クイックデプロイ</a> &bull;
  <a href="#-統合例">統合例</a> &bull;
  <a href="#-api-エンドポイント">API エンドポイント</a> &bull;
  <a href="#-設定">設定</a> &bull;
  <a href="#-重要な注意事項">重要な注意事項</a> &bull;
  <a href="#-ロードマップ">ロードマップ</a>
</p>

<p>
  📖 ドキュメント：<a href="../zh-CN/README.md">简体中文</a> | <a href="../zh-TW/README.md">繁體中文</a> | <a href="../en/README.md">English</a> | 日本語 | <a href="../ko/README.md">한국어</a>
</p>

<br>

<a href="https://github.com/xwteam/kiro2api/issues"><img src="https://img.shields.io/github/issues/xwteam/kiro2api?style=flat-square" alt="Issues"></a>
<a href="https://github.com/xwteam/kiro2api/stargazers"><img src="https://img.shields.io/github/stars/xwteam/kiro2api?style=flat-square" alt="Stars"></a>

</div>

---

> [!NOTE]
> このプロジェクトは研究と学習目的のみです。責任を持って使用し、商業目的での使用は禁止です。

> [!WARNING]
> このプロジェクトは Amazon / AWS / Kiro と無関係です。Kiro（CodeWhisperer）バックエンドをマルチプロトコル互換 API としてラップしており、関連する利用規約に違反する可能性があります。自己責任で使用してください。作者はアカウント停止やデータ損失に対して責任を負いません。

> [!TIP]
> バックエンドは Kiro（CodeWhisperer）アカウントプールです。**利用可能なモデルはアカウントのサブスクリプション階層に依存します**：無料階層（KIRO FREE）は通常 `claude-sonnet-4.5` のみを許可し、opus/GPT などはより上位の階層が必要です。サポートされていないモデルを要求すると明示的に 400（`INVALID_MODEL_ID`）を返し、黙って失敗することはありません。

> [!IMPORTANT]
> `apiKey`/`API_KEY` が空の場合、プロトコルエンドポイントは**オープンアクセス**になります（起動時に警告）。外部デプロイでは必ず設定してください。管理インターフェース `/api/admin/*` は `adminApiKey`（未設定時は `apiKey` にフォールバック）を設定して初めて保護されます——**どちらのキーも未設定なら、管理インターフェースもパネル同様に誰でも叩ける状態**で、認証情報の追加・削除も認証キーの書き換えも自由にできてしまいます。`/admin`・`/user` パネル本体は常に認証されません。インターネットに公開するなら `ADMIN_API_KEY` の設定が必須です。コンテナイメージには `HOST=0.0.0.0` が組み込まれています。ベアメタルデプロイでは `HOST` を安易に `0.0.0.0` に変更しないでください。

---

## 📝 最近の更新

> 完全な変更ログは [CHANGELOG.md](../../CHANGELOG.md) を参照してください。

| 日付 | 更新内容 |
|------|----------|
| 2026-07-26 | v0.2.0 - 🔒 全チェーン監査に基づく修正：API-KEY の消費上限が **4 プロトコルすべて**で有効に（従来は Anthropic エンドポイントでしか効かず、残り 3 つは無制限に消費でき使用量も 0 表示のままでした）；ユーザー級の API-KEY しか設定していない場合に管理インターフェースが無認証で開放される問題を修正；アップストリームのエラー・ストリーム途中の伝送中断・切り詰めを、どのプロトコルでも正常完了として報告しなくなりました；アカウントプールのリフレッシュ失敗をプールへ反映；再起動で使用量/課金が失われなくなり、統計ファイルはロールバック可能な形式を維持；`--credentials` と `PORT` に追従するヘルスチェックが実際に機能するように |
| 2026-07-26 | v0.1.4 - 🐛 修正：Anthropic の `system` フィールドがコンテンツブロック配列（文字列だけでなく）に対応——Claude Code / プロンプトキャッシュ対応 SDK が配列で送っても 422 にならない |
| 2026-07-26 | v0.1.3 - 📥 一括 JSON インポートがアカウントごとの進捗をリアルタイム表示：プログレスバー、成功/重複/失敗の集計、行ごとのステータスリスト（検証中 → 検証済み（使用量付き）/ 重複 / 失敗（ロールバック））；検証済みアカウントは即座に保存されるため、途中で中断しても失われません |
| 2026-07-25 | v0.1.2 - 🔔 更新ダイアログ刷新：更新チェックダイアログが現在の UI 言語のリリースノート + コピー可能なアップグレードコマンドを表示、更新がある場合はボタンが「vX に更新」とハイライト；平文 HTTP 下でのコピーボタンの不具合を修正 |
| 2026-07-25 | v0.1.1 - 🛠 パネルとアカウントインポートの修正：モデルテストが未作成時にマスター API キーへフォールバック；一括インポートを 1 件ずつの「疎通検証 + 重複排除」に変更；大量リストで一括インポートが失敗する不具合を修正；ユーザーパネル/全ページの favicon + 128x128 ロゴと各 README のバージョンバッジ；クロスコンパイルのマルチアーキテクチャイメージビルド |
| 2026-07-25 | v0.1.0 - 🚀 初回リリース：4 プロトコルフロントエンド（Anthropic ハブ + OpenAI / OpenAI-Responses / Gemini）、Kiro アカウントプール（複数アカウントローテーション / 段階的クールダウン / トークン自己修復）、エンドポイントフォールバックとアカウント間リトライ、統一認証ゲート、`/admin` 管理パネルと `/user` ユーザーパネル、日次/アカウント別使用量統計、失敗/スロットルログ、アカウント残高キャッシュ、リアルタイムログ（SSE）、3 種類の対話型ログインフロー、Docker マルチアーキテクチャ（amd64/arm64）配布と CI |

---

## 🌟 主な機能

> 📖 詳細な使用ガイド：[USAGE.md](USAGE.md)

### 🔌 4 プロトコルフロントエンド、1 つのバックエンド

- 単一サービスで **OpenAI Chat**、**Anthropic Messages**、**OpenAI Responses**、**Gemini ネイティブ** の 4 種類の SDK 形式を同時提供
- 内部では **Anthropic Messages をハブ母形式**とし、その他のプロトコルは双方向変換後に同一の中継カーネルを再利用
- 各プロトコルとも**ストリーミング（SSE）**、**関数呼び出し（ツール）の真の透過**、**画像入力（マルチモーダル）**に対応
- **デュアルプレフィックスマウント**：各プロトコルは標準ベアプレフィックスと明示的なベンダープレフィックス（`/openai/v1`、`/claude/v1`、`/gemini/v1beta`）を同時に公開し、主要 SDK は `base_url` を設定するだけでそのまま利用可能

### 🔐 統一認証ゲート

- 3 択：`Authorization: Bearer` / `x-api-key` / `?token=`、定数時間比較、失敗時は即座に `401`
- `adminApiKey`（未設定時は `apiKey` にフォールバック）が `/api/admin/*` を保護、両方とも未設定ならこのゲートはオープンモード；保有者は自身の **API-KEY** で `/api/user/*` にアクセス
- `/health`、`/v1/ping` などの疎通確認エンドポイントは認証不要

### 🔄 アカウントプールとトークン自己修復

- **複数アカウントローテーション**：`priority`（等重みローテーション、既定）と `balanced`（`weight` による加重）の 2 戦略、管理パネルで実行中に切り替え可能
- アカウントごとに独立した RPM 制限と段階的クールダウン；連続失敗はカテゴリ別（永久失効 / 曖昧な認証 / クォータ / 一時的）に差異化して処理
- トークン期限切れ時に**自動メモリ内リフレッシュ**（シングルフライト協調で並行リフレッシュによる 401 カスケードを回避）、リフレッシュ成功時は `credentials.json` にアトミック書き込み
- Builder ID デバイスコード / IAM SSO 認可コード / 社交トークンの 3 種類のログインフローに対応、認証情報は既存の Kiro データをドロップインで利用可能

### 🔀 エンドポイントフォールバックとアカウント間リトライ

- Kiro IDE → CodeWhisperer → AmazonQ の複数エンドポイントを順にフォールバック、`429`/ネットワークエラー時に自動切り替え
- アカウントレベルの失敗は自動でアカウント間リトライ；確定的なリクエストエラー（サポートされていないモデル `INVALID_MODEL_ID` など）は**無闇にリトライせず、アカウントを誤って傷つけない**、アップストリームの理由をそのままクライアントに返す
- body-aware 失敗分類：真の認証情報失効のみを永久無効化、クォータ/リスク管理/レート制限はすべてクールダウンで自己修復

### 🖥 Web 管理パネル

- 内蔵の静的管理コンソール（`/admin`）、`adminApiKey` でログイン、`/api/admin/*` の豊富な API で駆動
- **ダッシュボード**：稼働時間のリアルタイムカウンター、全体の残余ポイント、システム情報（バージョン/Rust/OS/メモリ/CPU/PID/実行モード）、スポンサー QR コードカード（リモート設定をリアルタイム取得）、**更新チェック**（GitHub Release との比較、ダイアログで現在の UI 言語のリリースノート + アップグレードコマンドを表示）
- **アカウント管理**：CRUD、3 種類の対話型ログイン、一括インポート（1 件ずつ疎通検証 + 重複排除）、優先度/重み、残高照会
- **モデルテスト**：パネルから任意のモデルへテストリクエストを送信して疎通を確認；カスタム key が未作成の場合はマスター API キーへフォールバック
- **API-KEY 管理**：発行/無効化/ラベル変更、key 別の使用量とページ分割記録；key ごとの消費上限は 4 つのプロトコルフロントエンド（Anthropic / OpenAI / OpenAI-Responses / Gemini）すべてで有効
- **使用量統計**：日次/アカウント別、クライアント IP とアカウントラベルを含む、日単位のドリルダウン
- **リアルタイムログ**：構造化テーブル + 方向フィルター + 検索 + ページネーション + SSE リアルタイムプッシュ + ダウンロード
- **設定**：実行中の負荷分散/認証キー切り替え、統合例（プロトコル×言語のコピー可能なスニペット）、**ワンクリックサービス再起動**
- 上部コントロールバー：稼働状態バッジ、GitHub、再起動、ダーク/ライトテーマ、5 言語切り替え

### 👤 ユーザーパネル

- 内蔵のユーザーコンソール（`/user`）、保有者が自身の **API-KEY** でログイン（admin 権限不要）
- その key の割り当て・累計使用量・ページ分割記録を確認、`/api/user/*` で駆動

### 🧭 モデル名マッピング

- クライアントが渡すモデル名を**小文字部分文字列**マッチで Kiro 内部モデルに対応（マッチしない場合 → `400`）
- `/models` エンドポイントは本サービスが実際に提供可能なモデル id を返す、クライアントは list-then-use を推奨

### ⚡ 高性能アーキテクチャ

- **Rust + axum 0.8 + tokio** ベース、全経路が非同期ノンブロッキング
- AWS eventstream フレームデコード、アカウントプールはロック保持のクリティカルセクションを最小化、ネットワーク送出後即座に解放
- 強型 serde 検証、各プロトコルごとに独立したアダプターモジュール
- マルチステージ Docker ビルド、非 root 実行（gosu）、マルチアーキテクチャイメージ、ヘルスチェック

---

## 📋 システム要件

| 依存関係 | バージョン | 説明 |
|---------|-----------|------|
| Rust | 2024 edition | ソースからビルドする場合のみ必要；Docker デプロイではローカルインストール不要 |
| Docker | 20.10+ | Docker デプロイ推奨 |
| Kiro アカウント | — | 有効な Kiro（CodeWhisperer）認証情報が必要（Builder ID / IdC / 社交ログイン） |
| アーキテクチャ | amd64 / arm64 | 公式イメージはマルチアーキテクチャ、自動的にマッチ |

> [!TIP]
> Docker デプロイではローカル Rust 環境のインストール不要、Docker と有効な Kiro 認証情報があれば十分です。

---

## ⚡ クイックデプロイ

> 📖 詳細なデプロイガイド：[DEPLOY.md](DEPLOY.md)

> **前提条件**：有効な Kiro（CodeWhisperer）アカウント認証情報が必要です。

### 1. Kiro 認証情報を取得

Kiro クライアント / 既存の Kiro 認証情報から以下のフィールドをエクスポートするか、管理パネルの 3 種類の対話型ログイン（Builder ID デバイスコード / IAM SSO 認可コード / 社交トークン）でその場で取得します：

| フィールド | 説明 |
|-----------|------|
| `accessToken` / `refreshToken` | アクセストークンとリフレッシュトークン（期限切れ時に自動リフレッシュ） |
| `expiresAt` | トークンの有効期限（RFC3339） |
| `authMethod` | `social`（`profileArn` 付き）または `idc`（`clientId`/`clientSecret` 付き） |

### 2. Docker デプロイ

```bash
# リポジトリをクローン
git clone https://github.com/xwteam/kiro2api.git
cd kiro2api

# 環境ファイルを作成
cp .env.example .env
```

`.env` を編集し、少なくとも 1 つの外部呼び出しキー `API_KEY` を設定：

```env
API_KEY=sk-あなたの外部呼び出しキー
# 管理端の独立キー。公開デプロイでは必須（未設定なら /api/admin/* は API_KEY にフォールバック、両方とも未設定なら無認証）。
# 不要なら行ごとコメントアウトしてください——空値で書くと config.json に設定済みのキーを上書きします。
ADMIN_API_KEY=sk-あなたの管理端キー
```

Kiro アカウント認証情報を `data/credentials.json` に配置（配列、既存の Kiro 認証情報をそのままドロップイン可能）：

```json
[
  {
    "id": 12345,
    "accessToken": "...",
    "refreshToken": "...",
    "expiresAt": "2026-07-25T12:00:00Z",
    "authMethod": "social",
    "profileArn": "arn:aws:codewhisperer:us-east-1:...:profile/...",
    "machineId": "..."
  }
]
```

サービスを起動：

```bash
mkdir -p data
docker compose up -d
```

ログを確認して起動成功を確認：

```bash
docker compose logs -f
# アカウントプール準備完了、ポート待受のログが表示されれば起動成功
```

### 3. 検証

```bash
# ヘルスチェック
curl http://localhost:8080/health
# {"service":"kiro2api","status":"ok","version":"0.1.0"}

# 利用可能なモデルを表示
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-あなたのAPIキー"

# テストリクエストを送信
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのAPIキー" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"こんにちは"}]}'
```

AI の応答テキストが表示されればデプロイ成功。401 が返された場合、API キーを確認してください。

---

## 🧪 統合例

> [!NOTE]
> すべての API リクエストには API キーが必要です。2 つの認証方法をサポート：
> - `Authorization: Bearer sk-xxx`（推奨、OpenAI/Anthropic SDK 互換）
> - `x-api-key: sk-xxx`
>
> base URL は**標準ベアプレフィックス**を使用：OpenAI = `{host}/v1`、Anthropic = `{host}`（SDK が自動的に `/v1/messages` を補完）、Gemini = `{host}/v1beta`。明示的なベンダープレフィックス `/openai/v1`、`/claude/v1`、`/gemini/v1beta` も使用可能。

<details>
<summary><b>OpenAI SDK（Python）</b></summary>

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://localhost:8080/v1",
    api_key="sk-あなたのAPIキー",
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
    api_key="sk-あなたのAPIキー",
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
    api_key="sk-あなたのAPIキー",
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
# ストリーミングなしリクエスト
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのAPIキー" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"Hi"}]}'

# ストリーミングリクエスト
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのAPIキー" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"Hi"}],"stream":true}'
```

</details>

<details>
<summary><b>関数呼び出し（ツール）</b></summary>

```python
resp = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "北京の今日の天気は"}],
    tools=[{
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "指定した都市の天気を取得",
            "parameters": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }
        }
    }]
)
```

> ツール呼び出しは 4 種類のプロトコル間で**真に透過**されます（Anthropic `tool_use` / OpenAI `tool_calls` / Gemini `functionCall`）、模倣は行いません。

</details>

---

## 📡 API エンドポイント

> 📖 詳細 API ドキュメント：[API.md](API.md)

<details>
<summary><b>クリックして完全なエンドポイント一覧を展開</b></summary>

> **デュアルプレフィックス並存**：各プロトコルは「標準ベアパス」と「明示的なベンダープレフィックスパス」を同時に提供します。ベアパスは公式 SDK が `base_url` を設定する際にサフィックス不要でそのまま利用でき、ベンダープレフィックスは 4 社を明確に区別するために使用します。

### OpenAI 互換（`/v1` または `/openai/v1`）

| メソッド | エンドポイント | 機能 |
|---------|--------------|------|
| GET | `/models` | 利用可能モデルリスト |
| POST | `/chat/completions` | チャット補完（ストリーミングで `chat.completion.chunk` + `[DONE]` を返却、ツール/画像を含む） |

### OpenAI Responses（`/v1/responses` または `/openai/v1/responses`）

| メソッド | エンドポイント | 機能 |
|---------|--------------|------|
| POST | `/responses` | Responses API（ストリーミングは名前付きイベント + 単調増加 `sequence_number`、`[DONE]` なし；`previous_response_id` は 400 を返却） |

### Anthropic 互換（`/v1` 対話入口；`/claude/v1` 明示プレフィックス）

| メソッド | エンドポイント | 機能 |
|---------|--------------|------|
| POST | `/v1/messages` | Messages（ストリーミング/ツール/画像） |
| POST | `/v1/messages/count_tokens` | トークン数推定 |
| GET | `/claude/v1/models` | モデルリスト（Anthropic 形状、OpenAI `/v1/models` との衝突を回避） |
| POST | `/claude/v1/messages` · `.../count_tokens` | 明示プレフィックスの変体 |

### Gemini ネイティブ（`/v1beta` または `/gemini/v1beta`）

| メソッド | エンドポイント | 機能 |
|---------|--------------|------|
| GET | `/models` | モデルリスト |
| POST | `/models/{m}:generateContent` | コンテンツ生成（非ストリーミング） |
| POST | `/models/{m}:streamGenerateContent` | ストリーミング生成（`?alt=sse`、camelCase） |

### 管理 / ユーザー / 運用

| メソッド | エンドポイント | 機能 |
|---------|--------------|------|
| GET | `/admin` · `/api/admin/*` | 管理パネル + 管理インターフェース（`adminApiKey` 認証、キーを一つも設定していなければオープン：認証情報 CRUD / ログイン / API-KEY / 使用量 / ログ / 残高 / 設定 / 更新チェック / 再起動） |
| GET | `/user` · `/api/user/*` | ユーザーパネル + インターフェース（自身の API-KEY 認証） |
| GET | `/health` · `/v1/ping` | 疎通確認（認証不要） |

</details>

> URL 内の `localhost:8080` は例に過ぎません；ポートは `PORT`/`config.json` で設定、あなたのデプロイに合わせて置き換えてください。
>
> Gemini/OpenAI クライアントは一律に本サービスの**統一認証**（Bearer/`x-api-key`/`?token=`）を使用し、ベンダーネイティブの `?key=`/`x-goog-api-key` は使用しません。

---

## ⚙ 設定

優先順位：**コマンドライン引数 > 環境変数 > `config.json` > 組み込み既定**。コマンドライン引数は 2 つだけです：`-c/--config`（設定ファイルのパス）と `--credentials`（認証情報ファイルのパス。省略時は `CREDENTIALS_PATH`/`config.json`/既定値で決まります）。マウントボリューム `./data` に `config.json`、`credentials.json`、ログと実行状態を格納します。

> 認証情報のパスは、使用量統計（`stats/`）・API-KEY ストア（`api_keys.json`）・残高キャッシュの保存先ディレクトリも決めます——いずれも `credentials.json` の親ディレクトリを使うためです。組み込み既定の認証情報パスは `-c` で渡した設定ファイルのあるディレクトリを基準に解決され、コンテナは `-c /app/data/config.json` で起動するので、既定ではこれらもマウントボリュームに落ちます。パスを変更する場合はマウントボリューム内を指すようにしてください。さもないとコンテナを作り直した時点で消えます。

**環境変数**（`.env.example` 参照）：

| 変数 | 必須 | デフォルト | 説明 |
|------|------|----------|------|
| `API_KEY` | ✅ | — | 外部呼び出しキー（空の場合はプロトコルエンドポイントがオープンアクセス、起動時に警告） |
| `ADMIN_API_KEY` | ❌ | `API_KEY` にフォールバック | 管理端の独立認証キー；`API_KEY` ともども未設定だと `/api/admin/*` はオープン、公開デプロイでは必須 |
| `HOST` | ❌ | `127.0.0.1`（イメージは `0.0.0.0` 組み込み） | 待受アドレス |
| `PORT` | ❌ | `8080` | サービスポート（compose のポートマッピングとヘルスチェックもこの値に追従） |
| `REGION` | ❌ | `us-east-1` | 既定 AWS region（アカウント `profileArn` 内の region が優先） |
| `LOAD_BALANCING_MODE` | ❌ | `priority` | 負荷分散：`priority`（等重みローテーション）/ `balanced`（weight による加重） |
| `MAX_RPM_PER_CREDENTIAL` | ❌ | `0` | アカウントあたり毎分のリクエスト上限、`0` = 無制限 |
| `CREDENTIALS_PATH` | ❌ | `credentials.json`（`-c` の設定ファイルと同じディレクトリを基準に解決；コンテナでは `/app/data/credentials.json`） | 認証情報ファイルのパス；コマンドラインの `--credentials` が優先 |

**`data/config.json`**（camelCase、すべて任意；`logCapacity` はここでのみ設定）：

```json
{
  "host": "0.0.0.0",
  "port": 8080,
  "region": "us-east-1",
  "apiKey": "sk-あなたの外部呼び出しキー",
  "adminApiKey": "任意,管理端",
  "credentialsPath": "/app/data/credentials.json",
  "loadBalancingMode": "priority",
  "maxRpmPerCredential": 0,
  "logCapacity": 5000,
  "kiroVersion": "0.11.107",
  "systemVersion": "win32#10.0.22631",
  "nodeVersion": "22.22.0"
}
```

- `logCapacity`：リアルタイムログのリングバッファ件数、`>0` でログキャプチャを有効化（管理パネルのログページで再生/SSE）、`0` で無効化（ログエンドポイントは 503 を返却）；既定 `5000`。
- `kiroVersion`/`systemVersion`/`nodeVersion`：偽装 UA のバージョン番号、設定から注入。

---

## ⚠ 重要な注意事項

1. **外部デプロイでは必ず `API_KEY` と `ADMIN_API_KEY` を設定**：`API_KEY` が空の場合はプロトコルエンドポイントがオープンアクセスになります（起動時に警告）；`adminApiKey`/`apiKey` のどちらも未設定なら `/api/admin/*` も同様にオープンで、認証情報・API-KEY・認証設定を誰にでも書き換えられてしまいます。`/admin`・`/user` パネル本体は常に認証されません（本当のゲートはその `/api/**` インターフェース側にあります）；ベアメタルデプロイでは `HOST=0.0.0.0` の変更に注意してください。

2. **利用可能なモデルはアカウントのサブスクリプション階層に依存**：無料階層（KIRO FREE）は通常 `claude-sonnet-4.5` のみを許可；サポートされていないモデルを要求すると `400`（`INVALID_MODEL_ID`）を返し、無闇にリトライせず、アカウントを誤って傷つけません。

3. **トークン自己修復**：トークン期限切れ時に自動でメモリ内リフレッシュし `credentials.json` にアトミック書き込み；真の認証情報失効のみを永久無効化、クォータ/リスク管理/レート制限はすべてクールダウンで自己修復。

4. **ストリーミング出力**：4 種類のプロトコルすべてがストリーミングに対応；`stream:false` の場合、サービスは内部でイベントストリームをデコードし、収集完了後に完全な JSON を一括返却します。アップストリームのエラーやストリーム途中の伝送中断が起きた場合は、そのプロトコル規範のエラーイベント（Anthropic `error` / OpenAI エラー chunk（`[DONE]` は付けない）/ Responses `response.failed` / Gemini エラーブロック）でストリームを終端し、**正常完了として報告することはありません**；`max_tokens` 到達やコンテキスト枯渇による切り詰めも、非ストリーミングと同じ切り詰め理由（`max_tokens` / `length` / `MAX_TOKENS` / `incomplete`）で報告します。

5. **ネットワーク環境**：デプロイサーバーは AWS CodeWhisperer/Kiro エンドポイント（`*.amazonaws.com`）にアクセスできる必要があります。

---

## 🗂 プロジェクト構成

```
kiro2api/
├── src/
│   ├── main.rs / cli.rs / lib.rs   # エントリ、CLI、ライブラリルート
│   ├── config.rs                   # 設定（env > config.json > 既定）
│   ├── http.rs                     # 送出 HTTP クライアント（タイムアウト上限）
│   ├── logcap.rs                   # リアルタイムログのリングバッファ + SSE ブロードキャスト
│   ├── server/                     # axum ルート組み立て、統一認証ゲート
│   ├── protocol/                   # 4 プロトコルアダプター
│   │   ├── anthropic/              #   Anthropic Messages（ハブ母形式 + relay カーネル）
│   │   ├── openai/                 #   OpenAI Chat Completions
│   │   ├── responses/              #   OpenAI Responses
│   │   └── gemini/                 #   Gemini ネイティブ v1beta
│   ├── kiro/                       # Kiro データプレーン
│   │   ├── pool.rs                 #   アカウントプール（負荷分散 + 失敗分類 + クールダウン）
│   │   ├── provider.rs             #   アップストリーム送出 + エンドポイントフォールバック
│   │   ├── convert.rs              #   モデルマッピング + リクエスト/レスポンス変換
│   │   ├── ensure_fresh.rs / refresh.rs  # トークンのシングルフライトリフレッシュ
│   │   ├── eventstream/            #   AWS eventstream フレームデコード
│   │   └── login/                  #   Builder ID / IAM SSO / 社交ログインフロー
│   ├── admin/                      # /api/admin/* 管理インターフェース
│   ├── user/                       # /api/user/* ユーザーインターフェース
│   ├── apikey/                     # API-KEY 保存と検証
│   ├── balance/                    # 残高キャッシュ（TTL）
│   ├── stats/                      # 使用量/失敗/レート制限統計 + 料金
│   ├── models_cache/               # 動的モデルリストキャッシュ
│   └── webui/                      # rust-embed 静的パネルサービス（admin-ui-v2/、user-ui/dist）
├── admin-ui-v2/                    # 静的管理パネル（HTML/CSS/JS、コンパイル時に埋め込み）
├── user-ui/                        # ユーザーパネル（ビルド成果物を埋め込み）
├── data/                           # 永続化データ（Docker ボリュームマウント）
│   ├── config.json                 #   実行設定
│   └── credentials.json            #   Kiro アカウント認証情報
├── docs/                           # 5 言語ドキュメント（README/USAGE/DEPLOY/API/SPONSORS）
├── Dockerfile                      # マルチステージビルド（マルチアーキテクチャ、非 root）
├── docker-compose.yml              # オーケストレーション設定
├── Cargo.toml / Cargo.lock
└── .env.example
```

---

## 🗺 ロードマップ

- [x] 4 プロトコルフロントエンド（OpenAI / Anthropic / OpenAI-Responses / Gemini）
- [x] Anthropic Messages ハブ母形式 + 統一中継カーネル
- [x] ストリーミング（SSE）+ 関数呼び出しの真の透過 + 画像マルチモーダル
- [x] Kiro アカウントプール（複数アカウントローテーション、段階的クールダウン、負荷分散）
- [x] トークンのシングルフライト自動リフレッシュ + アトミック書き込み
- [x] エンドポイントフォールバック（Kiro/CodeWhisperer/AmazonQ）+ アカウント間リトライ
- [x] body-aware 失敗分類（永久失効のみ無効化、その他はクールダウンで自己修復）
- [x] 統一認証ゲート（Bearer / x-api-key / ?token=）
- [x] Web 管理パネル（認証情報/ログイン/API-KEY/使用量/ログ/残高/設定）
- [x] ユーザーパネル（保有者が自身の API-KEY でログイン）
- [x] 3 種類の対話型ログインフロー（Builder ID / IAM SSO / 社交トークン）
- [x] 日次/アカウント別使用量統計（クライアント IP とアカウントラベルを含む）
- [x] リアルタイムログ（SSE）+ 残高キャッシュ + 動的モデルリスト
- [x] 統合例（プロトコル×言語のコピー可能なスニペット）
- [x] サービス再起動 + バージョン更新チェック（GitHub Release との比較）
- [x] Docker マルチアーキテクチャ（amd64/arm64）配布 + CI
- [ ] `/admin`・`/user` パネル本体の認証
- [ ] GitHub Actions 自動ビルドとイメージ公開

---

## ☕ サポート & 貢献

役に立ちましたか？作者にコーヒーをおごるか、WeChat グループに参加してサポートを受けてください。QR コードは管理パネルのダッシュボードにあります。詳細は [SPONSORS.md](SPONSORS.md) をご覧ください。

kiro2api は主に個人によってメンテナンスされています。コード、ドキュメント、修正、PR による参加を歓迎します。

**貢献の手順：**

1. このリポジトリをフォーク
2. ブランチを作成 `git checkout -b feature/your-feature`
3. コードをコミット `git commit -m "feat: add something"`
4. プッシュして Pull Request を作成

---

## 🙏 謝辞

[Issues](https://github.com/xwteam/kiro2api/issues) でバグの再現、ログ、互換性フィードバック、機能提案を提出してくださったすべてのユーザーに感謝します。これらのフィードバックが、アカウントプール、トークン自己修復、エンドポイントフォールバック、マルチプロトコル互換、Web パネルなどのコア機能の反復を直接推進しました。

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

## 📄 ライセンス

このプロジェクトは [MIT ライセンス](../../LICENSE) を使用しています：

- **許可**：個人学習、研究、自己ホスト型デプロイ、二次開発
- **要求**：著作権とライセンス表示の保持

このプロジェクトは Amazon / AWS / Kiro と無関係です。ユーザーはすべてのリスクを負い、関連する利用規約に準拠する必要があります。

---

<div align="center">
  <sub>Built with Rust + axum + tokio | Powered by Kiro (CodeWhisperer)</sub>
</div>
