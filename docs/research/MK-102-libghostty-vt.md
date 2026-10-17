# MK-102: libghostty-vt 재평가와 이미지 프로토콜 (OWE-467)

**결론: 재평가 메모 완료, 엔진 전환과 이미지 지원은 보류한다.** 2026-10-16 확인한 공식 C API는 main·앱 태그 v1.3.1 모두 불안정하며, 소스 빌드는 Zig가 필요하다. 이미 빌드된 라이브러리를 소비하는 경로는 있지만 임의 버전과의 호환성은 보장되지 않는다. Ghostty의 런타임 메모리는 이번에 측정하지 않았으므로 절감률이나 데몬 예산 합격을 주장하지 않는다.

## 범위와 판정

ARCHITECTURE §4·§5의 `alacritty_terminal`·데몬 단독 질의 응답·이미지 미지원 결정을 유지한다. ROADMAP MK-102의 완료 조건인 안정성·Zig 없는 빌드 여부·메모리 비교 메모를 작성하는 조사다. 엔진 교체, 이미지 프로토콜 구현, 새로운 의존성 도입은 하지 않았다. T3#29·#30은 조사 배경이며 규범 문서를 대체하지 않는다.

| 확인 항목 | 확인 결과 | 채택 관점 판정 |
|---|---|---|
| 안정된 C API인가 | 공식 main 및 v1.3.1의 `include/ghostty/vt.h`가 API 불안정·변경 예정을 명시 [S1, S2] | 미충족 |
| Zig 없이 소스에서 빌드 가능한가 | CMake도 Zig를 필수 탐색하고 `zig build`를 실행한다. main의 최소 Zig는 0.16.0 [S3, S4] | 미충족 |
| Zig 없는 소비자 빌드 경로가 있는가 | CMake `find_package` 및 Rust 바인딩의 opt-in `pkg-config`는 설치된 산출물을 사용할 수 있다 [S3, S5] | 경로 있음, 로컬 링크 미검증 |
| 메모리 비교 근거가 있는가 | 셀 표현 비교·계산과 기존 Alacritty 실측을 아래에 분리 기록. Ghostty footprint는 미측정 | 우열 미판정 |
| 이미지 프로토콜을 바로 지원할 수 있는가 | Kitty 이미지 저장·배치·픽셀 조회 API가 있으나 디코더·렌더·IPC·복구 설계가 별도로 필요 [S7, S8] | 구현 보류 |

여기서 API 안정성 미충족은 **채택 게이트의 결과**다. 조사 메모의 누락이나 API 부재라는 뜻은 아니다.

## 조사 기준점과 재현

- 확인일: 2026-10-16 KST. 공식 main 고정 SHA: `22fa134a07f5749edd9e92c6bc17d8aaf61843be`(커미터 시각 2026-10-15 08:22:33 UTC).
- 공식 tags API가 반환한 맨 앞 앱 태그: `v1.3.1`, SHA `332b2aefc6e72d363aa93ab6ecfc86eeeeb5ed28`. **앱 버전과 VT C API 안정 버전은 다르다.** `/releases/latest`는 이번 요청에서 HTTP 404를 반환해 릴리스 산출물 유무를 판정하는 근거로 쓰지 않았다.
- 비공식 Rust 바인딩 `Uzaaft/libghostty-rs` 태그 `v0.2.2`, SHA `a3a967698be27fde0077f8559ca36f852f5885ac`. 이 버전의 `build.rs`가 고정하는 Ghostty SHA는 `a887df42c56f6de86c0fe6da9c4eeca37931e083`이다. 바인딩 v0.2.2를 공식 C API 안정 태그로 해석하지 않는다.
- 로컬: Mac18,5, 메모리 34,359,738,368바이트, macOS 27.0.1(26A434), Homebrew rustc 1.98.1. `command -v zig`는 경로를 반환하지 않았다. 시스템 설정과 툴체인은 바꾸지 않았다.

다음은 실제 수행한 소스 확인을 재현하는 명령이다. 임시 디렉터리에 **CMake 진입 파일 하나**를 받아 Zig 탐색 실패를 관찰한다. 전체 소스 빌드나 라이브러리 링크 테스트가 아니다.

```sh
mkdir -p /tmp/mk102-cmake-source
curl -fsSL https://raw.githubusercontent.com/ghostty-org/ghostty/22fa134a07f5749edd9e92c6bc17d8aaf61843be/CMakeLists.txt \
  -o /tmp/mk102-cmake-source/CMakeLists.txt
cmake -S /tmp/mk102-cmake-source -B /tmp/mk102-cmake-build
```

