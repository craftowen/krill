# Krill 모바일 외부 접속(LTE) 개선 조사 보고서 (boost 2차 검증본, 2026-10-18)

> 요청: "처음 설치한 사람이 폰으로 외부에서 쉽게 연결" — 최적 방식으로, 비용 부담 없이, 성능 최고. 1차 독립 조사 → 2차 전수 검증·정정. 측정: 2026-10-18 17시 KST, 이 맥(LG U+ 유선). Quick Tunnel 7개를 임시로 띄워 측정 후 모두 종료.

**결론 — 가장 효과 큰 3가지**
1. **Quick Tunnel 준비 확인 뒤 QR 발행 + 감시·재기동 + 모드 영속.** cloudflared가 URL을 찍은 뒤에도 1.8~34초 동안 HTTP 530(Origin DNS error)이 났다(5회). 현재 코드는 URL이 찍히는 즉시 QR을 낸다. 원격 작업 불필요.
2. **연결 유지**: 서버 ping, 인증 후 타임아웃, 연결 상한 분리, PWA 자동 재접속. 유휴 WS는 약 125초에 끊겼고(1회), 상한 4개가 인증 연결까지 포함한다. 원격 작업 불필요.
3. **고정 출처 PWA + DO 랑데부(D안)** — 단 출처 위치가 중요하다. `krill.page`는 12회 중 8회 SJC였고 `*.pages.dev` 프로젝트는 12/12 ICN. `ws.rs`의 Origin 검사가 교차 출처 WS를 막으므로 수정이 필요하다. 배포(원격 작업) 필요.

---

## 1. 1차 조사 정정 (주장 / 실제 / 근거)

| # | 주장 | 실제 | 근거 |
|---|---|---|---|
| 1 | "미인증 4개·5초" | `MAX_CONNECTIONS=4`는 인증 연결까지 포함한 전체 상한. 5초는 미인증 전용. PWA 1대가 WS 2개(메인+터미널)를 쓰므로 폰 2대에서 포화 | gateway.rs:19, 325, 329 |
| 2 | krill.page connect 중앙값 ≈52ms, HKG·NRT 위주 | 재측정 12회: SJC 8, HKG 3, NRT 1. connect 49~276ms(중앙값 약 181ms), TTFB 159~686ms. 시간대마다 다름. "ICN이 아니다"는 결론은 더 강해짐 | curl `/cdn-cgi/trace` 12회 |
| 3 | trycloudflare.com ICN 10/10 | ICN 12/12, connect 11~45ms. 맞음 | 동일 |
| 4 | Quick Tunnel 하위 도메인 colo 미측정 | 하위 도메인 ICN 12/12(connect 11~24ms). cloudflared 로그 `location=icn06/icn07`로 커넥터 ICN 확인 | 실제 터널 |
| 5 | D안 e2e ≈45~70ms(추정) | Mac→ICN→cloudflared(Mac) WS echo 중앙값 30~40ms, 최소 24ms, 최대 110ms. 폰 LTE 구간은 미측정 | WS echo 측정 |
| 6 | (누락) | Quick Tunnel 경유 일반 HTTP는 연결을 재사용해도 300~450ms(27회). WS 업그레이드는 59~90ms. 에셋 로드 비용이 크다 | curl 반복 |
| 7 | F5 NXDOMAIN 캐시 가능(추정) | 확인. URL 출력 뒤 권한 DNS가 약 2.4초간 NX, 약 4.4초까지 NX와 정상이 번갈아 나옴. 음성 캐시 TTL 60초 | dig |
| 8 | "Registered tunnel connection 이후 발행"이면 충분 | 틀림. HTTP 200까지 URL 출력 후 1.8/2.2/2.8/31.8/34초. 실제 HTTP 프로브가 필요 | 5회 |
| 9 | 유휴 종료 약 100초(3자 자료) | 공식 문서에는 수치 없음. Quick Tunnel 경유 실측 약 125초(1회) | idle 테스트 |
| 10 | cloudflared RSS 미측정 | phys_footprint 20~21MB, RSS 23~25MB(데몬 예산 밖 별도 프로세스) | footprint |
| 11 | pages.dev는 루트만 ICN | 실제 `*.pages.dev` 프로젝트 2개 ICN 12/12, TTFB 85~183ms. workers.dev Worker는 HKG/NRT | curl trace |
| 12 | iOS 홈 화면 PWA는 7일 삭제 "면제" | 면제가 아니라 별도 카운터. 더 큰 문제는 홈 화면 앱과 Safari가 저장소를 공유하지 않는 것(WebKit 181849, "by design") | webkit.org |
| 13 | cloudflared 25MB, gzip 9.3MB, ad-hoc | 맞음. 공식 `cloudflared-darwin-arm64.tgz`(2026.10.1) 19.8MB, GitHub API가 sha256 제공 | 실측 |
| 14 | B안 Hibernation 없으면 N=100 ≈$410 | 공식 단가로 재계산하면 상시 연결 가정에서 ≈$405. 가정 의존 추정 | DO 요금 문서 |
| 15 | 인용 파일:줄 | 모두 원본과 일치 | 직접 열람 |
| 16 | 데몬은 항상 Lan으로 시작 | 맞음. 추가로 재시작 뒤 활성 기기가 있으면 Tunnel을 골랐어도 LAN 사설 IP 리스너가 열림 | gateway.rs:95, 286-306 |

