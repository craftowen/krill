# 도메인·소개 사이트·아티팩트 공유 (`krill.page` / `share.krill.page`)

Krill의 공식 소개·문서 사이트(`https://krill.page`)와 HTML·Markdown 아티팩트 공개 공유 서비스(`https://share.krill.page`), 그리고 로컬 CLI(`krill artifacts`)의 아키텍처·와이어 규약·운영 방법을 정리한다.

---

## 1. 인프라 및 도메인 요약

| 구분 | 값 | 비고 |
|---|---|---|
| **Cloudflare 계정** | 메인테이너 계정(계정 이메일·ID는 비공개) | 도메인은 Cloudflare Registrar로 등록 |
| **Zone** | `krill.page` | `rachel.ns.cloudflare.com`, `viddy.ns.cloudflare.com` |
| **소개·문서 사이트** | `https://krill.page`, `https://www.krill.page` | Worker: `krill-site`, 소스 `cloud/site/`(저장소 밖, 비공개) |
| **아티팩트 공유 서버** | `https://share.krill.page` | Worker: `krill-share`, 소스 `cloud/share/`(저장소 밖, 비공개) |
| **D1 데이터베이스** | `krill-artifacts` | 메타데이터·소유/수정 토큰 해시·만료 시각 저장, 소스 `cloud/share/schema.sql`(저장소 밖, 비공개) |
| **R2 버킷** | `krill-artifacts` | 아티팩트 원본 본문 저장 (`artifacts/<slug>`) |

---

## 2. `krill artifacts` CLI 사용법

구현 위치: [`crates/krill/src/artifacts.rs`](../crates/krill/src/artifacts.rs)

```sh
# 1. HTML 또는 Markdown 파일 공개 공유 (이미 공유된 파일이면 동일 링크를 자동 덮어씀)
krill artifacts share ./report.html [--title "성능 리포트"] [--api-url <url>] [--json]
krill artifacts share ./notes.md --json

# 2. 이전에 공유한 로컬 파일의 아티팩트 강제 갱신
krill artifacts update ./report.html [--title "새 제목"] [--api-url <url>] [--json]

# 3. 로컬 파일과 연결된 공개 링크 회수(삭제)
krill artifacts unshare ./report.html [--api-url <url>] [--json]

# 3-1. 로컬 파일의 공유 상태 확인(네트워크 없이 로컬 기록만)
krill artifacts status ./report.html [--api-url <url>] [--json]

# 4. 내 로컬 토큰이 소유한 공유 아티팩트 목록 조회 (커서 페이지네이션)
krill artifacts list [--cursor <cursor>] [--api-url <url>] [--json]

# 5. 원본 파일 없이 슬러그(ID)로 직접 삭제
krill artifacts delete <slug> [--api-url <url>] [--json]
krill artifacts rm <slug> [--json]
```

### 공개 게이트 (기본 꺼짐)

- `share`·`update`는 `<state_dir>/artifact-sharing.enabled` 파일이 있을 때만 동작한다. 없으면 업로드하지 않고 `artifact_sharing_disabled` 오류로 끝난다. 에이전트만이 아니라 이 기기 전체에 적용된다.
- 켜고 끄는 곳은 앱 **설정 > 아티팩트 > 공개 공유 허용** 스위치다. CLI에는 켜는 명령이 없다. 게이트 정의: [`crates/krill-proto/src/artifacts.rs`](../crates/krill-proto/src/artifacts.rs).
- **정책 게이트일 뿐 보안 경계가 아니다.** 상태 디렉터리에 쓸 수 있는 프로세스(같은 사용자로 도는 에이전트 포함, 세션에는 `KRILL_STATE_DIR`이 들어 있다)는 파일을 직접 만들어 우회할 수 있다. 에이전트에게는 내장 `krill` 스킬이 `artifact_sharing_disabled`를 받으면 재시도·우회하지 말고 사용자에게 묻도록 안내한다.
- **업그레이드 영향**: 이 게이트가 들어간 버전부터 `krill artifacts share`·`update`는 기본으로 거부된다. 앱 없이 CLI만 쓰는 환경에서는 `touch "<state_dir>/artifact-sharing.enabled"`(0600 권장)로 사람이 직접 켠다. 같은 변경으로 지원하지 않는 확장자의 종료 코드가 2에서 1로 바뀌었고, 경로가 붙은 `--api-url`(예: `https://host/prefix`)은 거부된다.
- `list`·`unshare`·`delete`·`status`는 게이트와 상관없이 동작한다(이미 만든 링크를 회수할 수 있어야 한다). 게이트를 꺼도 기존 링크는 지울 때까지 남는다.

