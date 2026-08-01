# 배포 가이드

kiro2api 서비스를 배포하는 방법을 상세히 설명합니다.

## 환경 요구사항

| 구성 요소 | 최소 버전 | 설명 |
|---------|---------|------|
| Docker | 20.10+ | Docker 배포 권장 |
| Docker Compose | 1.29+ | 컨테이너 오케스트레이션 도구 |
| Rust | 2024 edition | 소스에서 빌드할 때만 필요, Docker 배포는 불필요 |
| 메모리 | 512MB+ | 1GB 이상 권장 |
| 디스크 | 500MB+ | 이미지, 로그 및 설정 저장용 |
| 아키텍처 | amd64 / arm64 | 공식 이미지 멀티 아키텍처, 자동 매칭 |
| 네트워크 | AWS CodeWhisperer/Kiro 엔드포인트 접근 | `*.amazonaws.com` 접근 필요 |

## Kiro 자격 증명 준비

### 사전 조건

- 유효한 Kiro(CodeWhisperer) 계정 필요
- Builder ID / IAM SSO / 소셜 로그인 중 하나로 발급된 자격 증명
- 기존 Kiro 클라이언트의 자격 증명을 그대로 재사용(drop-in) 가능

### 단계별 가이드

1. Kiro 클라이언트 또는 기존 Kiro 자격 증명에서 아래 필드를 내보내기

2. 또는 관리 패널의 세 가지 대화형 로그인을 사용해 현장에서 발급:

   | 로그인 방식 | 설명 |
   |-----------|------|
   | Builder ID 디바이스 코드 | 디바이스 코드로 브라우저 인증 |
   | IAM SSO 인증 코드 | IdC(`clientId`/`clientSecret`) 기반 |
   | 소셜 토큰 | `profileArn`을 포함하는 social 방식 |

3. 자격 증명에서 다음 두 값이 핵심입니다:

   | 필드 | 특징 | 설명 |
   |-----------|------|------|
   | `accessToken` | 액세스 토큰 | 만료 시 자동 갱신 |
   | `refreshToken` | 갱신 토큰 | 액세스 토큰 재발급용 |

4. 발급받은 자격 증명 필드를 안전한 위치에 저장

5. `data/credentials.json`(배열)에 배치

### 획득 팁

- 기존 Kiro 클라이언트의 자격 증명 파일을 그대로 배열에 넣으면 됩니다
- `authMethod`가 `social`이면 `profileArn`, `idc`이면 `clientId`/`clientSecret`을 함께 포함
- `expiresAt`은 RFC3339 형식이어야 합니다
- 복사 시 여분의 공백이나 줄바꿈 없는지 확인

### 토큰 자가 치유

- 액세스 토큰 만료 시 서비스가 **메모리 내에서 자동 갱신**(단일 비행 조율로 동시 갱신 시 401 캐스케이드 방지)
- 갱신 성공 시 `credentials.json`에 원자적으로 재기록
- 데이터센터 IP: 토큰 수명은 계정 구독 등급 정책의 영향을 받음
- 서비스 갑자기 작동 불가 시 먼저 자격 증명 유효 여부 및 계정 냉각 상태 확인

## Docker 배포

### 빠른 시작

```bash
# 1. 저장소 복제
git clone https://github.com/xwteam/kiro2api.git
cd kiro2api

# 2. 환경 변수 템플릿 복사
cp .env.example .env

# 3. .env 파일 편집하여 API 키 입력
nano .env
# 또는
vim .env
```

### .env 파일 설정

`.env` 파일을 편집하여 최소한 대외 호출 키 `API_KEY`를 입력:

```env
# 필수: 대외 호출용 키 (비워두면 프로토콜 엔드포인트 개방 접근)
API_KEY=sk-당신의API키

# 관리 콘솔 독립 키 (줄 자체를 쓰지 않아야 API_KEY로 대체됩니다. 빈 값으로 쓰면 config.json의 관리 키를 지우는 셈)
# 외부 공개 배포 시 필수, 설정하지 않으면 /api/admin/*에 인증이 걸리지 않습니다
ADMIN_API_KEY=sk-당신의관리자키

# 선택: 서비스 포트 (기본값 8080). compose의 포트 매핑과 헬스체크가 이 값을 따르므로 여기만 바꾸면 됩니다
PORT=8080

# 선택: 기본 AWS region (계정 profileArn 내 region 우선, 기본값 us-east-1)
REGION=us-east-1

# 선택: 부하 분산 모드 (priority/balanced, 기본값 priority)
LOAD_BALANCING_MODE=priority

# 선택: 계정별 분당 요청 상한 (0 = 무제한)
MAX_RPM_PER_CREDENTIAL=0
```

### 중요 사항