관찰: AppleClang 21.0.0.21000334로 C 컴파일러 확인 후 `CMakeLists.txt:94 (find_program)`에서 `ZIG_EXECUTABLE`을 찾을 수 없다는 오류로 configure 실패. 이는 [S3]의 필수 Zig 탐색과 일치한다. Zig를 설치한 빌드 성공 여부는 시험하지 않았다.

## C API와 배포 경로

main은 터미널·렌더 상태·검색·스냅샷·키/마우스·Kitty 그래픽 API를 공개하지만, 같은 헤더가 변경 가능성을 경고한다 [S1]. 기능이 많아졌다는 사실과 ABI/API 안정 보장은 구분해야 한다. 앱 태그 v1.3.1에도 같은 경고가 있다 [S2].

빌드 경로는 다음처럼 구분한다.

| 경로 | Zig 필요 여부와 제한 |
|---|---|
| 공식 소스 `zig build -Demit-lib-vt` | 빌드 머신에 Zig 필요. main의 `build.zig.zon` 최소 버전은 0.16.0 [S4] |
| 공식 CMake 소스 빌드 / FetchContent | CMake가 Zig를 찾아 호출하므로 Zig 의존성을 없애지 않는다 [S3] |
| 공식 CMake `find_package` | 설치된 라이브러리와 헤더를 소비하는 경로. 라이브러리를 만드는 단계에는 위 조건이 남는다 [S3] |
| Rust 바인딩 v0.2.2 기본 경로 | 고정 Ghostty 소스를 가져와 Zig 빌드. 해당 README 요구 버전은 0.15.x로, 위 main과도 다르다 [S5, S6] |
| Rust 바인딩 v0.2.2 `pkg-config` 기능 | `GHOSTTY_SOURCE_DIR` 미설정이고 설치 라이브러리 탐색에 성공하면 Zig 호출 전에 반환한다. 탐색 실패 시 소스 빌드로 돌아간다 [S6] |

따라서 “Zig 없이 빌드”는 **소비자에게 Zig를 요구하지 않는 사전 빌드 배포**로는 가능성이 있지만, **깨끗한 소스 빌드에서 Zig 제거**는 확인한 경로로 불가능하다. `GHOSTTY_SOURCE_DIR`·`GHOSTTY_ZIG_SYSTEM_DIR`는 소스/패키지 사전 확보에 도움을 줄 뿐 Zig 컴파일러를 없애지 않는다. 설치 라이브러리와 바인딩의 헤더 리비전도 함께 맞춰야 한다 [S5, S6]. macOS arm64 산출물의 확보·서명·재현성과 오프라인 링크 성공은 미검증이다.

## 메모리 비교 메모

### 셀 표현과 계산값

Ghostty main의 내부 셀은 `packed struct(u64)`이며 스타일은 ID로 참조한다. 페이지가 스타일 집합과 grapheme 저장소 등을 별도로 가진다 [S9]. Alacritty 0.26.0 셀은 MK-009에서 24바이트로 측정했다. **8바이트 대 24바이트는 셀 본체 표현 비교이며, 프로세스 메모리 3배 절감을 의미하지 않는다.**

다음은 셀 수 × 본체 크기만 계산한 **추정 하한 비교**다. 화면과 핫 1,000행, 10세션을 전부 채운 조건을 가정한다. 추가 화면·행 헤더·스타일·grapheme·페이지 여유·파서·이미지·할당기·렌더 캐시·IPC·저장소는 제외했다. 특히 Ghostty의 행 제한은 페이지 단위 근사치라 1,000행 설정이 정확히 1,000행 할당을 뜻하지 않는다 [S8].

| 화면·세션 | Alacritty 24B 계산 | Ghostty 8B 계산 | 근거 |
|---|---:|---:|---|
| 120×40, 핫 1,000행, 10개 | 28.56 MiB | 9.52 MiB | `120 × (40 + 1000) × 10 × 크기 / 2^20` |
| 200×50, 핫 1,000행, 10개 | 48.07 MiB | 16.02 MiB | `200 × (50 + 1000) × 10 × 크기 / 2^20` |

Ghostty는 스크롤백 바이트·행 제한을 함께 설정할 수 있으나 페이지 단위로 오래된 내용을 제거하므로 예산은 실제 할당량으로 재확인해야 한다 [S8].