### `--json` 출력

- 성공 형태는 `krill_proto::artifacts`의 `ShareOutcome`(`share`·`update`), `LinkStatus`(`status`), `ListPage`(`list`, 서버 응답 그대로), `{deleted, slug}`(`unshare`·`delete`)다.
- 실패도 stdout에 `{"error":{"code","message","nextSteps"?}}`로 내고 비0으로 끝난다. 코드: `artifact_sharing_disabled`, `not_shared`, `unsupported`, `empty`, `too_large`, `unreadable`, `invalid_argument`, 서버 오류 코드(없으면 `http_<status>`), 그 밖 `failed`.

### 앱 UI

- **사이드바 진입점**: 메인 창 왼쪽 사이드바의 '에이전트 대시보드' 바로 아래 '아티팩트' 항목을 누르면 아래 페이지가 열린다.
- **설정 > 아티팩트**(메인 창 안 페이지): 공개 공유 허용 스위치, 사용 방법, 공유한 아티팩트 목록(이름·종류·만료, 링크 복사·브라우저에서 열기·두 번 눌러 삭제, 새로고침·더 보기). 목록·삭제는 `krill artifacts list|delete --json`을 하위 프로세스로 부른다.
- **파일 머리 공유 버튼**: 열린 `.html`·`.htm`·`.md`·`.markdown` 파일 머리에 있다. 누르면 `krill artifacts share <file> --json`으로 링크를 만들거나 같은 링크의 내용을 갱신하고 링크를 클립보드에 복사한다. 저장 안 된 변경이 있으면 저장하라고 안내하고, 공개 공유가 꺼져 있으면 설정 > 아티팩트를 연다.

- 앱이 부르는 CLI는 `KRILL_STATE_DIR`만 넘기고 `KRILL_ARTIFACTS_AUTH_TOKEN`은 넘기지 않는다. 셸에서 토큰을 재정의했다면 앱 목록과 터미널 목록이 다를 수 있다.

### 동작 및 로컬 상태 파일

- **지원 확장자·용량**: `.html`, `.htm`, `.md`, `.markdown` 단일 파일(최대 2 MiB, 빈 파일 거부). 상대 경로 로컬 에셋은 업로드하지 않으므로 단일 완결형 HTML이거나 절대 URL 에셋을 사용한다.
- **기본 서버 주소**: `https://share.krill.page` (`--api-url <url>` 플래그 또는 `KRILL_ARTIFACTS_API_URL` 환경변수로 재정의 가능; HTTPS origin만, 로컬 개발 시에만 `http://127.0.0.1`·`http://localhost` 허용. 경로·자격증명(`user@`)·쿼리는 거부).
- **계정 없는 로컬 인증 (0600 권한)**:
  - `<state_dir>/artifact-token`: 최초 실행 시 256-bit 무작위 소유자 토큰을 자동 생성해 보관한다(`KRILL_ARTIFACTS_AUTH_TOKEN`으로 재정의 가능).
  - `<state_dir>/artifact-shares.json`: 정규화된 로컬 파일 경로(`canonical_path`) ↔ `{ slug, edit_token, share_url, api_origin, expires_at }` 매핑을 저장한다.
  - 외부 프로세스 실행 시 토큰이나 본문이 프로세스 인자(`ps`)에 노출되지 않도록 임시 `0600` 헤더 파일과 `stdin`으로 `curl`을 구동한다.

---

## 3. 아티팩트 서버 (`share.krill.page`) API 및 보안 모델

구현 위치: `cloud/share/src/index.ts`(저장소 밖, 비공개)

### 3.1 REST API (`/v1/artifacts`)

Orca 아티팩트 클라이언트 규약과 호환된다.

