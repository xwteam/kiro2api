# デプロイガイド

kiro2api を本番環境にデプロイするための完全なガイドです。

## 環境要件

| 要件 | 最小値 | 推奨値 | 説明 |
|------|--------|--------|------|
| Docker | 20.10+ | 最新版 | コンテナ化デプロイ用 |
| メモリ | 256MB | 1GB+ | 複数アカウント運用時は 1GB 以上推奨 |
| ディスク | 100MB | 500MB+ | ログ・認証情報・実行時データ保存用 |
| OS | Linux/Mac/Windows | Linux | Docker Desktop で Windows/Mac 対応 |
| Rust | 2024 edition | 2024 edition | ソースからビルドする場合のみ必要。Docker 使用時は不要 |
| アーキテクチャ | amd64 / arm64 | — | 公式イメージはマルチアーキテクチャ、自動で一致 |
| ネットワーク | — | — | AWS CodeWhisperer/Kiro エンドポイント（`*.amazonaws.com`）への直接アクセス必須 |

## Kiro 認証情報の取得

kiro2api を動作させるには、Kiro（CodeWhisperer）アカウントの認証情報が必要です。後端は Kiro アカウントプールで、統一して Claude 系モデルを提供します。以下のいずれかの方法で取得してください。

### 方法 1: 既存の Kiro 認証情報を流用

お使いの Kiro クライアントや既存の Kiro 認証情報から、以下のフィールドをエクスポートして `data/credentials.json`（配列）にそのまま drop-in できます。

| フィールド | 説明 | 例 |
|-----------|------|-----|
| `id` | アカウント識別子 | `12345` |
| `accessToken` / `refreshToken` | アクセストークンとリフレッシュトークン（期限切れ時に自動リフレッシュ） | `...` |
| `expiresAt` | トークン有効期限（RFC3339 形式） | `2026-07-25T12:00:00Z` |
| `authMethod` | `social`（`profileArn` を伴う）または `idc`（`clientId`/`clientSecret` を伴う） | `social` |
| `profileArn` | `social` の場合の CodeWhisperer プロファイル ARN | `arn:aws:codewhisperer:us-east-1:...:profile/...` |
| `machineId` | マシン識別子（任意） | `...` |

> **ヒント**: `expiresAt` は RFC3339 形式です。`region` の既定値は `us-east-1`、`disabled:true` で該当アカウントをプールから除外できます。

> **注意**: トークンの期限切れはサービス側がメモリ内で自動的にリフレッシュし、成功時にアトミックに `credentials.json` へ書き戻します（single-flight 協調で、並行リフレッシュによる 401 の連鎖を回避）。

### 方法 2: 管理パネルの対話型ログイン

管理パネル（`/admin`）から、3 種類の対話型ログインフローでその場で認証情報を取得できます。

| ログインフロー | 説明 |
|---------------|------|
| Builder ID | デバイスコード方式でのログイン |
| IAM SSO | IAM Identity Center 認可コード方式 |
| 社交令牌 | ソーシャルトークンでのログイン |

## Docker デプロイ

### ステップ 1: リポジトリをクローン

```bash
git clone https://github.com/xwteam/kiro2api.git
cd kiro2api
```

### ステップ 2: 環境変数ファイルを作成

```bash
cp .env.example .env
```

### ステップ 3: .env ファイルを編集

テキストエディタで `.env` を開き、少なくとも対外呼び出し用の `API_KEY` を設定します。

