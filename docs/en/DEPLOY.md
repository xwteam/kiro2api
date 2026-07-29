# Deployment Guide

This guide covers deploying kiro2api using Docker, the recommended method for production environments.

## System Requirements

| Component | Minimum | Recommended |
|-----------|---------|-------------|
| Docker | 20.10+ | Latest stable |
| RAM | 512 MB | 2 GB+ |
| Disk | 500 MB | 2 GB+ |
| OS | Linux / macOS / Windows | Linux (best performance) |
| Architecture | amd64 or arm64 | Multi-arch image auto-matches |
| Network | Direct access to `*.amazonaws.com` (AWS CodeWhisperer / Kiro) | Stable connection |

> **Note:** Docker deployment does not require a local Rust toolchain. You only need Docker and a valid set of Kiro (CodeWhisperer) credentials. Rust 2024 edition is required only when building from source.

## Getting Credentials

kiro2api requires valid Kiro (CodeWhisperer) credentials to function. The backend is a Kiro account pool that serves Claude-family models. Follow these steps to obtain them:

### Step 1: Prepare a Kiro Account

1. Have an active Kiro (CodeWhisperer) subscription — Builder ID, IAM Identity Center (IdC), or a social login
2. Verify the account can reach the Kiro / CodeWhisperer service normally

> **Note:** Available models depend on your subscription tier. The free tier (KIRO FREE) usually only authorizes `claude-sonnet-4.5`; opus / GPT-class models require a higher tier. Requesting an unsupported model returns a clear `400` (`INVALID_MODEL_ID`) rather than failing silently.

### Step 2: Obtain the Credential Fields

Export the following fields from your Kiro client / existing Kiro credentials, or use one of the panel's three interactive login flows (Builder ID device code / IAM SSO authorization code / social token) to fetch them on the spot:

| Field | Description |
|-------|-------------|
| `accessToken` / `refreshToken` | Access token and refresh token (auto-refreshed on expiry) |
| `expiresAt` | Token expiry time (RFC3339) |
| `authMethod` | `social` (carries `profileArn`) or `idc` (carries `clientId`/`clientSecret`) |
| `profileArn` | CodeWhisperer profile ARN (`social` accounts) |
| `machineId` | Machine identifier used in the request signature |

> **Tip:** You can drop existing Kiro data straight into `credentials.json` — it accepts an array of accounts, so multiple credentials can be pooled together.

## Docker Deployment

### Quick Start (Single Account)

```bash
# Clone the repository
git clone https://github.com/xwteam/kiro2api.git
cd kiro2api

# Copy environment template
cp .env.example .env
```

Edit `.env` and set at least one external API key:

```env
API_KEY=sk-your-custom-key
ADMIN_API_KEY=sk-your-admin-key
```

**Important notes:**
- If `API_KEY` is empty, the protocol endpoints are **open access** (a warning is logged at startup) — always set it for externally facing deployments
- Omit the `ADMIN_API_KEY` line and `/api/admin/*` falls back to `API_KEY`; with **neither** key set the admin API is **open access** — anyone can add or delete credentials, rotate the auth keys, and restart the service, so always set `ADMIN_API_KEY` on a public deployment
- Never write an empty value: `API_KEY=` or `ADMIN_API_KEY=` overrides whatever is already configured in `config.json`. If you don't want a variable, comment the whole line out
- The container image ships with `HOST=0.0.0.0` built in; for bare-metal runs, do not casually set `HOST=0.0.0.0`
- The `/admin` and `/user` panels themselves are never gated — the auth gate sits on their `/api/**` endpoints

Place your Kiro credentials in `data/credentials.json` (an array; existing Kiro credentials can be dropped in directly):

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

Start the service:

```bash
mkdir -p data
docker compose up -d
```

Check logs to confirm startup:

```bash
docker compose logs -f
```

Look for these messages:
- Account pool ready + listening on the configured port — Service is ready
- A startup warning about an empty `apiKey` — protocol endpoints are open; set `API_KEY`

### Multi-Account Setup (Load Balancing)

For higher throughput and redundancy, pool multiple Kiro accounts in the same `data/credentials.json` array:

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

