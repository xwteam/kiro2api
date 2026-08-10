<div align="center">

<img src="../logo.png" width="128" height="128" alt="kiro2api">

<h1>kiro2api</h1>
<h3>다중 프로토콜 AI 중계 · Kiro 백엔드</h3>
<p>단일 코드베이스로 OpenAI / Anthropic / OpenAI-Responses / Gemini 4대 주류 AI SDK 호환, Kiro(CodeWhisperer) 백엔드가 Claude 계열 모델을 통합 제공, 순수 비동기 Rust 아키텍처, Docker 빠른 배포.</p>

<p>
  <img src="https://img.shields.io/badge/Rust-2024-orange?style=flat-square&logo=rust&logoColor=white" alt="Rust">
  <img src="https://img.shields.io/badge/axum-0.8-000000?style=flat-square&logo=rust&logoColor=white" alt="axum">
  <img src="https://img.shields.io/badge/tokio-async-4E9A06?style=flat-square&logo=rust&logoColor=white" alt="tokio">
  <img src="https://img.shields.io/badge/Docker-20.10+-2496ED?style=flat-square&logo=docker&logoColor=white" alt="Docker">
  <img src="https://img.shields.io/badge/arch-amd64%20%7C%20arm64-4285F4?style=flat-square&logo=linux&logoColor=white" alt="Arch">
  <img src="https://img.shields.io/badge/License-MIT-green?style=flat-square" alt="License">
  <img src="https://img.shields.io/badge/version-v0.15.0-success?style=flat-square" alt="Version">
</p>

<p>
  <a href="#-최근-업데이트">최근 업데이트</a> &bull;
  <a href="#-핵심-기능">핵심 기능</a> &bull;
  <a href="#-시스템-요구사항">시스템 요구사항</a> &bull;
  <a href="#-빠른-배포">빠른 배포</a> &bull;
  <a href="#-통합-예제">통합 예제</a> &bull;
  <a href="#-api-엔드포인트">API 엔드포인트</a> &bull;
  <a href="#-설정">설정</a> &bull;
  <a href="#-주의사항">주의사항</a> &bull;
  <a href="#-로드맵">로드맵</a>
</p>

<p>
  📖 문서 언어: <a href="../zh-CN/README.md">简体中文</a> | <a href="../zh-TW/README.md">繁體中文</a> | <a href="../en/README.md">English</a> | <a href="../ja/README.md">日本語</a> | 한국어
</p>

<br>

<a href="https://github.com/xwteam/kiro2api/issues"><img src="https://img.shields.io/github/issues/xwteam/kiro2api?style=flat-square" alt="Issues"></a>
<a href="https://github.com/xwteam/kiro2api/stargazers"><img src="https://img.shields.io/github/stars/xwteam/kiro2api?style=flat-square" alt="Stars"></a>

</div>

---

> [!NOTE]
> 이 프로젝트는 연구 및 학습 목적으로만 사용됩니다. 책임감 있게 사용하고 상업적 목적으로 사용하지 마십시오.

> [!IMPORTANT]
> `apiKey`/`API_KEY`가 비어 있으면 프로토콜 엔드포인트가 **개방 액세스** 상태가 됩니다(시작 시 경고 표시). 외부 배포 시 반드시 설정하십시오. 관리 인터페이스 `/api/admin/*`는 `adminApiKey`(미설정 시 `apiKey`로 폴백)를 설정한 뒤에야 보호됩니다 — **두 key를 모두 설정하지 않으면 관리 인터페이스도 패널과 똑같이 개방 상태**가 되어, 누구나 자격 증명을 추가·삭제하고 인증 키를 바꿀 수 있습니다. `/admin`, `/user` 패널 본체는 언제나 인증이 걸려 있지 않습니다. 공개 인터넷에 배포한다면 `ADMIN_API_KEY`를 반드시 설정하십시오. 컨테이너 이미지는 `HOST=0.0.0.0`이 기본 내장되어 있습니다. 베어메탈 배포 시에는 `HOST`를 함부로 `0.0.0.0`으로 바꾸지 마십시오.

> [!TIP]
> 백엔드는 Kiro(CodeWhisperer) 계정 풀입니다. **사용 가능한 모델은 계정 구독 등급에 따라 결정됩니다**: 무료 등급(KIRO FREE)은 일반적으로 `claude-sonnet-4.5`만 허용하며, opus/GPT 등은 더 높은 등급이 필요합니다 — 지원되지 않는 모델을 요청하면 조용히 실패하는 대신 400(`INVALID_MODEL_ID`)을 명확히 반환합니다.

---

## 📝 최근 업데이트

> 아래 표에는 **최근 10건**만 있습니다. 전체 변경 로그는 [CHANGELOG.md](../../CHANGELOG.md)를 참조하세요.