```env
API_KEY=sk-あなたの対外呼び出しキー
# 管理端の独立認証キー。公開デプロイでは必須（未設定だと /api/admin/* は API_KEY で照合され、
# API_KEY も未設定なら無認証になります）。
# 不要なら行ごとコメントアウトしてください（空値で書いた場合も未設定と同じ扱いで、
# config.json 側の管理キーはそのまま残ります）。
ADMIN_API_KEY=sk-管理端専用の独立キー
HOST=0.0.0.0
# サービスポート。compose のポートマッピングとヘルスチェックもこの値に追従します
PORT=8080
REGION=us-east-1
LOAD_BALANCING_MODE=priority
MAX_RPM_PER_CREDENTIAL=0
# 任意：認証情報ファイルのパス。イメージはこの変数を設定しません。組み込み既定は
# `-c` で指定した設定ファイルと同じディレクトリに解決されるため、コンテナでは
# そのまま /app/data/credentials.json になります（通常は指定不要）。
# CREDENTIALS_PATH=/app/data/credentials.json
```

**重要な設定項目：**

| 変数 | 説明 | デフォルト |
|------|------|-----------|
| `API_KEY` | 対外呼び出しキー（空白かつ管理パネルで API-KEY を 1 件も作成していない間だけプロトコルエンドポイントが開放され、起動時に警告） | 必須 |
| `ADMIN_API_KEY` | 管理端の独立認証キー（行ごと省略・空値のどちらも未設定扱いで `API_KEY` にフォールバック。`API_KEY` ともども未設定だと `/api/admin/*` は無認証） | — |
| `HOST` | リッスンアドレス（コンテナイメージには `0.0.0.0` を内蔵） | `127.0.0.1` |
| `PORT` | サービスポート（compose のポートマッピングとヘルスチェックもこの値に追従） | 8080 |
| `REGION` | 既定の AWS リージョン（アカウントの `profileArn` 内リージョンが優先） | us-east-1 |
| `LOAD_BALANCING_MODE` | 負荷分散：`priority`（均等ローテーション）/ `balanced`（`weight` による重み付け） | priority |
| `MAX_RPM_PER_CREDENTIAL` | アカウント当たりの毎分リクエスト上限、`0` = 無制限 | 0 |
| `CREDENTIALS_PATH` | 認証情報ファイルのパス。使用量統計・`api_keys.json`・残高キャッシュの保存先（このファイルの親ディレクトリ）も決めるため、必ずマウントボリューム内を指すこと | `credentials.json`（`-c` の設定ファイルと同じディレクトリを基準に解決。コンテナでは `/app/data/credentials.json`。イメージは `CREDENTIALS_PATH` を設定しないため、`config.json` の `credentialsPath` も有効なままです） |

> **注意**: 値に引用符は不要です。余分なスペースや改行がないことを確認してください。`logCapacity` は `config.json` でのみ設定します。
>
> **キーは必ず設定**: `API_KEY` が空で、かつ管理パネルで API-KEY を 1 件も作成していない間はプロトコルエンドポイントが開放されます（API-KEY を 1 件でも作れば、以降は有効な API-KEY が必要になります）。さらに `ADMIN_API_KEY`・`API_KEY` の両方が未設定だと `/api/admin/*` も無認証で開放され、認証情報・API-KEY・認証設定を誰でも書き換えられます。管理ゲートは API-KEY を作っても閉じません（管理者級のキーを設定して初めて閉じます）。公開デプロイでは `ADMIN_API_KEY` の設定が必須です。
>
> **空値は無視される**: `API_KEY=`・`ADMIN_API_KEY=` のような空値（空白のみも同様）は読み込み時に捨てられ、`config.json` やパネルで設定済みのキーは**上書きされません**。環境変数で値を変えたいときは必ず非空の値を書いてください。

### ステップ 4: 認証情報を配置

Kiro アカウント認証情報を `data/credentials.json`（配列、既存の Kiro 認証情報をそのまま drop-in 可能）に配置します。

```bash
mkdir -p data
```

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

### ステップ 5: サービスを起動

```bash
docker compose up -d
```

イメージはマルチアーキテクチャ（amd64/arm64）で、コンテナ内では非 root ユーザーで実行されます。`docker-entrypoint.sh` がまず root でマウントボリュームの所有者を `chown` で修正し、その後 `gosu` で権限を降格します（レガシーな root 作成の data からもシームレスにアップグレード可能）。