- `authMethod`: `social` carries `profileArn`; `idc` carries `clientId`/`clientSecret`
- `expiresAt` is RFC3339; `region` defaults to `us-east-1` (the region inside an account's `profileArn` takes precedence)
- `disabled: true` excludes that account from the pool

**Load Balancing Strategies:**
- `priority` (default): even round-robin across all active accounts
- `balanced`: weighted round-robin by each account's `weight`

Change strategy in `.env`:
```env
LOAD_BALANCING_MODE=balanced
```

You can also switch the strategy at runtime from the admin panel's **Settings** page without restarting.

### Dynamic Account Management

Manage accounts without restarting via the admin panel (`/admin`, authenticated with `adminApiKey`) or its `/api/admin/*` API:

- Add / remove / edit accounts and their priority / weight
- Run one of the three interactive login flows (Builder ID device code / IAM SSO authorization code / social token) to onboard a new account
- Bulk-import credentials
- Query per-account balances

New and updated credentials are written back atomically to `credentials.json`.

## Token Refresh

Access tokens expire periodically. kiro2api self-heals — you rarely need to touch tokens manually.

### Automatic Refresh (Self-Healing)

- When a token nears or reaches expiry, the service refreshes it **in memory** automatically, coordinated by a single-flight guard so concurrent requests do not trigger a cascade of parallel refreshes (avoiding 401 storms)
- On a successful refresh, the new token is atomically persisted back to `credentials.json`
- Failures are classified body-aware: only genuine credential invalidation permanently disables an account; quota / risk-control / rate-limit failures are simply cooled down and retried automatically

### Endpoint Fallback and Cross-Account Retry

- Requests fall back across endpoints in order: **Kiro IDE → CodeWhisperer → AmazonQ**, switching automatically on `429` / network errors
- Account-level failures trigger automatic cross-account retry
- Deterministic request errors (e.g. an unsupported model, `INVALID_MODEL_ID`) are **not blindly retried and do not penalize the account** — the upstream reason is returned to the client directly

### Manual Refresh via Admin Panel

If you want to force an account back to health or replace its credentials:

1. Open the admin panel at `http://localhost:8080/admin`
2. Log in with your `adminApiKey`
3. Go to **Account Management**
4. Edit the account, or re-run an interactive login to refresh its tokens

No restart required.

## Verification

### Health Check

```bash
curl http://localhost:8080/health
```

Expected response:
```json
{"service":"kiro2api","status":"ok","version":"0.7.10"}
```

### List Available Models

```bash
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-your-api-key"
```

### Test API Request

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

You should receive an AI response. If you get a 401 error, verify your API Key is correct.

## Troubleshooting

### Model Returns 400 (INVALID_MODEL_ID)

**Symptom:** A request fails with `400` and `INVALID_MODEL_ID`

**Solutions:**
1. Available models depend on the account's subscription tier — the free tier (KIRO FREE) usually only authorizes `claude-sonnet-4.5`
2. `/v1/models` will not tell you which ids work: it is a fixed, hard-coded three-entry list that is not derived from your pool. Use `GET /api/admin/models` for the real catalog, and fall back to `claude-sonnet-4.5`
3. This is a deterministic error: the service does **not** retry it and does not penalize the account

### Port Already in Use

**Symptom:** `Error: bind: address already in use`

**Solutions:**
```bash
# Find process using port 8080
lsof -i :8080

# Kill the process
kill -9 <PID>

# Or move the service to another port — set PORT in .env, then restart
# PORT=8081
```

> One change is enough: the listening port, the compose port mapping (`${PORT:-8080}:${PORT:-8080}`), and the healthcheck probe all follow `PORT` — no need to touch `docker-compose.yml`. The same applies on bare metal, where `PORT` takes precedence over `port` in `config.json`.

### All Accounts Cooling Down / Disabled

**Symptom:** Requests fail after repeated 429s or auth errors

**Solutions:**
1. Genuine credential invalidation permanently disables an account — re-run an interactive login to refresh its tokens
2. Quota / risk-control / rate-limit failures only cool the account down; it recovers automatically after the cooldown
3. Add more accounts to the pool for cross-account retry headroom
4. Check the admin panel's **Logs** page (failure / throttle records) or `docker compose logs -f`

### Auth Errors (401)

**Symptom:** Every request returns `401 Unauthorized`

**Solutions:**
1. Include a valid key on any of the six accepted channels, tried in this order: `Authorization: Bearer <key>`, `x-api-key: <key>`, `x-goog-api-key: <key>`, `?api_key=<key>`, `?token=<key>`, `?key=<key>`
2. Verify `API_KEY` (or `config.json` `apiKey`) matches the key you send
3. `/health` and `/v1/ping` do not require auth — use them to confirm the service is up

### High Latency or Timeouts

**Symptom:** Requests take a long time or time out

**Solutions:**
1. Check network latency to AWS CodeWhisperer / Kiro endpoints (`*.amazonaws.com`)
2. Add more accounts so load spreads across the pool
3. Raise `MAX_RPM_PER_CREDENTIAL` if a single account is being held back by the local RPM cap (`0` = unlimited). Note the symptom: an account over its local cap is simply **skipped during selection** — it never returns `429`. If every account is skipped, the caller gets a `503` (`no available upstream account`). A `429` reaching your client always came from an upstream throttle, never from this setting, so do not alert or back off on `429` to detect your own cap
4. Increase the request timeout in your client code

## Configuration Reference

Precedence: **command-line flags > environment variables > `config.json` > built-in defaults**. There are exactly two command-line flags: `-c/--config` (config file path) and `--credentials` (credentials file path; when omitted, `CREDENTIALS_PATH` / `config.json` / the built-in default decides). The mounted volume `./data` holds `config.json`, `credentials.json`, logs, and runtime state.

| Variable | Default | Description |
|----------|---------|-------------|
| `API_KEY` | — | Required: external API key (empty = protocol endpoints open access, warning at startup) |
| `ADMIN_API_KEY` | Falls back to `API_KEY` | Separate auth key for `/api/admin/*`; with neither this nor `API_KEY` set, `/api/admin/*` is open access — mandatory on public deployments |
| `HOST` | `127.0.0.1` (image ships `0.0.0.0`) | Listen address |
| `PORT` | 8080 | Service port (the compose port mapping and the healthcheck both follow this value) |
| `REGION` | us-east-1 | Default AWS region (the region inside an account's `profileArn` takes precedence) |
| `LOAD_BALANCING_MODE` | priority | Load balancing: `priority` (even round-robin) or `balanced` (weighted by `weight`) |
| `MAX_RPM_PER_CREDENTIAL` | 0 | Per-account requests-per-minute cap; `0` = unlimited. Exceeding it makes that account unselectable — it does **not** return `429`; with every account excluded the request ends as `503` |
| `CREDENTIALS_PATH` | `credentials.json`, resolved next to the `-c` config file (so `/app/data/credentials.json` in the container) | Path to the credentials file; overridden by the `--credentials` flag |

> The credentials path also decides where usage stats (`stats/`), API-KEY storage (`api_keys.json`), and the balance cache are written — all of them use the parent directory of `credentials.json`. The built-in default resolves next to the mounted `config.json`, so they land on the volume by themselves; the image deliberately sets **no** `CREDENTIALS_PATH` (its only `ENV` is `HOST=0.0.0.0`), which is what keeps `credentialsPath` in `config.json` effective — an image-level env var would outrank it. If you set a custom path, keep it inside the volume too, or the data is gone the moment the container is recreated.

**`data/config.json`** (camelCase, all fields optional; `logCapacity` is configured only here):

```json
{
  "host": "0.0.0.0",
  "port": 8080,
  "region": "us-east-1",
  "apiKey": "sk-your-external-key",
  "adminApiKey": "optional, admin console key",
  "credentialsPath": "/app/data/credentials.json",
  "loadBalancingMode": "priority",
  "maxRpmPerCredential": 0,
  "logCapacity": 5000,
  "kiroVersion": "0.11.107",
  "systemVersion": "win32#10.0.22631",
  "nodeVersion": "22.22.0"
}
```

- `logCapacity`: size (in lines) of the live-log ring buffer. `> 0` enables log capture (replayed/streamed by the admin panel's Logs page); `0` disables it (log endpoints return `503`). Default `5000`.
- `kiroVersion` / `systemVersion` / `nodeVersion`: spoofed UA version numbers injected from config.

## Docker Compose Reference

Key volumes and their purposes:

```yaml
volumes:
  - ./data:/app/data           # Persistent data (config.json, credentials.json, logs, runtime state)
```

The image is `ghcr.io/xwteam/kiro2api` (multi-arch amd64 / arm64). Inside the container the process runs as the non-root user `appuser` (UID 1000): `docker-entrypoint.sh` first `chown`s the mounted volume as root, then drops privileges via `gosu` (seamlessly upgrading data created by a legacy root process). The image ships with a `HEALTHCHECK` plus a compose healthcheck (the probed port resolves as `PORT` env var > `port` in `data/config.json` > `8080`, so it always matches the port the app listens on), and `restart: unless-stopped`.

Upgrade to a new image:
```bash
docker compose pull && docker compose up -d
# Ownership of the mounted ./data volume is corrected automatically by the entrypoint
```

> **Tip:** For production rollouts, bring up the new image on a side-channel port, run it in parallel with the live service, compare outputs (same request → identical output), and only cut over after it passes. Keep the old image on disk so you can roll back.

## Next Steps

- Read [USAGE.md](USAGE.md) to learn about the web panel and client integration
- Read [API.md](API.md) for detailed API endpoint documentation
- Check [README.md](../../README.md) for architecture and advanced features