### 기존 실측과 비교 한계

이번에 새로 측정한 런타임 footprint는 없다. 아래 Alacritty 값은 MK-009·OWE-477의 **기존 대역 프로세스 측정**을 재인용했다. OWE-477의 회차별 CSV에서 `krilld_mb + holder_mb` 평균을 다시 계산해 표와 일치하는지 확인했다. 현재 본체 krilld 전체 비용으로 해석하지 않는다.

| 조건(각 세션 16MB 컬러 출력 뒤 유휴) | Alacritty 데몬 합(krilld 대역+holder) | Ghostty 동일 조건 |
|---|---:|---|
| 120×40, 10개 붙음, 시스템 malloc | 39.8 MiB (MK-009, 10회) | 미측정 |
| 200×50, 10개 붙음, 시스템 malloc | 63.2 MiB (MK-009, 10회) | 미측정 |
| 120×40, 10개 붙음, jemalloc decay 0 | 40.4 MiB (OWE-477, 3회) | 미측정 |
| 200×50, 10개 붙음, jemalloc decay 0 | 64.0 MiB (OWE-477, 3회) | 미측정 |
| 200×50, 숨은 9개 강등, jemalloc decay 0 | 18.6 MiB (OWE-477, 3회) | 미측정 |
| 200×50, 10개 붙음, 큰 sync 입력, jemalloc decay 0 | 82.5 MiB (OWE-477, 3회) | 미측정 |

측정은 같은 Mac18,5/macOS 27.0.1, 화면 잠금 상태였으며, 앱은 gpui `runtime_shaders` 개발 구성의 release 빌드였다(MK-009·OWE-477 스파이크 회차별 CSV에서 `krilld_mb + holder_mb` 평균 집계). 대역에는 실제 데몬의 전체 tokio·세그먼트·IPC 비용이 포함되지 않는다. 강등 뒤 반환 불가라는 초기 MK-009 결과만으로 결론 내리지 않는다. OWE-477은 할당기를 바꿔 반환을 확인했고, MK-035는 실제 krilld 라이브러리 테스트에서 강등을 별도 검증했다.

**판단:** Ghostty 셀 압축은 비교 실험의 동기지만 엔진 교체의 측정 근거는 아직 없다. 현재 예산은 엔진 본체만이 아니라 데몬 전체의 `phys_footprint`로 판정해야 한다.

## 이미지 프로토콜

확인한 후보는 **Kitty Graphics Protocol**이다. 공개 API는 이미지 데이터와 placement를 읽어 자체 렌더러에서 그리도록 한다. PNG 전송을 쓰려면 embedder가 디코더 콜백을 제공하며, 이미지 저장 한도를 0으로 설정하면 프로토콜과 저장 이미지를 비활성화한다 [S7, S8]. 이 조사에서 sixel·iTerm2 이미지의 지원 여부는 판정하지 않았다.

향후 채택 실험에는 다음 경계가 필요하다. 이는 구현 완료 항목이 아닌 재평가 조건이다.

- krilld가 이미지 파싱·질의 응답·수명 관리를 소유한다. 앱은 전달받은 이미지와 배치만 그린다. Ghostty를 링크하는 것만으로 gpui 이미지 렌더가 생기지는 않는다.
- 이미지 저장 한도와 APC 수신 한도, 디코딩 중간 버퍼, 앱의 GPU 텍스처까지 함께 계측한다. API는 파일·임시 파일·공유 메모리 전송 제어도 제공한다 [S8]. 데몬 환경에서 허용할 매체는 별도 결정한다.
- 앱 재시작·느린 클라이언트·재접속·krilld 복구 시 이미지/placement의 동기화와 지우기 동작을 정의한다. 현 텍스트 키프레임·델타만으로 이미지 복구가 되는지는 검증하지 않았다.
- 저장소의 zstd 콜드 재생·키프레임·Gap 보장에 이미지 자산을 어떻게 연결할지 결정하고 생존/재생 테스트로 검증한다. 이미지 지원을 광고하거나 성공 응답하기 전에 이 경로가 완성돼야 한다.

## 다음 재평가 조건과 미확인