## 2. 이미 적용된 것

E2EE v2, Lazy Exposure, 미인증 5초·전역 실패 예산 5회/60초, 회수 시 WS 종료, QR fragment 키 즉시 삭제, cloudflared 인자에 비밀 없음·Drop 정리, 딥링크에 비밀 없음, WS ping 응답·TCP_NODELAY, gzip 사전 압축 에셋(`app.js` = crypto+terminal+client 연결), 메인 WS 2.5초 폴링, 엄격한 trycloudflare URL 파서.

## 3. 개선안 (우선순위순)

| 순위 | 항목 | 근거 | 효과 | 난이도 | 위험 | 원격 |
|---|---|---|---|---|---|---|
| 1 P0 | 터널 HTTP 준비 프로브 후 URL 발행 | tunnel.rs, gateway.rs MintPairingTicket | 1.8~34초 530 구간·NX 음성 캐시로 인한 첫 스캔 실패 제거 | 중 | curl 서브프로세스 의존 | 불필요 |
| 2 P0 | 터널 기동을 락 밖에서 | gateway.rs handle | 최대 15초+ 동안 accept·Status·회수가 멈추는 것 제거 | 중 | 회수·모드 변경 경합 → Ready 시점 의도 재확인 | 불필요 |
| 3 P0 | 연결 모드·custom 영속(별도 0600 파일) | gateway.rs:95 | 재시작 뒤 Tunnel 기기 복구, 의도하지 않은 LAN 리스너 방지 | 하 | 기기 파일 형식은 유지 | 불필요 |
| 4 P0 | cloudflared 감시·재기동(백오프) | gateway.rs reconcile, tunnel.rs running | 터널이 죽어도 자동 복구(URL은 바뀜) | 하~중 | 재기동 폭주 → 백오프 | 불필요 |
| 5 P0 | 서버 ping 30초 + 인증 후 유휴 감시 + 연결 상한 분리 | gateway.rs:19, 329, bridge | 125초 유휴 종료 방지, LTE 전환 좀비 연결 정리 | 중 | 중간 프레임 타임아웃 → peek로 프레임 경계에서만 판단 | 불필요 |
| 6 P0 | PWA 자동 재접속 | client.js, terminal.js | 화면 복귀·망 전환 뒤 수동 새로고침 불필요 | 중 | 전역 실패 예산에 스스로 걸림 → 백오프·인증 실패 시 중단. 5번 선행 | 불필요 |
| 7 P1 | Service Worker cache-first | pwa.rs no-store | 다시 열 때 0.6~0.9초 단축(추정) | 하 | https 출처에서만. 고정 출처(8번) 없이는 효과가 짧음 | 불필요 |
| 8 P1 | D안: 고정 출처 PWA + DO 랑데부 | ws.rs Origin, gateway.rs, client.js | 재스캔 제거, 홈 화면 아이콘 지속 | 상 | 출처 colo, 코드 공급 신뢰 이동, 버전 불일치, Quick Tunnel 정책 | **필요** |
| 9 P2 | cloudflared를 krilld 재시작 너머로 유지 | tunnel.rs Drop, net.rs 포트 0 | URL 변경 빈도 감소 | 중 | 고아 프로세스·pid 재사용 | 불필요 |
| 10 P2 | accept 50ms 폴링 → rustix poll | gateway.rs accept | 페어링 사용자 상시 초당 20회 깨어남 제거 | 중 | 리스너 교체 시 깨우기 | 불필요 |
| 11 P2 | 재접속마다 fsync 제거 | devices.rs seen | 연결마다 파일 재작성 제거 | 하 | 크래시 시 last_seen만 손실 | 불필요 |
| 12 P2 | 공개 URL 잠금 DoS 완화 | gateway.rs attempt | 제3자가 WS 5개로 60초 잠금 거는 것 완화 | 중 | Cf-Connecting-Ip 신뢰 범위 | 불필요 |
| 13 P2 | 터미널 WS 합치기 / 핸드셰이크 파이프라이닝 | client.js, terminal.js | 터미널 열기 4~5 RTT 제거(추정) | 상 | 프로토콜 변경 | 불필요 |
| 14 P1 | iOS 홈 화면 앱 안에서 페어링(코드 입력·앱 내 스캔) | client.js, manifest.json | Safari 저장소 분리로 인한 자격증명 소실 해결 | 중 | 8번 필요 | 8번과 함께 |
| 15 P3 | cloudflared 자동 확보(공식 tgz+sha256 또는 DMG 번들) | tunnel.rs, packaging/ | brew 없는 사용자도 사용 | 중 | 공식 바이너리 서명·공증 미확인 | 배포 결정(사람) |

