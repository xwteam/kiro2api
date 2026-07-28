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
  <img src="https://img.shields.io/badge/version-v0.7.0-success?style=flat-square" alt="Version">
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

> 전체 변경 로그는 [CHANGELOG.md](../../CHANGELOG.md)를 참조하세요.

| 날짜 | 업데이트 내용 |
|------|----------|
| 2026-07-28 | v0.7.0 - 토큰 갱신 실패가 완전히 삼켜지고 있었습니다. 로그에는 「갱신 중」 직후 「다른 계정으로 재시도」만 찍히고 **왜 실패했는지**가 통째로 빠져 있었습니다. 실제 장애에서 업스트림이 계정 전체에 `access_denied`를 반환했지만 화면에는 「전부 만료됨」으로만 보여, 수동으로 업스트림에 curl하지 않으면 릴레이 자체 고장과 구분할 수 없었습니다. 이제 실패 시 업스트림 상태 코드와 본문을 기록하고 `statusReason`에 새 구간 `refresh_denied`로 반영합니다. 아무리 재시도해도 해결되지 않으므로 단순 만료와 엄격히 구분합니다. 계정 페이지에는 「이 페이지 전체 선택」과 「일괄 비활성화」도 추가 |
| 2026-07-28 | v0.6.0 - 계정 목록이 30초마다 조용히 자동 새로 고침되며 신선도 라벨이 붙습니다. 기존에는 페이지를 연 시점에서 멈춰 있어, 계정이 정지되거나 쿨다운에서 복귀하거나 만료되어도 수동으로 새로 고치기 전까지 화면이 그대로였습니다. 오래된 배지를 보고 판단하는 것은 배지가 없는 것보다 나쁩니다 — 「정지됨 (0)」 같은 숫자는 결론처럼 보이지만 10분 전 값일 수 있습니다. 다시 가져오는 것은 저렴한 목록 엔드포인트뿐이며, 잔액 팬아웃을 타이머로 재실행하지는 않습니다. 조용한 새로 고침은 페이지·필터·선택·스크롤 위치를 유지하고, 툴바에 숫자가 몇 초 전 것인지 표시합니다 |
| 2026-07-28 | v0.5.1 - 헬스 배지와 새 상태 필터가 서로 다른 판단을 했습니다: 필터는 v0.5.0의 분류를 쓰는데 배지는 `healthStatus`만 보고 있어 「만료됨」 구간의 행에 초록색 「정상」 배지가 그대로 달렸습니다. 이제 둘 다 같은 분류에서 나옵니다. 한도 소진도 「선택되어 실제로 실패한」 경우에만 감지되어, 아직 선택되지 않았지만 잔액이 없는 계정을 놓쳤습니다. 잔액 조회 결과 잔여가 0이면 동일하게 한도 소진으로 판정하며(「아직 조회하지 않음」과 엄격히 구분), 잔액이 도착할 때마다 해당 행의 배지와 드롭다운 개수를 갱신합니다 |
| 2026-07-28 | v0.5.0 - 계정 관리에 상태 필터(전체 / 정상 / 이상 / 비활성 / 정지됨 / 만료됨 / 한도 소진, 각 항목에 실시간 개수)를 추가하고 「이상」을 운영자가 실제로 다르게 대응하는 단위로 분리했습니다. 업스트림이 계정을 정지하면 응답 본문에 `suspend`가 담기고 코드도 이를 인식했지만, 「영구 비활성화 대신 쿨다운」 판단에만 쓰고 곧바로 버렸습니다. `GET /api/admin/credentials`의 새 필드 `statusReason`이 마지막 실패 사유를 노출합니다. 정지 판정은 스로틀링보다 우선하며, 분류는 표시 계층에만 영향을 줍니다 |
| 2026-07-28 | v0.4.0 - 프로토콜 측 `/models`가 제공 가능한 17개 모델을 모두 나열하며 세 프로토콜의 결과가 일치합니다. 기존에는 `GET /v1/models`·`GET /claude/v1/models`·`GET /v1beta/models`가 각각 **서로 다른 3개**를 하드코딩했고 관리 엔드포인트는 17개였습니다 — 「모델 목록을 받아 그 id로 호출」이라는 표준 흐름이 불완전한, 게다가 프로토콜마다 다른 집합을 반환했습니다. 이제 단일 카탈로그(`src/models_catalog.rs`)가 네 엔드포인트를 모두 뒷받침하며, 카탈로그의 모든 id가 `map_model`로 해석되는지와 세 목록이 일치하는지를 테스트가 보장합니다. API 레퍼런스에 한 번도 없던 12개 라우트도 추가 |
| 2026-07-28 | v0.3.1 - `POST /v1/messages/count_tokens`가 잘못된 요청 본문에 대해 Anthropic 오류 객체가 아니라 axum 기본 평문 `422`를 반환했습니다. v0.3.0에서 4개 대화 엔드포인트에는 명시적 거부 처리를 넣었지만, 동일한 Anthropic 프로토콜에 속하며 SDK가 직접 호출하는 이 엔드포인트만 누락되었습니다 —— 평문 본문에 `response.json()`을 쓰면 파싱 예외만 발생하고 실제 실패 원인은 사라집니다 |
| 2026-07-28 | v0.3.0 - 🔍 v0.2.1 자체 수정에 대한 독립 재검증. 확인된 39건 중 **9건은 일부만 막혔는데도** 완료로 공지되었고, **13건의 후보는 아예 판정된 적이 없었다**(검증 에이전트가 도중에 중단됨). 그중 12건이 실제 결함으로 확인되어 본 버전에서 21건을 모두 막았다. 가장 중대한 것은 v0.2.1이 「이제 실제로 적용된다」고 공지한 API-KEY 자격증명 바인딩이 **한 번도 적용된 적이 없었다**는 점이다 — 게이트가 화이트리스트를 요청 확장에 넣기만 했을 뿐 하위 어디에서도 읽지 않아, 특정 계정에 묶인 키도 풀의 아무 계정으로나 처리되었다(모든 프로토콜 공통). 그 밖에: 클라이언트 IP는 여전히 위조 가능했고(`X-Forwarded-For`의 최좌측 항목, 즉 호출자가 직접 쓸 수 있는 항목을 채택했음), `api_keys.json` 손상 시 `next_id`가 0으로 돌아가 새 키가 이전 사용자의 사용 내역과 누적 지출을 그대로 물려받았으며, 종료 시 잔액 캐시와 이벤트 로그가 유실되었고, 업스트림 오류 본문이 저장되지 않아 패널의 실패 상세가 항상 비어 있었다. `temperature`·`max_tokens`·`tool_choice` 세 파라미터는 문서에만 있고 실제로는 무시되므로 그대로 명시했다. v0.2.1이 바인딩에 대해 작성한 회귀 테스트는 「값이 요청 확장에 도달했는지」를 검증했을 뿐 「계정 선택이 이를 따르는지」는 검증하지 않았다 — 죽은 기능이 테스트 전부 통과 상태로 출시된 이유가 이것이다. 이번 라운드에 추가한 모든 테스트는 수정 전 코드에서 실패하는 것을 직접 확인했다 |
| 2026-07-27 | v0.2.1 - 🛠 후속 감사 수정: 적대적 검토로 확인된 39건의 문제(한 번도 감사한 적 없던 패널과 문서 포함). 보안: 비밀 정보가 담긴 파일(`api_keys.json`, `config.json`)이 누구나 읽을 수 있는 권한으로 저장되고, 수동으로 `chmod`해도 flush 때마다 조용히 다시 넓어지던 문제; 포트에 직접 닿을 수 있는 사람이면 누구나 클라이언트 IP를 위조할 수 있던 문제; 저장만 되고 실제로는 적용되지 않던 API-KEY의 자격 증명 바인딩. 수정: 대시보드를 열 때마다 `GET /api/admin/models`가 계정 풀 전체에 무제한 업스트림 조회를 유발하던 문제(이제 단일 실행·상한·쿨다운 적용); 손상된 자격 증명 파일을 빈 풀로 간주한 뒤 덮어써 모든 계정이 사라지던 문제(이제 백업 후 항목 단위로 복구); 종료 시 API-KEY 변경 사항이 유실되던 문제; OpenAI 병렬 도구 호출이 잘못된 도구 왕복을 만들던 문제; 일부 Gemini 페이로드(내장 도구, snake_case 키, 이미지가 아닌 `inlineData`)가 거부되거나 손상되던 문제; 2 MB 본문 제한이 약 1.5 MB 이미지를 거부하던 문제; 그 외 다수의 관리자/사용자 패널 수정 |
| 2026-07-26 | v0.2.0 - 🛠 전체 체인 감사 수정: API-KEY 지출 한도가 **4개 프로토콜 전부**에 적용(이전에는 Anthropic 엔드포인트에서만 유효하여 나머지 3개는 무제한 소비되고 사용량도 0으로 표시됨); 사용자용 API-KEY만 설정된 경우에도 관리 인터페이스가 더 이상 개방되지 않음; 업스트림 오류·스트림 도중 전송 중단·잘림을 어느 프로토콜에서도 정상 완료로 보고하지 않음; 계정 풀 갱신 실패가 풀에 그대로 반영됨; 재시작해도 사용량/과금이 유실되지 않고 원장 파일이 롤백에 안전하게 유지됨; `--credentials`와 `PORT`를 반영하는 헬스 체크가 실제로 동작 |
| 2026-07-26 | v0.1.4 - 🐛 수정: Anthropic `system` 필드가 콘텐츠 블록 배열(문자열뿐 아니라)을 지원 — Claude Code / 프롬프트 캐싱 SDK가 배열로 보내도 더 이상 422가 발생하지 않음 |
| 2026-07-26 | v0.1.3 - 일괄 JSON 가져오기가 이제 계정별 실시간 진행 상황을 표시: 진행률 바, 실시간 성공/중복/실패 통계, 계정별 상태 목록(검증 중 → 사용량과 함께 검증 완료 / 중복 / 실패 후 롤백); 검증된 계정은 즉시 저장되므로 가져오기 도중 중단해도 유실되지 않음 |
| 2026-07-25 | v0.1.2 - 업데이트 대화상자 개편: 업데이트 확인 대화상자가 현지화된 릴리스 노트 + 복사 가능한 업그레이드 명령을 표시, 업데이트가 있으면 버튼이 「vX로 업데이트」로 강조, 순수 HTTP 환경에서 복사 버튼 오류 수정 |
| 2026-07-25 | v0.1.1 - 패널 및 계정 가져오기 수정: 모델 테스트가 마스터 API 키로 폴백, 일괄 가져오기를 항목별 「생존 검증 + 중복 제거」로 전환, 대량 목록에서 일괄 가져오기 실패 수정, 사용자 패널/전체 페이지 favicon + 128x128 로고 및 각 README의 버전 배지, 크로스 컴파일 멀티 아키텍처 이미지 빌드 |
| 2026-07-25 | v0.1.0 - 🚀 첫 릴리스: 4개 프로토콜 프런트엔드(Anthropic 중추 + OpenAI / OpenAI-Responses / Gemini), Kiro 계정 풀(다중 계정 라운드로빈 / 단계별 쿨다운 / 토큰 자가 치유), 엔드포인트 폴백 및 크로스 계정 재시도, 통합 인증 게이트, `/admin` 관리 패널과 `/user` 사용자 패널, 일별/계정별 사용량 통계, 실패/스로틀 로그, 계정 잔액 캐시, 실시간 로그(SSE), 3가지 대화형 로그인 플로우, Docker 멀티 아키텍처(amd64/arm64) 배포 및 CI |

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
# {"service":"kiro2api","status":"ok","version":"0.7.0"}

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