- 값 앞뒤에 따옴표 불필요
- 여분의 공백이나 줄바꿈 없어야 함
- `API_KEY`가 비어 있으면 프로토콜 엔드포인트가 **개방 접근** 상태가 되며 시작 시 경고 출력, 외부 배포 시 반드시 설정
- `ADMIN_API_KEY`와 `API_KEY`를 모두 설정하지 않으면 `/api/admin/*`도 개방 접근 상태가 되어 자격 증명·인증 키·설정을 누구나 바꿀 수 있으므로, 외부 공개 배포 시에는 `ADMIN_API_KEY`를 반드시 설정
- `API_KEY=`, `ADMIN_API_KEY=`처럼 **빈 값으로 두지 말 것** — `config.json`에 설정해 둔 키를 덮어씁니다. 쓰지 않을 항목은 줄 전체를 주석 처리

### 자격 증명 배치

Kiro 계정 자격 증명을 `data/credentials.json`(배열, 기존 Kiro 자격 증명 그대로 drop-in 가능)에 배치:

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

### 서비스 시작

```bash
mkdir -p data
docker compose up -d
```

### 시작 확인

```bash
docker compose logs -f
```

다음 상태 확인:
- 계정 풀 준비 완료 및 리스닝 포트 표시 - 시작 성공
- 자격 증명 무효 또는 계정 냉각 - 자격 증명 재확인 필요

이미지는 멀티 아키텍처(amd64/arm64)이며, 컨테이너 내부는 non-root 사용자로 실행됩니다: entrypoint가 먼저 root로 마운트된 볼륨을 `chown`한 다음 `gosu`로 권한을 낮춥니다. 이미지에는 `HEALTHCHECK`가 내장되어 있으며(프로브 포트는 `PORT` 환경 변수 > `data/config.json`의 `port` > `8080` 순으로 해석되어 애플리케이션 리스닝 포트와 항상 일치), `restart: unless-stopped`가 적용됩니다.

## 다중 계정 설정

여러 Kiro 계정으로 부하 분산을 구현하려면 `data/credentials.json` 배열에 여러 항목을 추가:

```json
[
  {
    "id": 12345,
    "accessToken": "g.a000xxx...",
    "refreshToken": "...",
    "expiresAt": "2026-07-25T12:00:00Z",
    "authMethod": "social",
    "profileArn": "arn:aws:codewhisperer:us-east-1:...:profile/...",
    "label": "주 계정"
  },
  {
    "id": 12346,
    "accessToken": "...",
    "refreshToken": "...",
    "expiresAt": "2026-07-25T12:00:00Z",
    "authMethod": "idc",
    "clientId": "...",
    "clientSecret": "...",
    "label": "보조 계정"
  }
]
```

### 참고 사항

- 자격 증명이 여러 개면 **다중 계정 라운드로빈**이 자동 활성화됩니다
- 부하 분산 모드는 `priority`(동일 가중치 라운드로빈, 기본값)와 `balanced`(`weight` 기반 가중치 분배) 두 가지이며 관리 패널에서 런타임에 전환 가능
- 관리 패널의 세 가지 대화형 로그인으로 실행 중 동적 계정 추가 가능

### 토큰 자가 치유와 엔드포인트 회귀

kiro2api는 내장 토큰 자가 치유 및 회귀 메커니즘 포함:
- 토큰 만료 시 자동 메모리 갱신(단일 비행 조율) + `credentials.json` 원자적 재기록
- Kiro IDE → CodeWhisperer → AmazonQ 다중 엔드포인트 순차 회귀, `429`/네트워크 오류 시 자동 전환
- 계정 레벨 실패 시 계정 간 자동 재시도, 결정적 요청 오류(예: 지원하지 않는 모델 `INVALID_MODEL_ID`)는 **무의미하게 재시도하지 않고 계정에도 영향 주지 않음**

body-aware 실패 분류: 진정한 자격 증명 실효만 영구 비활성화하고, 할당량/리스크 관리/스로틀은 일괄 냉각 자가 치유합니다.

> 사용 가능한 모델은 계정 구독 등급의 영향을 받습니다. 무료 등급(KIRO FREE)은 보통 `claude-sonnet-4.5`만 허용됩니다. opus/GPT 등은 더 높은 등급이 필요하며, 지원하지 않는 모델을 요청하면 명확히 400(`INVALID_MODEL_ID`)을 반환합니다.

## 검증

### 헬스 체크

```bash
curl http://localhost:8080/health
```

예상 응답:
```json
{"service":"kiro2api","status":"ok","version":"0.7.11"}
```

### 모델 목록 조회

```bash
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-당신의API키"
```