### ステップ 6: ログを確認

```bash
docker compose logs -f
```

起動成功の確認（認証情報の読み込みとリッスン開始の 2 行が出れば成功）。サーバーのログメッセージ本体は中国語で出力されます（先頭のタイムスタンプは省略）：

```
INFO kiro2api::server: 已载入账号凭据 path=/app/data/credentials.json accounts=3
INFO kiro2api::server: kiro2api listening on 0.0.0.0:8080
```

`API_KEY` が空の場合、起動時に次の警告が出ます（API-KEY を 1 件も作成していない間、プロトコルエンドポイントは開放されます）。

```
WARN kiro2api::server: 未设置 api_key:在未创建任何 API-KEY 前,四条协议端点(Anthropic/OpenAI/Responses/Gemini)开放访问
```

この場合は `.env` に `API_KEY` を設定し、`docker compose restart` を実行してください。

## マルチアカウント設定

複数の Kiro アカウントを使用して負荷分散を実現できます。`data/credentials.json` の配列に複数のアカウントを列挙するだけです。

### credentials.json の作成

`data/credentials.json` にアカウントの配列を記述します。

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
  },
  {
    "id": 67890,
    "accessToken": "...",
    "refreshToken": "...",
    "expiresAt": "2026-07-25T12:00:00Z",
    "authMethod": "idc",
    "clientId": "...",
    "clientSecret": "..."
  }
]
```

### 設定項目

| フィールド | 説明 | 必須 |
|-----------|------|------|
| `id` | アカウント識別子（ユニーク） | ✅ |
| `accessToken` / `refreshToken` | アクセス／リフレッシュトークン | ✅ |
| `expiresAt` | トークン有効期限（RFC3339） | ✅ |
| `authMethod` | `social` または `idc` | ✅ |
| `profileArn` | `social` 時のプロファイル ARN | ❌ |
| `disabled` | `true` でプールから除外 | ❌ |

### 負荷分散の挙動

- **多アカウントローテーション**：`priority`（均等ローテーション、既定）と `balanced`（`weight` による重み付け）の 2 戦略。管理パネルから実行時に切り替え可能。
- 各アカウント独立の RPM 制限、分級クールダウン。連続失敗はカテゴリ別（永久失効 / 曖昧な認証 / 配额 / 一時的）に差別化して処置。
- **端点回退**：Kiro IDE → CodeWhisperer → AmazonQ の順にエンドポイントを回退し、`429`／ネットワークエラー時に自動切り替え。アカウント級の失敗はクロスアカウントで自動リトライ。

### 起動

```bash
docker compose up -d
```

サービスは自動的に `credentials.json` を読み込み、複数アカウントで負荷分散を開始します。

> **ヒント**: 確定的なリクエストエラー（サポートされないモデルの `INVALID_MODEL_ID` など）は**無闇にリトライせず、アカウントを誤って傷つけません**。上流の原因をそのままクライアントに返します。

## 検証

### ヘルスチェック

```bash
curl http://localhost:8080/health
```

期待される応答：

```json
{"service":"kiro2api","status":"ok","version":"0.16.0"}
```

### モデル一覧の確認

```bash
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-あなたのキー"
```

### テストリクエスト

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-あなたのキー" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "こんにちは"}]
  }'
```

AI からの応答が返ってくれば、デプロイは成功です。

## トラブルシューティング

### 認証エラー（401）

**症状**: `{"error": "Unauthorized"}`

**原因**: API Key が正しくない、または認証ヘッダーが不正

**解決方法**:

1. 受理される 6 チャネルのいずれか 1 つで渡す（ゲートは最初に見つかった 1 つを採用。優先順位は下記の並び順）：