1. 공식 VT C API 안정 선언/태그와 버전 호환 정책을 확인한다. 바인딩·헤더·라이브러리를 같은 리비전으로 고정한다.
2. 사전 빌드 배포를 택한다면 Zig 없는 깨끗한 macOS arm64 소비자 환경에서 오프라인 빌드·링크를 실증한다. 소스 빌드까지 Zig를 없애야 한다면 현재 게이트는 계속 미충족이다.
3. 두 엔진에 동일 바이트 fixture·120×40/200×50·10세션·핫 0/1,000행·동일 유휴 시간을 적용한다. 엔진 단독과 데몬 합을 나눠 `phys_footprint`·피크·할당량을 측정한다. ASCII·한글/ZWJ·다양한 스타일·대체 화면·큰 sync·리사이즈·강등을 포함한다.
4. Kitty 비활성/활성 각각에서 원본 데이터·디코딩 결과·텍스처·placement·재접속 복제 비용을 포함하고, ARCHITECTURE의 앱/데몬 예산과 재생 보장을 다시 검증한다.

**검토 사항:** 당장 제품 결정을 변경할 필요는 없다. 향후 엔진 도입 시 Zig를 빌드 인프라에 허용할지, 사전 빌드 산출물의 공급망 책임을 맡을지, 이미지 전송 매체·메모리 예산을 어떻게 정할지 승인해야 한다. Ghostty 실측 비교와 이미지 렌더 검증은 위 게이트가 열린 뒤 남는 실험이며 이번 메모에는 결과가 없다.

## 저장소 검증

2026-10-16 `cargo fmt --all` 후 아래 게이트가 종료 코드 0으로 끝났다. nextest는 730개 통과·13개 건너뜀, doctest·cargo-deny도 통과했다. 이 결과는 저장소 회귀 검사이며 Ghostty 빌드나 메모리 측정의 증거가 아니다.

```sh
CARGO_BUILD_JOBS=2 MK_TEST_SLOTS=1 NEXTEST_TEST_THREADS=4 cargo fmt --all
CARGO_BUILD_JOBS=2 MK_TEST_SLOTS=1 NEXTEST_TEST_THREADS=4 scripts/check.sh
```

## 근거 링크(리비전 고정)

- S1: [main VT 헤더](https://github.com/ghostty-org/ghostty/blob/22fa134a07f5749edd9e92c6bc17d8aaf61843be/include/ghostty/vt.h#L10) — 안정성 경고·API 목록.
- S2: [v1.3.1 VT 헤더](https://github.com/ghostty-org/ghostty/blob/332b2aefc6e72d363aa93ab6ecfc86eeeeb5ed28/include/ghostty/vt.h#L10) — 앱 태그에도 남아 있는 안정성 경고.
- S3: [공식 CMake](https://github.com/ghostty-org/ghostty/blob/22fa134a07f5749edd9e92c6bc17d8aaf61843be/CMakeLists.txt#L94) — Zig 필수 탐색·호출, 상단 소비자 예시.
- S4: [공식 build.zig.zon](https://github.com/ghostty-org/ghostty/blob/22fa134a07f5749edd9e92c6bc17d8aaf61843be/build.zig.zon#L6) — 최소 Zig 버전.
- S5: [Rust 바인딩 v0.2.2 README](https://github.com/Uzaaft/libghostty-rs/blob/a3a967698be27fde0077f8559ca36f852f5885ac/README.md#L56) — 빌드 요구·사전 설치 경로·리비전 호환 한계.
- S6: [Rust 바인딩 v0.2.2 build.rs](https://github.com/Uzaaft/libghostty-rs/blob/a3a967698be27fde0077f8559ca36f852f5885ac/crates/libghostty-vt-sys/build.rs#L6) — 고정 SHA, 94~108행의 경로 선택, 144행 Zig 실행.
- S7: [Kitty 그래픽 C API](https://github.com/ghostty-org/ghostty/blob/22fa134a07f5749edd9e92c6bc17d8aaf61843be/include/ghostty/vt/kitty_graphics.h#L22) — 저장/배치/픽셀·디코더 연결.
- S8: [터미널 옵션](https://github.com/ghostty-org/ghostty/blob/22fa134a07f5749edd9e92c6bc17d8aaf61843be/include/ghostty/vt/terminal.h#L2084) — 이미지 매체·수신 한도, 2213행 이후 스크롤백 근사 제한.
- S9: [내부 페이지와 셀](https://github.com/ghostty-org/ghostty/blob/22fa134a07f5749edd9e92c6bc17d8aaf61843be/src/terminal/page.zig#L2137) — 셀 본체와 197~227행의 부가 저장 구조.