| 날짜 | 업데이트 내용 |
|------|----------|
| 2026-08-10 | v0.15.0 - 🔁 **Quota-exhausted accounts no longer have to be relearned after every restart.** The user asked directly: "aren't exhausted accounts already disabled — why do requests still reach them?" They were disabled, but **only in memory**: v0.10.2 deliberately kept runtime verdicts off disk so one permission blip could not write an account off permanently — but quota is precisely the kind that has a **known recovery time**. So every deploy wiped the knowledge and the pool rediscovered it using **the user's requests** (measured: 13 exhausted accounts meant two 502s before the third request succeeded). It is now persisted with its reset time, survives restarts and returns to the pool automatically; the time comes from upstream's `nextResetAt` when available, falling back to the first of next month. Also: **the server-side `web_search` tool actually works now** — it used to be merely tolerated (no longer a 400 since v0.11.0) while never searching, so the model answered with ordinary text and the client believed a search had happened. Such requests are now intercepted before the data plane, sent to the upstream `/mcp` endpoint, and returned as `server_tool_use` + `web_search_tool_result` |
| 2026-08-10 | v0.14.1 - 🔧 **Two things found while verifying the previous releases in production.** ① `usage.input_tokens` was **always 0** in responses: the estimate added in v0.13.0 only fed billing and was never written back, so the number existed in the invoice but not in the reply the client uses to compute cost and context usage — both the non-streaming `usage` and the streaming `message_start` now carry it. ② **With a batch of quota-exhausted accounts in the pool, a user's first few requests failed in a row**: quota exhaustion shared the 3-attempt budget with auth failures, and with 13 exhausted accounts two requests returned 502 before the third succeeded (each request burned three accounts before disabling them). Quota exhaustion is **deterministic** for the billing period, so it now shares the account-level deterministic class with "model unavailable" and gets an allowance sized to the pool, while transient and auth failures keep their small cap of 3. When the pool really is exhausted the answer is a `429` that says so, instead of an unhelpful 502 |
| 2026-08-10 | v0.14.0 - 🧩 **비교 마무리**. ① 히스토리의 어시스턴트 턴 `content`가 **빈 문자열**이 될 수 있어 업스트림이 요청 전체를 거부했습니다(도구 호출만 있는 턴에는 텍스트가 없는데, 사용자 턴에는 오래전부터 비어 있지 않도록 처리가 있었지만 어시스턴트 턴에는 없었습니다) → 공백 하나로 대체. ② **경계가 이미 알려진 손상 프레임을 1바이트씩 다시 스캔**했습니다(prelude CRC가 통과하면 `total_len`은 신뢰할 수 있습니다) → 프레임 단위로 건너뜁니다. ③ **`tlsBackend`를 런타임에 전환 가능**하게 했습니다(이전에는 컴파일 타임 선택이라 이미지를 다시 만들어야 했습니다) — 자체 서명 CA 프록시 뒤에서는 보통 한쪽만 핸드셰이크에 성공하며, 증상은 「토큰 갱신 불가/연결 불가」로 겉보기에 TLS와 무관해 보입니다 |
| 2026-08-09 | v0.13.0 - 🧠 **확장 사고(thinking)를 완전히 연결**했습니다(이전에는 기능 자체가 없었습니다). 요청 측에서 `thinking` 필드가 조용히 버려져 업스트림에 지시가 전달되지 않았고, 응답 측에서는 업스트림이 `<thinking>…</thinking>`로 감싸 보낸 사고 과정을 그대로 통과시켜 클라이언트가 사고 전체를 본문으로 표시했습니다. 이제 enabled/adaptive 지시를 system 맨 앞에 주입하고 독립적인 `thinking` 블록으로 분리합니다(스트리밍은 `thinking_delta`). 일반 텍스트는 **추가 지연 없이** 통과합니다. 그 외 **토큰 추정이 중국어를 약 3배 과소평가**하던 문제, **스트리밍 입력 토큰이 항상 0**이던 문제, **컨텍스트 윈도가 모든 모델에서 200K로 고정**(업스트림 `maxInputTokens`를 파싱 후 폐기)되던 문제를 수정 |
| 2026-08-09 | v0.12.0 - 🎚️ **서로 다른 구독 등급의 계정이 드디어 공존합니다**(사용자가 조사용으로 제공한 실제 API 키로 재현·검증). 원인 두 가지: ① `INVALID_MODEL_ID`를 *요청 수준* 오류로 분류해 즉시 400을 반환했지만 실제로는 *계정 수준*입니다 — 사용 가능한 모델은 등급에 따라 다른데, 이 릴레이가 클라이언트에 노출하는 것은 전체 계정의 **합집합**이라 합집합의 모델이 지원하지 않는 계정에 걸리면 반드시 실패합니다 → `ModelUnavailable`로 분리해 계정에 불이익 없이 다른 계정으로 재시도. ② 계정 간 재시도 예산이 3회였는데 해당 모델을 지원하는 계정이 14번째일 수 있습니다 → 모델 미지원은 계정 장애 예산을 소비하지 않습니다. 그 외 어느 계정이 어느 모델을 지원하지 않는지 기억(같은 모델 두 번째 요청은 계정 1개, 첫 번째는 14개), **`priority`가 `weight`의 별칭일 뿐 선택에 전혀 관여하지 않았음** → 숫자가 작을수록 우선, 가져오기는 일괄 999, us-east-1 외 지역의 모델 새로고침 실패를 `q.{region}`으로 폴백 |
| 2026-08-09 | v0.11.1 - 🔬 **운영 환경 실측으로 드러난 두 가지.** ① **도구 설명이 비어 있으면 업스트림이 요청 전체를 거부**합니다(실측: 같은 도구에 설명이 있으면 200과 정상 `tool_use`, 설명을 빼면 `400 Invalid tool use format / REQUEST_BODY_INVALID`). v0.11.0에서 `null`을 빈 문자열로 바꿨지만 업스트림이 원하는 것은 **비어 있지 않은** 값이었습니다 → 도구 이름으로 대체. ② `REQUEST_BODY_INVALID`를 재시도 가능으로 처리했습니다. 이는 결정적이어서 어떤 계정으로도 똑같이 실패하는데, 잘못된 요청 하나가 여러 계정의 재시도 예산을 소모하고(실측 4개) 결국 모호한 502를 반환했습니다 → 재시도 없음·계정 무벌점 등급으로 옮기고 도구 명세를 지목하는 400을 반환합니다 |
| 2026-08-09 | v0.11.0 - 🧰 **「업스트림이 요청을 거부하게 만드는」 부류의 버그 수정.** ① 서버 측 내장 도구(web_search 등)는 `input_schema`가 없는데 해당 필드가 필수여서, 공식 표기를 쓰기만 해도 우리 계층에서 400. ② 도구 `description`이 **null**로 직렬화될 수 있었습니다(실제 클라이언트는 항상 문자열). ③ `input_schema`를 그대로 전달해 `properties: null` 같은 형태에서 **요청 전체**가 거부됐습니다 → 형태만 정규화(의미는 변경하지 않음). ④ 너무 긴 도구 이름을 줄이지 않았고(업스트림 상한 63) 복원도 불가능했습니다 → 결정적으로 축약하고 `축약명→원본명`을 출구까지 전달해 복원. ⑤ `:message-type == "error"` 프레임을 완전히 무시해 업스트림 오류가 200+빈 메시지가 됐습니다. ⑥ 패널의 없는 자산이 404가 아니라 200+HTML을 반환했습니다. 그 외 `POST /api/admin/credentials/{id}/refresh` 추가 |
| 2026-08-09 | v0.10.2 - 🩹 **수정: 런타임 사용 중단이 디스크에 기록되어 「재시작하면 복구된다」가 빈말이었습니다.** v0.10.0은 메모리 상태만 바꾼다고 명시했지만, 영속화 시 `snapshot_credentials()`가 런타임 `disabled`로 `cred.disabled`를 덮어써서 한도 소진 1회 또는 401/403 2회로 `credentials.json`에 **영구히** 기록됐고 재시작으로도 돌아오지 않았습니다 — 바꾸기 전 동작보다 나쁩니다(운영 중 계정 하나를 이렇게 잃었습니다). 이제 영속적인 결론만 기록합니다. **직접 중지한 적 없는 `"disabled": true`가 있다면 false로 되돌리면 복구됩니다** |
| 2026-08-09 | v0.10.1 - 🔌 **계정별 아웃바운드 프록시와 누락됐던 세션 식별 필드 보완.** ① 프록시 3개 필드는 **받고 나서 버려졌고**, `hasProxy`도 `false`로 하드코딩돼 있었습니다 — 패널은 설정됐다고 표시하지만 실제로는 전부 직접 연결. 이제 실제로 저장되어 동작하며 우선순위는 자격증명 > 전역 > 직결(`"direct"`는 명시적 직결)이고, **한 계정의 데이터 플레인/토큰 갱신/잔량 조회/모델 목록/백그라운드 갱신이 모두 같은 출구**를 사용합니다(데이터 플레인만 프록시를 타고 갱신은 메인 IP에서 나가는 것은 프록시가 없느니만 못합니다). ② `conversationId`가 요청마다 새로 생성되고 UUID 형태도 아니었습니다 → 클라이언트 `metadata.user_id`의 세션 UUID를 우선 사용해 같은 세션이 공유합니다. ③ `agentContinuationId`는 **아예 보내지 않았습니다**. ④ 패널로 추가한 계정은 machineId가 고정되지 않았습니다 → 풀 진입 시점에 고정. ⑤ `isCurrent` 하드코딩 `false`를 실제 값으로 |
| 2026-08-09 | v0.10.0 - 🎯 **동작 형태를 실제 클라이언트에 맞췄습니다.** 오랫동안 안정적으로 운영되는 동종 구현과 모듈 단위로 비교한 결과, 앞선 두 릴리스의 정지 원인 진단이 뒤집혔습니다: 그 구현은 연결을 재사용하고, HTTP/1.1로 고정하지 않으며, TLS 기본값도 rustls입니다 — 우리가 걸었던 세 가지를 하나도 하지 않습니다. 실제 차이는: ① `priority`가 **요청마다 계정을 교체**해, 같은 출구 IP에서 수백 개의 machineId가 초 단위로 번갈아 나타났습니다 → **한 계정을 쓸 수 없게 될 때까지 고정**하도록 변경; ② 정지/한도 소진 계정이 5분·30분 쿨다운 후 **풀로 복귀**해 벽을 계속 들이받았습니다 → 사용 중단(메모리 상태, 초기화하면 복구); ③ **토큰 갱신 요청에 User-Agent조차 없었습니다**(바이트 실측). 그것도 Kiro 자체 엔드포인트이며 모든 계정이 반드시 지나는 경로입니다 → axios와 sso-oidc 두 가지 실제 형태로 보완; ④ **machineId가 갱신할 때마다 바뀜**(교체되는 refreshToken에서 매번 계산) → 로드 시 고정해 디스크에 기록; ⑤ ksk 계정의 machineId가 전역 상수로 퇴화 → 자격 증명 유형별로 파생. 그 밖에 429를 일시적 스로틀링으로 재분류, 데이터 플레인 엔드포인트를 3개에서 1개로 축소, `amz-sdk-invocation-id`를 UUID v4로, 헤더 순서 정렬, `claude-opus-5` 매핑 추가, SSE에 25초 keep-alive 추가 |