```bash
# 方法 1
curl -H "Authorization: Bearer sk-xxx"

# 方法 2
curl -H "x-api-key: sk-xxx"

# 方法 3（Gemini ネイティブヘッダー）
curl -H "x-goog-api-key: sk-xxx"

# 方法 4〜6（クエリパラメータ。ヘッダーを設定できないクライアント向け）
curl "http://localhost:8080/v1/models?api_key=sk-xxx"
curl "http://localhost:8080/v1/models?token=sk-xxx"
curl "http://localhost:8080/v1/models?key=sk-xxx"
```

2. `.env` の `API_KEY` が設定されているか確認。空白でも管理パネルで API-KEY を作成済みなら、その API-KEY による認証が必要です（API-KEY が 1 件も無いときだけプロトコルエンドポイントが開放されます）。

### サポートされないモデルエラー（400 / INVALID_MODEL_ID）

**症状**: `400`（`INVALID_MODEL_ID`）が返る

**原因**: リクエストしたモデルがアカウントのサブスクリプション階層で認可されていない。**利用可能なモデルはアカウントのサブスクリプション階層に依存します**。無料階層（KIRO FREE）は通常 `claude-sonnet-4.5` のみ認可されます。

**解決方法**:

1. より広いモデルカタログを確認（管理 API。上流の和集合、なければ静的な 17 件）：

```bash
curl http://localhost:8080/api/admin/models \
  -H "Authorization: Bearer sk-管理端のキー"
```

プロトコル側の `/v1/models` も使えますが、こちらは**固定 3 件**を返すだけで階層によるフィルタは掛かりません：

```bash
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-あなたのキー"
```

2. 一覧に載っていても階層が足りなければ `400` になります。opus/GPT 等はより高い階層が必要なので、実際に通るモデルは 1 回試して確かめてください。

> **注意**: この 400 は明示的なエラーであり、暗黙のうちに失敗するわけではありません。無闇にリトライせず、アカウントも誤って傷つけません。

### ポート競合エラー

**症状**: `Error response from daemon: Ports are not available`

**原因**: ポート 8080 が既に使用されている

**解決方法**:

`.env` でポートを変更：

```env
PORT=8081
```

再起動：

```bash
docker compose up -d
```

> **注意**: 変更は `PORT` の一箇所だけで完結します。アプリのリッスンポート、compose のポートマッピング（`${PORT:-8080}:${PORT:-8080}`）、ヘルスチェックの探測ポートがすべてこの値に追従するため、`docker-compose.yml` を書き換える必要はありません。ベアメタルでも同様で、`PORT` は `config.json` の `port` より優先されます。

### トークン期限切れ／認証情報の失効

**症状**: アカウントがクールダウンや無効化される

**原因**: トークンの期限切れ、または認証情報の失効

**解決方法**:

1. **トークンの期限切れ**は自動処理：サービス側がメモリ内で自動リフレッシュし、成功時にアトミックに `credentials.json` へ書き戻します。手動操作は不要です。
2. **本当の認証情報失効**の場合のみ該当アカウントが永久無効化されます。配额／风控／限流は一律クールダウンで自愈します。
3. 認証情報を再取得して `data/credentials.json` を更新後、再起動：

```bash
docker compose restart
```

### メモリ不足エラー

**症状**: コンテナが頻繁に再起動される

**原因**: メモリが不足している

**解決方法**:

1. **SWAP を追加**（Linux）:

```bash
# 2GB の SWAP ファイルを作成
sudo fallocate -l 2G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile

# 永続化
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

2. **アカウント数を削減**:

`data/credentials.json` のアカウント数を減らす、または不要なアカウントに `"disabled": true` を設定。

## 本番環境での推奨設定

```env
# セキュリティ
API_KEY=sk-生成された対外呼び出しキー
ADMIN_API_KEY=sk-管理端専用の独立キー

# パフォーマンス / 負荷分散
LOAD_BALANCING_MODE=balanced
MAX_RPM_PER_CREDENTIAL=60