| 메서드 및 경로 | 인증 헤더 | 설명 |
|---|---|---|
| `POST /v1/artifacts` | `Authorization: Bearer <token>` (생략 시 임시 토큰 발급), 선택 `Idempotency-Key` | `{ content, contentType, fileName, title? }` 업로드. 90일 만료 설정 후 `{ change: "created", artifact, editToken, shareUrl }` 반환 |
| `PUT /v1/artifacts/:slug` | `x-krill-edit-token` 또는 `x-orca-edit-token` (또는 소유자 Bearer 토큰) | 본문·제목·만료 시각(+90일) 갱신 후 `{ artifact, shareUrl }` 반환 |
| `DELETE /v1/artifacts/:slug` | 수정 토큰 또는 소유자 Bearer 토큰 | R2 객체 삭제 및 D1 `deleted_at` 마킹 후 `204 No Content` 반환 |
| `GET /v1/artifacts?cursor=...` | `Authorization: Bearer <token>` | 해당 소유자 토큰 해시의 활성 아티팩트 페이지 조회 (`{ artifacts, nextCursor? }`) |

### 3.2 열람 라우트 및 샌드박스 격리

| 경로 | 동작 |
|---|---|
| `GET /` | 브라우저 드래그 앤 드롭·붙여넣기 업로드 웹 포털 및 CLI 사용 안내 |
| `GET /a/:slug` | 아티팩트 렌더링 뷰어 (`?embed=1` 쿼리 시 상단 배너 숨김) |
| `GET /a/:slug/raw` | 원본 UTF-8 텍스트(`text/plain; charset=utf-8`) 반환 |

- **HTML 아티팩트 (`text/html`) 보안 헤더**:
  ```http
  Content-Security-Policy: sandbox allow-scripts allow-forms allow-popups allow-popups-to-escape-sandbox allow-modals allow-downloads;
  X-Content-Type-Options: nosniff
  Referrer-Policy: no-referrer
  ```
  `allow-same-origin`을 제외한 `sandbox` 지시어로 서빙하므로, 업로드된 HTML 내의 JavaScript·차트·인터랙티브 UI는 정상 실행되면서도 `krill.page`나 `share.krill.page`의 쿠키·스토리지에는 접근할 수 없는 불투명 Origin(Opaque Origin)으로 격리된다.
- **Markdown 아티팩트 (`text/markdown`)**:
  서버에서 HTML로 변환하고 `<script>`·`<iframe>`·인라인 이벤트 핸들러(`on*=`)·`javascript:` URI를 제거한 뒤 다크/라이트 스타일 뷰어로 응답한다.

---

## 4. 공식 소개·문서 사이트 (`krill.page`)

구현 위치: `cloud/site/`(저장소 밖, 비공개)

- **런타임 의존성 0**: HTML·셸 스크립트를 Wrangler Text 모듈로 번들하여 콜드 스타트를 줄인다(응답 시간 추정, 미측정).
- **제공 경로**:
  - `GET /`: 제품 소개 랜딩 페이지 (한/영 토글, 실제 Claude Code 세션을 연 macOS 앱 캡처, 3계층 데몬 생존 아키텍처, 오케스트레이션 DAG, 아티팩트 공유 안내, CLI 레퍼런스)
  - `GET /docs` (`/docs/*`): 빠른 시작·설치·아키텍처·CLI 전체 레퍼런스 문서
  - `GET /install.sh`: macOS용 `curl -fsSL https://krill.page/install.sh | bash` 설치 스크립트
  - `GET /install.ps1`: Windows x64용 `irm https://krill.page/install.ps1 | iex` 설치 스크립트. 최신 Windows 릴리스 ZIP·SHA-256이 있어야 설치하며, 홈페이지는 배포 전까지 준비 중으로 표시한다.
  - 설치 안내는 접속 환경에 따라 macOS·Windows·Linux·모바일 탭을 선택하며 직접 전환할 수도 있다. Linux는 원격 호스트 CLI 설치, 모바일은 Mac 앱 QR 연결을 안내한다.

---

## 5. 배포 및 검증 명령

`cloud/`는 공개 저장소에 없다(gitignore). 1~3은 메인테이너의 비공개 작업 트리에서만 실행한다.

```sh
# 1. 소개 사이트 배포 (krill.page, www.krill.page)
(cd cloud/site && npx wrangler deploy)

# 2. 아티팩트 서버 D1 스키마 적용 및 배포 (share.krill.page)
(cd cloud/share && npx wrangler d1 execute krill-artifacts --remote --file=schema.sql)
(cd cloud/share && npx wrangler deploy)

# 3. 아티팩트 서버 라이브 E2E 검증
(cd cloud/share && ./e2e.sh https://share.krill.page)

# 4. Rust CLI 테스트
cargo nextest run -p krill artifacts
```