### 테스트 요청

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의API키" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"안녕하세요"}]}'
```

AI 응답 텍스트가 보이면 배포 성공입니다. 401 반환 시 API 키 확인하세요.

## 문제 해결

### 지원하지 않는 모델 요청 시 400

**증상**: 요청 시 `400`과 함께 `INVALID_MODEL_ID` 오류

**해결책**:
1. 사용 가능한 모델은 계정 구독 등급에 따라 결정됩니다
2. 무료 등급(KIRO FREE)은 보통 `claude-sonnet-4.5`만 허용
3. 더 넓은 모델 목록(계정들의 업스트림 합집합, 비어 있으면 내장 정적 17종)은 관리 API `GET /api/admin/models`로 확인하십시오. 다만 중계가 실제로 받아들이는 id는 이 목록보다 넓습니다 — 이름 매핑이 소문자 부분 문자열로 해석해 내기만 하면 목록에 없는 별칭도 통과합니다. 프로토콜의 `GET /v1/models`는 계정 풀도 구독 등급도 읽지 않는 **컴파일 시점 고정 목록**이므로, 거기 있다고 서비스 가능한 것도 아니고 거기 없다고 못 쓰는 것도 아닙니다:

```bash
curl http://localhost:8080/api/admin/models \
  -H "Authorization: Bearer sk-당신의관리키"
```

### 포트 충돌

**증상**: `Address already in use` 오류

**해결책**: `.env` 파일에서 PORT 변경:

```env
PORT=8081
```

그 후 `docker compose up -d` 재실행

> `PORT` 한 곳만 바꾸면 됩니다: 애플리케이션 리스닝 포트, compose의 포트 매핑(`${PORT:-8080}:${PORT:-8080}`), 헬스체크 프로브 포트가 모두 이 값을 따르므로 `docker-compose.yml`을 따로 손댈 필요가 없습니다. 베어메탈 배포도 마찬가지이며, `PORT`가 `config.json`의 `port`보다 우선합니다.

### 인증 실패 (401)

**증상**: 요청 시 `401 Unauthorized` 오류

**해결책**:

1. 프로토콜 호출에는 키를 실어 보내야 합니다. 게이트는 아래 채널을 `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > 쿼리(`api_key` > `token` > `key`) 우선순위로 모두 받아들이므로, 어느 것을 써도 됩니다:
```bash
# Authorization: Bearer (권장)
curl -H "Authorization: Bearer sk-당신의API키" ...
# 또는 x-api-key
curl -H "x-api-key: sk-당신의API키" ...
# 또는 Gemini 네이티브 헤더 x-goog-api-key (공식 google-genai SDK가 쓰는 채널)
curl -H "x-goog-api-key: sk-당신의API키" ...
# 또는 쿼리 파라미터 api_key / token / key
curl "http://localhost:8080/v1/models?token=sk-당신의API키"
curl "http://localhost:8080/v1beta/models?key=sk-당신의API키"
```

2. `/health`, `/v1/ping`은 인증이 필요 없습니다
3. `/api/admin/*`는 `adminApiKey`(미설정 시 `apiKey`로 대체)로 인증하며, 둘 다 설정하지 않으면 인증 없이 개방됩니다(`/admin`, `/user` 패널 본체는 언제나 인증 없음)

### 계정 상태 확인

관리 패널(http://localhost:8080/admin)에 `adminApiKey`로 로그인하여 "계정 관리" 탭에서 각 계정의 잔액, 냉각 상태, 우선순위/권중을 확인할 수 있습니다.

### 토큰 갱신

토큰 만료 시 서비스가 자동으로 메모리 내 갱신 후 `credentials.json`에 원자적으로 재기록하므로 수동 개입이 필요 없습니다. 진정한 자격 증명 실효만 영구 비활성화되며, 할당량/리스크 관리/스로틀은 냉각 후 자가 치유됩니다.

### 서비스 재시작

```bash
docker compose restart
```

또는 관리 패널 설정 페이지에서 "서비스 재시작" 버튼 클릭

## 로그 확인

```bash
# 실시간 로그 보기
docker compose logs -f

# 마지막 100줄 보기
docker compose logs --tail=100

# 특정 시간 이후 로그 보기
docker compose logs --since 10m
```

관리 패널의 "실시간 로그" 페이지에서는 구조화된 표 + 방향 필터 + 검색 + 페이지네이션 + SSE 실시간 푸시 + 다운로드가 제공됩니다. 단, `logCapacity`가 `> 0`이어야 로그 캡처가 활성화되며(`0`이면 로그 엔드포인트가 `503` 반환), 이 값은 `config.json`에서만 설정합니다.

## 다음 단계

배포 완료 후:
1. [USAGE.md](USAGE.md)에서 Web 패널 및 API 사용법 확인
2. [API.md](API.md)에서 API 엔드포인트 상세 정보 확인
3. 서드파티 클라이언트 연동 설정