# リッスン
HOST=0.0.0.0
PORT=8080
REGION=us-east-1
```

> **注意**: `ADMIN_API_KEY` を設定すると `/api/admin/*` を `API_KEY` から分離できます。外部公開時は必ず `adminApiKey`（少なくとも `apiKey`）を設定してください——**どちらも未設定だと `/api/admin/*` は誰でも叩ける状態**で、認証情報・API-KEY・認証設定を自由に書き換えられてしまいます。`/admin`・`/user` パネル本体には常に認証がなく、実際のゲートは `/api/admin/*`・`/api/user/*` の API 側にあります。

## Docker Compose の詳細設定

`docker-compose.yml` の主要な設定：

```yaml
services:
  kiro2api:
    image: ghcr.io/xwteam/kiro2api:latest
    container_name: kiro2api
    env_file:
      - .env
    environment:
      - TZ=Asia/Shanghai
    # ホスト側・コンテナ側とも .env の PORT に追従（未設定なら 8080）
    ports:
      - "${PORT:-8080}:${PORT:-8080}"
    volumes:
      - ./data:/app/data           # config.json / credentials.json / ログ / 実行時データ
      - /etc/localtime:/etc/localtime:ro
    healthcheck:
      # 探測ポートはアプリと同じ優先順位で解決：PORT 環境変数 > data/config.json の port > 8080
      test: ["CMD-SHELL", "P=\"$${PORT:-$$(grep -oE '\"port\"[[:space:]]*:[[:space:]]*[0-9]+' /app/data/config.json 2>/dev/null | grep -oE '[0-9]+' | head -1)}\"; wget -q -O /dev/null \"http://localhost:$${P:-8080}/health\" || exit 1"]
      interval: 30s
      timeout: 10s
      retries: 3
      start_period: 20s
    restart: unless-stopped
```

> **注意**: 永続ボリューム `./data` に `config.json`、`credentials.json`、ログ、実行時データを保存します。イメージには `HEALTHCHECK` と compose の healthcheck が組み込まれており（探測ポートは `PORT` 環境変数 > `data/config.json` の `port` > `8080` の順で解決され、アプリのリッスンポートと一致します）、`restart: unless-stopped` が設定されています。

## ログの確認

### リアルタイムログ

```bash
docker compose logs -f
```

### 特定のコンテナのログ

```bash
docker compose logs kiro2api
```

### 管理パネルの実時ログ（SSE）

`logCapacity`（`config.json` の環形バッファ行数、既定 `5000`）が `> 0` の場合、管理パネル（`/admin`）のログページで、構造化テーブル + 方向フィルタ + 検索 + ページング + SSE 実時プッシュ + ダウンロードが利用できます。`0` の場合はログ捕捉が無効になり、ログエンドポイントは `503` を返します。

## アップデート

新しいバージョンに更新する場合：

```bash
# 最新イメージを取得
docker compose pull

# サービスを再起動
docker compose up -d
```

> **ヒント**: マウントボリューム `./data` の所有者は entrypoint が自動的に修正します。管理パネルの設定ページから「更新チェック」（GitHub Release との比較）も利用できます。

### ソースからビルドする場合

```bash
# リポジトリを更新
git pull origin main

# イメージを再構築
docker compose build --no-cache

# サービスを再起動
docker compose up -d
```

## 停止・削除

### サービスを停止

```bash
docker compose stop
```

### サービスを削除

```bash
docker compose down
```

### ボリュームも削除

```bash
docker compose down -v
```

> **注意**: `-v` フラグはボリュームも削除します。`./data` 内の `config.json`・`credentials.json` を保持したい場合は使用しないでください。

---

より詳しい使い方は [USAGE](USAGE.md)、API 仕様は [API](API.md)、プロジェクト全体像はルート [README](../../README.md) を参照してください。

<div align="center">
  <sub>Built with Rust + axum + tokio | Powered by Kiro (CodeWhisperer)</sub>
</div>