---

## 🌟 핵심 기능

> 📖 자세한 사용 문서: [USAGE.md](USAGE.md)

### 🔌 4개 프로토콜 프런트엔드, 하나의 백엔드

- 하나의 서비스로 **OpenAI Chat**, **Anthropic Messages**, **OpenAI Responses**, **Gemini 네이티브** 네 가지 SDK 형식 동시 제공
- 내부적으로 **Anthropic Messages를 중추 모(母) 포맷**으로 삼고, 나머지 프로토콜은 양방향 변환 후 동일한 중계 코어를 재사용
- 각 프로토콜 모두 **스트리밍(SSE)**, **함수 호출(도구) 실전달**, **이미지 입력(멀티모달)** 지원
- **이중 프리픽스 마운트**: 각 프로토콜을 표준 베어 프리픽스와 명시적 벤더 프리픽스(`/openai/v1`, `/claude/v1`, `/gemini/v1beta`)에 동시 마운트하여, 주요 SDK가 `base_url`만 채우면 바로 사용 가능

### 🔐 통합 인증 게이트

- 키 추출 채널은 6개이며 `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > 쿼리(`?api_key=` > `?token=` > `?key=`) 순으로 모두 받습니다. 값은 상수 시간 비교, 실패 시 즉시 `401`
- `adminApiKey`(미설정 시 `apiKey`로 폴백)로 `/api/admin/*` 보호, 둘 다 설정되지 않으면 이 게이트는 개방 모드; 보유자는 자신의 **API-KEY**로 `/api/user/*` 액세스
- `/health`, `/v1/ping` 등 헬스 엔드포인트는 인증 불필요

### 🔄 계정 풀 및 토큰 자가 치유

- **다중 계정 라운드로빈**: `priority`(등가 순환, 기본값)와 `balanced`(`weight` 가중치 기반) 두 가지 전략, 관리 패널에서 런타임 전환 가능
- 계정별 독립 RPM 제한, 단계별 쿨다운; 연속 실패는 범주별(영구 무효 / 모호한 인증 / 쿼터 / 일시적)로 차등 처리
- 토큰 만료 시 **메모리 내 자동 갱신**(싱글플라이트 조율로 동시 갱신에 따른 연쇄 401 방지), 갱신 성공 시 `credentials.json`에 원자적 저장
- Builder ID 디바이스 코드 / IAM SSO 인가 코드 / 소셜 토큰 세 가지 로그인 플로우 지원, 기존 Kiro 자격 증명을 그대로 drop-in 가능

### 🔀 엔드포인트 폴백 및 크로스 계정 재시도

- Kiro IDE → CodeWhisperer → AmazonQ 다중 엔드포인트 순차 폴백, `429`/네트워크 오류 시 자동 전환
- 계정 단위 실패 시 자동으로 다른 계정으로 재시도; 결정적 요청 오류(예: 지원되지 않는 모델 `INVALID_MODEL_ID`)는 **무모하게 재시도하지 않고 계정을 오손시키지 않으며**, 업스트림 원인을 그대로 클라이언트에 반환
- body-aware 실패 분류: 진짜 자격 증명 무효만 영구 비활성화, 쿼터/리스크 컨트롤/스로틀은 일률적으로 쿨다운 자가 치유

### 🖥 웹 관리 패널

- 내장 정적 관리 콘솔(`/admin`), `adminApiKey`로 로그인, 풍부한 `/api/admin/*` 인터페이스로 구동
- **대시보드**: 가동 시간 실시간 카운터, 전역 잔여 크레딧, 시스템 정보(버전/Rust/OS/메모리/CPU/PID/실행 모드), 후원 QR 코드 카드(원격 설정 실시간 로드), **업데이트 확인**(GitHub Release 비교, 대화상자에서 현지화된 릴리스 노트 + 업그레이드 명령 표시)
- **계정 관리**: 추가/삭제/수정/조회, 3가지 대화형 로그인, 일괄 가져오기(항목별 생존 검증 + 중복 제거), 우선순위/가중치, 잔액 조회
- **API-KEY 관리**: 발급/비활성화/라벨 수정, key별 사용량 및 페이지 단위 기록
- **모델 테스트**: 패널에서 임의의 모델에 테스트 요청을 보내 연결성 확인; 커스텀 key가 없으면 마스터 API 키로 폴백
- **사용량 통계**: 일별/계정별 차원, 클라이언트 IP 및 계정 라벨 포함, 일별 드릴다운
- **실시간 로그**: 구조화 테이블 + 방향 필터 + 검색 + 페이지네이션 + SSE 실시간 푸시 + 다운로드
- **설정**: 런타임에 부하 분산/인증 키 변경, 통합 예시(프로토콜×언어 복사 가능 스니펫), **원클릭 서비스 재시작**
- 상단 컨트롤 바: 실행 상태 배지, GitHub, 재시작, 다크/라이트 테마, 5개 언어 전환

### 👤 사용자 패널

- 내장 사용자 콘솔(`/user`), 보유자가 자신의 **API-KEY**로 로그인(관리자 권한 불필요)
- 해당 key의 할당량, 누적 사용량, 페이지 단위 기록 확인, `/api/user/*`로 구동

### 🧭 모델 이름 매핑

- 클라이언트가 전달한 모델 이름을 **소문자 부분 문자열** 기준으로 Kiro 내부 모델에 매칭(미매칭 시 → `400`)
- 프로토콜의 `/models`는 **고정된 짧은 목록**(프로토콜당 3종)을 반환하며 계정 구독 등급으로 필터링되지 않습니다 — 목록에 있어도 등급 미인가면 `400`이 날 수 있고, 목록에 없어도 이름 매핑만 되면 동작합니다. 더 넓은 목록(계정들의 업스트림 합집합, 비어 있으면 내장 정적 17종)은 `GET /api/admin/models` 참고

### ⚡ 고성능 아키텍처

- **Rust + axum 0.8 + tokio** 기반, 전체 체인 비동기 논블로킹
- AWS eventstream 프레임 디코딩, 계정 풀 직렬 락 점유의 임계 구역 최소화, 네트워크 발송 즉시 해제
- 강력한 타입 serde 검증, 각 프로토콜별 독립 어댑터 모듈
- 멀티스테이지 Docker 빌드, non-root 실행(gosu), 멀티 아키텍처 이미지, 헬스체크

---

## 📋 시스템 요구사항

| 종속성 | 버전 | 설명 |
|------|------|------|
| Rust | 2024 edition | 소스에서 빌드할 때만 필요; Docker 배포는 로컬 설치 불필요 |
| Docker | 20.10+ | Docker 배포 권장 |
| Kiro 계정 | — | 유효한 Kiro(CodeWhisperer) 자격 증명 필요(Builder ID / IdC / 소셜 로그인) |
| 아키텍처 | amd64 / arm64 | 공식 이미지 멀티 아키텍처, 둘 중 자동 매칭 |

> [!TIP]
> Docker 배포를 사용하면 로컬에 Rust 환경을 설치할 필요가 없으며, Docker와 유효한 Kiro 자격 증명만 있으면 됩니다.

---

## ⚡ 빠른 배포

> 📖 자세한 배포 문서: [DEPLOY.md](DEPLOY.md)

> **전제 조건**: 유효한 Kiro(CodeWhisperer) 계정 자격 증명이 필요합니다.

### 1. Kiro 자격 증명 획득

Kiro 클라이언트 / 기존 Kiro 자격 증명에서 다음 필드를 내보내거나, 관리 패널의 3가지 대화형 로그인(Builder ID 디바이스 코드 / IAM SSO 인가 코드 / 소셜 토큰)으로 현장에서 획득하십시오:

| 필드 | 설명 |
|------|------|
| `accessToken` / `refreshToken` | 액세스 토큰과 리프레시 토큰(만료 시 자동 갱신) |
| `expiresAt` | 토큰 만료 시각(RFC3339) |
| `authMethod` | `social`(`profileArn` 포함) 또는 `idc`(`clientId`/`clientSecret` 포함) |

### 2. Docker 배포

```bash
# 저장소 복제
git clone https://github.com/xwteam/kiro2api.git
cd kiro2api

# 환경 변수 파일 생성
cp .env.example .env
```

`.env`를 편집하여 최소한 외부 호출 키 `API_KEY` 하나를 입력하십시오:

```env
API_KEY=sk-당신의외부호출키
# 관리자 전용 독립 키. 공개 인터넷 배포 시 필수(설정하지 않으면 /api/admin/*가 API_KEY로 폴백되며, 둘 다 없으면 개방 상태).
# 필요 없다면 줄 전체를 주석 처리하십시오 — 빈 값으로 쓰면 config.json에 설정해 둔 키를 덮어씁니다.
ADMIN_API_KEY=sk-당신의관리자키
```

Kiro 계정 자격 증명을 `data/credentials.json`(배열, 기존 Kiro 자격 증명을 그대로 drop-in 가능)에 배치하십시오:

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

서비스 시작:

```bash
mkdir -p data
docker compose up -d
```

로그를 확인하여 시작 성공 확인:

```bash
docker compose logs -f
# 계정 풀 준비 완료 및 리스닝 포트가 표시되면 시작 성공
```

### 3. 검증

```bash
# 헬스 체크
curl http://localhost:8080/health
# {"service":"kiro2api","status":"ok","version":"0.15.0"}

# 프로토콜 고정 모델 목록 조회
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-당신의API키"

# 테스트 요청 보내기
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의API키" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"안녕하세요"}]}'
```

AI 응답 텍스트가 표시되면 배포 성공입니다. 401이 반환되면 API Key가 올바른지 확인하십시오.

---

## 🧪 통합 예제

> [!NOTE]
> 모든 API 요청에는 API Key가 필요합니다. 게이트는 아래 채널을 이 우선순위로 모두 받아들이므로, 어느 것을 써도 됩니다:
> - `Authorization: Bearer sk-xxx`(권장, OpenAI/Anthropic SDK 호환)
> - `x-api-key: sk-xxx`
> - `x-goog-api-key: sk-xxx`(공식 `google-genai` SDK가 기본으로 쓰는 채널)
> - 쿼리 파라미터 `?api_key=sk-xxx` / `?token=sk-xxx` / `?key=sk-xxx`(헤더를 설정할 수 없는 브라우저 SSE, Gemini 생태 표준)
>
> base URL은 **표준 베어 프리픽스**를 사용합니다: OpenAI = `{host}/v1`, Anthropic = `{host}`(SDK가 자동으로 `/v1/messages` 보완), Gemini = `{host}/v1beta`. 명시적 벤더 프리픽스 `/openai/v1`, `/claude/v1`, `/gemini/v1beta`도 사용 가능합니다.

<details>
<summary><b>OpenAI SDK (Python)</b></summary>

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://localhost:8080/v1",
    api_key="sk-당신의API키",
)

resp = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "Hello"}],
)
print(resp.choices[0].message.content)
```

</details>

<details>
<summary><b>Anthropic SDK (Python)</b></summary>

```python
import anthropic

client = anthropic.Anthropic(
    base_url="http://localhost:8080",
    api_key="sk-당신의API키",
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
<summary><b>Gemini SDK (Python)</b></summary>

```python
from google import genai

client = genai.Client(
    api_key="sk-당신의API키",
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
# 비스트리밍 요청
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의API키" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"Hi"}]}'

# 스트리밍 요청
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의API키" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"Hi"}],"stream":true}'
```

</details>

<details>
<summary><b>함수 호출(도구)</b></summary>

```python
resp = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "서울 오늘 날씨 어때"}],
    tools=[{
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "지정한 도시의 날씨 조회",
            "parameters": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }
        }
    }]
)
```

> 도구 호출은 네 가지 프로토콜 간에 **실전달**됩니다(Anthropic `tool_use` / OpenAI `tool_calls` / Gemini `functionCall`), 시뮬레이션하지 않습니다.

</details>

---

## 📡 API 엔드포인트

> 📖 자세한 API 문서: [API.md](API.md)

<details>
<summary><b>클릭하여 전체 엔드포인트 목록 펼치기</b></summary>

> **이중 프리픽스 공존**: 각 프로토콜은 「표준 베어 경로」와 「명시적 벤더 프리픽스 경로」를 동시에 제공합니다. 베어 경로는 공식 SDK가 `base_url`을 채울 때 접미사를 붙이지 않아도 바로 사용 가능하며, 벤더 프리픽스는 4개 벤더를 명확히 구분하는 데 사용됩니다.

### OpenAI 호환 (`/v1` 또는 `/openai/v1`)

| 메서드 | 엔드포인트 | 기능 |
|------|------|------|
| GET | `/models` | 프로토콜 고정 모델 목록(컴파일 시점 고정, 구독 등급으로 필터링되지 않음) |
| POST | `/chat/completions` | 대화 완성(스트리밍은 `chat.completion.chunk` + `[DONE]` 반환, 도구/이미지 포함) |

### OpenAI Responses (`/v1/responses` 또는 `/openai/v1/responses`)

| 메서드 | 엔드포인트 | 기능 |
|------|------|------|
| POST | `/responses` | Responses API(스트리밍은 명명 이벤트 + 단조 증가 `sequence_number`, `[DONE]` 없음; `previous_response_id`는 400 반환) |

### Anthropic 호환 (`/v1` 대화 진입점; `/claude/v1` 명시적 프리픽스)

| 메서드 | 엔드포인트 | 기능 |
|------|------|------|
| POST | `/v1/messages` | Messages(스트리밍/도구/이미지) |
| POST | `/v1/messages/count_tokens` | 토큰 수 추정 |
| GET | `/claude/v1/models` | 모델 목록(Anthropic 형태, OpenAI `/v1/models`와의 충돌 회피) |
| POST | `/claude/v1/messages` · `.../count_tokens` | 명시적 프리픽스 변형 |

### Gemini 네이티브 (`/v1beta` 또는 `/gemini/v1beta`)

| 메서드 | 엔드포인트 | 기능 |
|------|------|------|
| GET | `/models` | 모델 목록 |
| POST | `/models/{m}:generateContent` | 콘텐츠 생성(비스트리밍) |
| POST | `/models/{m}:streamGenerateContent` | 스트리밍 생성(`?alt=sse`, camelCase) |

### 관리 / 사용자 / 운영

| 메서드 | 엔드포인트 | 기능 |
|------|------|------|
| GET | `/admin` · `/api/admin/*` | 관리 패널 + 관리 인터페이스(`adminApiKey`, key를 하나도 설정하지 않으면 개방: 자격 증명 CRUD / 로그인 / API-KEY / 사용량 / 로그 / 잔액 / 설정 / 업데이트 확인 / 재시작) |
| GET | `/user` · `/api/user/*` | 사용자 패널 + 인터페이스(자신의 API-KEY) |
| GET | `/health` · `/v1/ping` | 헬스 체크(인증 불필요) |

</details>

> URL 안의 `localhost:8080`은 예시일 뿐이며, 포트는 `PORT`/`config.json`으로 설정하므로 배포 환경에 맞게 교체하십시오.
>
> 인증 게이트는 `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > 쿼리(`?api_key=` > `?token=` > `?key=`) 순으로 어느 채널이든 받아들입니다. Gemini 네이티브의 `x-goog-api-key`와 `?key=`도 지원하므로 공식 `google-genai` SDK는 `base_url`만 바꾸면 됩니다. 바뀌어야 하는 건 **값**으로, 언제나 **본 서비스의** API Key를 넘기고 실제 Google/OpenAI 벤더 키를 넘기지 마십시오.

---

## ⚙ 설정

우선순위: **명령줄 인자 > 환경 변수 > `config.json` > 내장 기본값**. 명령줄 인자는 두 개뿐입니다: `-c/--config`(설정 파일 경로)와 `--credentials`(자격 증명 파일 경로, 주지 않으면 `CREDENTIALS_PATH`/`config.json`/기본값 순으로 결정). 마운트 볼륨 `./data`에 `config.json`, `credentials.json`, 로그 및 런타임 상태가 저장됩니다.

> 자격 증명 경로는 사용량 통계(`stats/`), API-KEY 저장소(`api_keys.json`), 잔액 캐시가 기록되는 디렉터리까지 함께 결정합니다 — 모두 `credentials.json`의 상위 디렉터리를 사용합니다. 내장 기본값은 `-c`로 지정한 설정 파일이 있는 디렉터리를 기준으로 결정되며, 컨테이너는 `-c /app/data/config.json`으로 기동하므로 기본 경로가 `/app/data/credentials.json`, 즉 마운트 볼륨 안이 됩니다. 경로를 직접 지정할 때는 반드시 마운트 볼륨 안을 가리키게 하십시오. 그러지 않으면 컨테이너를 다시 만드는 순간 유실됩니다.

**환경 변수**(`.env.example` 참조):

| 변수 | 필수 | 기본값 | 설명 |
|------|------|--------|------|
| `API_KEY` | ✅ | — | 외부 호출 키(비워두면 프로토콜 엔드포인트 개방 액세스, 시작 시 경고) |
| `ADMIN_API_KEY` | ❌ | `API_KEY`로 폴백 | 관리자 전용 독립 인증 키; `API_KEY`와 함께 설정하지 않으면 `/api/admin/*`가 개방되므로 공개 배포 시 필수 |
| `HOST` | ❌ | `127.0.0.1`(이미지 내장 `0.0.0.0`) | 리스닝 주소 |
| `PORT` | ❌ | `8080` | 서비스 포트(compose의 포트 매핑과 헬스체크가 모두 이 값을 따름) |
| `REGION` | ❌ | `us-east-1` | 기본 AWS region(계정 `profileArn` 내 region 우선) |
| `LOAD_BALANCING_MODE` | ❌ | `priority` | 부하 분산: `priority`(등가 순환) / `balanced`(weight 가중치) |
| `MAX_RPM_PER_CREDENTIAL` | ❌ | `0` | 계정당 분당 요청 상한, `0` = 무제한 |
| `CREDENTIALS_PATH` | ❌ | `credentials.json`(`-c` 설정 파일과 같은 디렉터리 기준, 컨테이너에서는 `/app/data/credentials.json`) | 자격 증명 파일 경로; 명령줄 `--credentials`가 우선 |

**`data/config.json`**(camelCase, 모두 선택; `logCapacity`는 여기서만 설정):

```json
{
  "host": "0.0.0.0",
  "port": 8080,
  "region": "us-east-1",
  "apiKey": "sk-당신의외부호출키",
  "adminApiKey": "선택,관리자 전용",
  "credentialsPath": "/app/data/credentials.json",
  "loadBalancingMode": "priority",
  "maxRpmPerCredential": 0,
  "logCapacity": 5000,
  "kiroVersion": "0.11.107",
  "systemVersion": "win32#10.0.22631",
  "nodeVersion": "22.22.0"
}
```

- `logCapacity`: 실시간 로그 링 버퍼 개수, `>0`이면 로그 캡처 활성화(관리 패널 로그 페이지 재생/SSE), `0`이면 비활성화(로그 엔드포인트 503 반환); 기본값 `5000`.
- `kiroVersion`/`systemVersion`/`nodeVersion`: 위장 UA 버전 번호, 설정에서 주입.

---

## ⚠ 주의사항

1. **외부 배포 시 반드시 `API_KEY`와 `ADMIN_API_KEY` 설정**: `API_KEY`를 비워두면 프로토콜 엔드포인트가 개방 액세스됩니다(시작 시 경고). `adminApiKey`/`apiKey`를 모두 설정하지 않으면 `/api/admin/*`도 똑같이 개방되어 자격 증명, API-KEY, 인증 설정을 누구나 바꿀 수 있습니다. `/admin`, `/user` 패널 본체는 언제나 인증이 걸려 있지 않습니다(실제 게이트는 그 뒤의 `/api/**` 인터페이스에 있습니다); 베어메탈 배포 시 `HOST=0.0.0.0` 변경에 주의하십시오.

2. **사용 가능한 모델은 계정 구독 등급에 따라 결정**: 무료 등급(KIRO FREE)은 일반적으로 `claude-sonnet-4.5`만 허용합니다; 지원되지 않는 모델을 요청하면 `400`(`INVALID_MODEL_ID`)을 반환하며, 무모하게 재시도하거나 계정을 오손시키지 않습니다.

3. **토큰 자가 치유**: 토큰 만료 시 메모리 내에서 자동 갱신하고 `credentials.json`에 원자적으로 저장합니다; 진짜 자격 증명 무효만 영구 비활성화하고, 쿼터/리스크 컨트롤/스로틀은 일률적으로 쿨다운 자가 치유합니다.

4. **스트리밍 출력**: 네 가지 프로토콜 모두 스트리밍을 지원합니다; `stream:false`일 때도 서비스 내부에서는 여전히 이벤트 스트림을 디코딩하고, 수집 완료 후 전체 JSON을 한 번에 반환합니다. 업스트림 오류나 스트림 도중의 전송 중단이 발생하면 해당 프로토콜의 오류 이벤트로 스트림을 끝내며, 정상 완료로 위장하지 않습니다; `max_tokens` 도달이나 컨텍스트 소진으로 잘린 경우에도 그 잘림 사유를 그대로 보고합니다.

5. **네트워크 환경**: 배포 서버는 AWS CodeWhisperer/Kiro 엔드포인트(`*.amazonaws.com`)에 액세스할 수 있어야 합니다.

---

## 🗂 프로젝트 구조

```
kiro2api/
├── src/
│   ├── main.rs / cli.rs / lib.rs   # 진입점, CLI, 라이브러리 루트
│   ├── config.rs                   # 설정(env > config.json > 기본값)
│   ├── http.rs                     # 아웃바운드 HTTP 클라이언트(타임아웃 하드 리밋)
│   ├── logcap.rs                   # 실시간 로그 링 버퍼 + SSE 브로드캐스트
│   ├── server/                     # axum 라우트 조립, 통합 인증 게이트
│   ├── protocol/                   # 4개 프로토콜 어댑터
│   │   ├── anthropic/              #   Anthropic Messages(중추 모 포맷 + relay 코어)
│   │   ├── openai/                 #   OpenAI Chat Completions
│   │   ├── responses/              #   OpenAI Responses
│   │   └── gemini/                 #   Gemini 네이티브 v1beta
│   ├── kiro/                       # Kiro 데이터 플레인
│   │   ├── pool.rs                 #   계정 풀(부하 분산 + 실패 분류 + 쿨다운)
│   │   ├── provider.rs             #   업스트림 발송 + 엔드포인트 폴백
│   │   ├── convert.rs              #   모델 매핑 + 요청/응답 변환
│   │   ├── ensure_fresh.rs / refresh.rs  # 토큰 싱글플라이트 갱신
│   │   ├── eventstream/            #   AWS eventstream 프레임 디코딩
│   │   └── login/                  #   Builder ID / IAM SSO / 소셜 로그인 플로우
│   ├── admin/                      # /api/admin/* 관리 인터페이스
│   ├── user/                       # /api/user/* 사용자 인터페이스
│   ├── apikey/                     # API-KEY 저장 및 검증
│   ├── balance/                    # 잔액 캐시(TTL)
│   ├── stats/                      # 사용량/실패/스로틀 통계 + 가격
│   ├── models_cache/               # 동적 모델 목록 캐시
│   └── webui/                      # rust-embed 정적 패널 서비스(admin-ui-v2/, user-ui/dist)
├── admin-ui-v2/                    # 정적 관리 패널(HTML/CSS/JS, 컴파일 시 임베드)
├── user-ui/                        # 사용자 패널(빌드 산출물 임베드)
├── data/                           # 지속화 데이터(Docker 볼륨 마운트)
│   ├── config.json                 #   런타임 설정
│   └── credentials.json            #   Kiro 계정 자격 증명
├── docs/                           # 5개 언어 문서(README/USAGE/DEPLOY/API/SPONSORS)
├── Dockerfile                      # 멀티스테이지 빌드(멀티 아키텍처, non-root)
├── docker-compose.yml              # 오케스트레이션 설정
├── Cargo.toml / Cargo.lock
└── .env.example
```

---

## 🗺 로드맵

- [x] 4개 프로토콜 프런트엔드(OpenAI / Anthropic / OpenAI-Responses / Gemini)
- [x] Anthropic Messages 중추 모 포맷 + 통합 중계 코어
- [x] 스트리밍(SSE) + 함수 호출 실전달 + 이미지 멀티모달
- [x] Kiro 계정 풀(다중 계정 라운드로빈, 단계별 쿨다운, 부하 분산)
- [x] 토큰 싱글플라이트 자동 갱신 + 원자적 저장
- [x] 엔드포인트 폴백(Kiro/CodeWhisperer/AmazonQ) + 크로스 계정 재시도
- [x] body-aware 실패 분류(영구 무효만 비활성화, 나머지는 쿨다운 자가 치유)
- [x] 통합 인증 게이트(Bearer / x-api-key / x-goog-api-key / 쿼리 키 `?api_key=`·`?token=`·`?key=`)
- [x] 웹 관리 패널(자격 증명/로그인/API-KEY/사용량/로그/잔액/설정)
- [x] 사용자 패널(보유자가 자신의 API-KEY로 로그인)
- [x] 3가지 대화형 로그인 플로우(Builder ID / IAM SSO / 소셜 토큰)
- [x] 일별/계정별 사용량 통계(클라이언트 IP 및 계정 라벨 포함)
- [x] 실시간 로그(SSE) + 잔액 캐시 + 동적 모델 목록
- [x] 통합 예시(프로토콜×언어 복사 가능 스니펫)
- [x] 서비스 재시작 + 버전 업데이트 확인(GitHub Release 비교)
- [x] Docker 멀티 아키텍처(amd64/arm64) 배포 + CI
- [ ] `/admin`, `/user` 패널 본체 인증
- [ ] GitHub Actions 자동 빌드 및 이미지 게시

---

## ☕ 후원 & 기여

도움이 되셨나요? 작성자에게 커피를 사주거나 WeChat 그룹에 가입하여 지원을 받으세요. 자세한 내용은 [SPONSORS.md](SPONSORS.md)를 참조하세요. QR 코드는 관리 패널 대시보드에서 확인할 수 있습니다.

kiro2api는 주로 개인이 유지 관리하며, 코드, 문서, 수정 또는 PR을 통한 참여를 환영합니다.

**기여 방법:**

1. 이 저장소를 포크하기
2. 브랜치 생성 `git checkout -b feature/your-feature`
3. 코드 커밋 `git commit -m "feat: add something"`
4. 푸시 및 풀 리퀘스트 생성

---

## 🙏 감사의 말

[Issues](https://github.com/xwteam/kiro2api/issues)에서 버그 재현, 로그, 호환성 피드백, 기능 제안을 제출해 주신 모든 사용자에게 감사드립니다. 이러한 피드백이 계정 풀, 토큰 자가 치유, 엔드포인트 폴백, 다중 프로토콜 호환, 웹 패널 등 핵심 기능의 발전을 직접적으로 이끌었습니다.

---

## 📄 라이선스

이 프로젝트는 [MIT 라이선스](../../LICENSE)를 채택합니다:

- **허용**: 개인 학습, 연구, 자체 배포, 2차 개발
- **요구**: 저작권 및 라이선스 고지 유지

이 프로젝트는 Amazon / AWS / Kiro와 무관합니다. 사용자는 스스로 위험을 부담하고 관련 서비스 약관을 준수해야 합니다.

---

<div align="center">
  <sub>Built with Rust + axum + tokio | Powered by Kiro (CodeWhisperer)</sub>
</div>