### 3-1. D안 세부

- **(a) Origin·CSP**: cloudflared는 `Origin: https://krill.page`를 그대로 전달하고 `Host`는 `*.trycloudflare.com`. 현재 `ws.rs`는 `origin == http(s)://{host}`만 허용하므로 교차 출처 PWA는 거부된다. Tunnel 모드에서만 고정 출처를 허용 목록에 추가. 인증이 challenge·HKDF·AEAD라 Origin 완화의 보안 영향은 작다. https 출처는 `ws://<LAN IP>`에 접속할 수 없으므로 LAN/Tailscale은 지금처럼 krilld가 직접 서빙.
- **(b) Quick Tunnel 정책**: 공식 문서 — "Quick Tunnels are for testing and development." "For production, create a Cloudflare Tunnel." "no uptime guarantee", 동시 요청 200개, SSE 미지원. 명시적 금지는 없지만 제품 기본 경로로는 용도 불일치 → 퇴로(B안 릴레이) 유지 필요. trycloudflare는 악성코드 유포에 대량 악용된 이력(Proofpoint 2024)이 있어 기업망 차단 가능성.
- **(c) 랑데부 없는 대안**: 계정 없이 무료인 고정 수단은 없음. 단계적으로 ① 9번으로 URL 변경 빈도 감소 → ② 고정 출처만으로 자격증명 유지(URL만 재스캔) → ③ DO 랑데부로 재스캔 제거. KV는 최종 일관성이라 Mac 재시작 직후 낡은 URL을 줄 수 있어 DO 권장. 캐시 URL 우선 접속·실패 시만 조회하면 비용 사실상 $0(추정).
- **(d) 버전 불일치**: 릴리스마다 내장 에셋 해시로 `/m/<hash>/` 불변 경로 보존, QR·랑데부에 해시 포함, 없는 해시는 터널 출처로 강등. 로더만 Worker에 두고 `app.js`를 터널에서 받는 변형은 피싱 링크로 자격증명 탈취가 가능하므로 금지. `?pair=`는 fragment로 이동. 새 신뢰 대상(krill.page 계정·배포 파이프라인) → 2FA·권한 최소화.
- **(e) 단계0 범위**: 1~7, 10~13번은 `crates/krilld/src/mobile/` 안에서 끝나고 새 크레이트 0개. 순서 3→2→1→4→5→6→7→10→11. 5번이 6번보다 먼저.

## 4. 미확인 항목

폰 LTE 3사 colo·RTT, pages.dev Functions·DO 경로 colo, Pro 플랜 라우팅, 망 전환 시 Quick Tunnel URL 유지 여부, HTTP 300~450ms 원인, 유휴 종료·터널 준비 시간 분포(표본 1회·5회), 공식 cloudflared 서명·공증, iOS 최신 버전 저장소 복사 여부, 국내망 trycloudflare 차단 여부, 폰 기준 e2e 합계(구성 요소만 실측).

**출처**: [Quick Tunnels](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/do-more-with-tunnels/trycloudflare/) · [Cloudflare WebSockets](https://developers.cloudflare.com/network/websockets/) · [Durable Objects 요금](https://developers.cloudflare.com/durable-objects/platform/pricing/) · [WebKit 블로그 10218](https://webkit.org/blog/10218/full-third-party-cookie-blocking-and-more/) · [WebKit 181849](https://bugs.webkit.org/show_bug.cgi?id=181849) · [Proofpoint](https://www.proofpoint.com/us/blog/threat-insight/threat-actor-abuses-cloudflare-tunnels-deliver-rats)
