# 공개 릴리스 (`publish-github.sh`)

GitHub Releases 게시의 **공식 경로**는 로컬 맥에서 메인테이너가 돌리는 `packaging/publish-github.sh`다.
`.github/workflows/release.yml`은 수동 예비·Windows 자산 추가용이며 같은 규칙(main 끝 또는 태그 커밋에서만,
기존 자산 덮어쓰기 없음)을 따른다. `git push`·릴리스 생성은 사람이 한다(AGENTS.md 규칙 9).

```sh
bash packaging/publish-github.sh --preflight      # 가드만 확인(빌드·업로드 없음, 실패 시 1)
bash packaging/publish-github.sh --bump patch     # 가드 통과 후 버전·Cargo.lock·THIRD_PARTY_NOTICES.md를 한 커밋으로 만들고 멈춤
#   → 메인테이너가 그 커밋을 main에 올리고 CI가 녹색이 될 때까지 기다린다
bash packaging/publish-github.sh --dry-run        # 조립만(가드 실패는 경고). --bin-dir은 여기서만 허용
bash packaging/publish-github.sh                  # 가드 → 빌드 → 재확인 → 태그 푸시 → gh release create
```

빌드 전에 모든 가드를 보고, 하나라도 실패하면 아무것도 빌드·업로드하지 않는다(v0.1.6 덮어쓰기 사고 대응).

| 가드 | 실패 시 | 탈출구 |
|---|---|---|
| `/tmp/krill-publish.lock` `lockf` — 이 맥에서 동시에 한 번만(dry-run·preflight 포함) | 즉시 종료(75) | 없음 |
| `git fetch origin` 후 HEAD == `origin/main` | 중단 | `KRILL_PUBLISH_FORCE` |
| HEAD의 GitHub check-runs가 모두 `success`(또는 `skipped`), 0개면 실패 | 중단 | `KRILL_PUBLISH_FORCE` |
| `THIRD_PARTY_NOTICES.md`가 최신(`generate_notices.py --check`) | 중단 | `KRILL_PUBLISH_FORCE` |
| 원격 태그(`git ls-remote --tags`)·GitHub 릴리스가 이미 있음 — 빌드 뒤 태그 직전에 한 번 더 확인 | 중단 | 없음(버전을 올린다) |
| 로컬 태그가 있으면 HEAD를 가리켜야 함 | 중단 | 없음 |
| 소스 tarball과 같은 `HEAD` 트리에 `docs/boost-*`·`docs/reports/`·`.night/` 없음, `scripts/public-surface.sh`가 있으면 `git archive HEAD`를 푼 경로로 실행 | 중단 | 없음 |

- 기존 릴리스에 자산을 올리는 경로(`gh release upload --clobber`)는 없다. 태그만 푸시하고 `gh release create --verify-tag`로 만든다.
- 게시 빌드는 전용 `CARGO_TARGET_DIR`(설정된 절대 경로의 형제 `krill-publish`, 없으면 `target/krill-publish`)을 써서
  다른 워크트리 빌드가 게시 바이너리를 덮어쓰지 못하게 한다. 락이 단독 사용을 보장한다.
- `KRILL_PUBLISH_FORCE=<사유>`는 사람 전용이다. 대화형 터미널에서 태그를 직접 입력해 확인해야 하고 사유를 출력한다.
- 회귀 테스트: `python3 packaging/test_publish.py`(임시 git 저장소·로컬 bare origin·가짜 `gh`, 네트워크·빌드 없음).

# 로컬 릴리스와 개인 Homebrew tap (MK-072)

Apple Developer 미가입 상태에서의 **ad-hoc 서명 DMG와 소스 빌드 formula·개인 cask 준비** 안내다.
공증된 배포나 Gatekeeper 무경고를 보장하지 않는다. 스크립트는 업로드·원격 생성·push·tap 등록을 하지 않는다.
공식 homebrew-cask에 제출하지 않는다.

## 조립

macOS Command Line Tools, Rust(MSRV는 루트 Cargo.toml), Python 3.9 이상이 필요하다.
`packaging/bundle.sh`(MK-061)가 다음 번들을 먼저 만들어야 한다.

```text
packaging/dist/Krill.app/Contents/
  Info.plist                 # dev.krill.app, CFBundleShortVersionString, CFBundleIconFile
  MacOS/krill-app
  MacOS/krill
  Helpers/krill-holder
  Helpers/krilld
  Helpers/krill-link
  Resources/AppIcon.icns
```

번들러는 각 실행 파일과 앱을 ad-hoc 서명한다. MK-072는 번들러를 중복 구현하지 않는다.
헬퍼 identifier는 각각 고유해야 하며 `codesign --deep`을 쓰지 않는다(CONVENTIONS A3).

```sh
perl -e 'alarm shift; exec @ARGV' 300 cargo build --release --locked -p krill-app -p krill -p krill-holder -p krilld -p krill-link
bash packaging/bundle.sh
# 소스·번들러가 커밋된 깨끗한 워크트리에서 실행. 버전은 Cargo.toml·Info.plist와 같아야 한다.
bash packaging/release.sh 0.1.0 https://YOUR-HOMEPAGE
```

`packaging/dist/`(gitignore)에 DMG, 소스 tar.gz, `source-commit.txt`,
`homebrew-krill/Formula/krill.rb`, `Casks/krill.rb`, `SHA256SUMS`를 만든다.
DMG는 앱과 `/Applications` 링크를 담으며 자체 ad-hoc 서명·`codesign --verify --strict`·
`hdiutil verify`를 거친다. 소스 아카이브는 `git archive HEAD`이며 vendor·Cargo.lock을 포함한다.
기존 산출물을 덮어쓰지 않는다. 소스 formula가 빠진 번들러를 참조하지 않도록
`packaging/bundle.sh`가 현재 커밋에 없으면 조립을 거부한다.

기본 다운로드 URL은 이 맥의 `file://` 경로다. 비공개 GitHub의 브라우저 다운로드 URL을
Homebrew가 자동 인증한다고 가정하지 않는다. 다른 맥에서는 소스·DMG를 인증된 경로로 직접 내려받고
아래 명령으로 그 맥의 로컬 URL을 넣어 tap을 다시 만든다. 인증 정보는 파일·URL에 넣지 않는다.

```sh
python3 packaging/homebrew/generate.py --version 0.1.0 \
  --homepage https://YOUR-HOMEPAGE \
  --source /absolute/path/Krill-0.1.0-source.tar.gz \
  --source-url file:///absolute/path/Krill-0.1.0-source.tar.gz \
  --dmg /absolute/path/Krill-0.1.0.dmg \
  --dmg-url file:///absolute/path/Krill-0.1.0.dmg \
  --output /absolute/path/homebrew-krill
```

공개 접근 가능한 HTTPS URL을 준비한 뒤에는 조립 명령의 세 번째·네 번째 인자로
소스 URL·DMG URL을 넣는다. URL 자격 증명·query·fragment는 거부한다.
다섯 번째·여섯 번째 인자로 입력 앱과 새 출력 디렉터리를 바꿀 수 있다.

## 설치와 제거

생성된 formula는 `--locked`로 로컬 소스 빌드하고 MK-061 번들러를 실행한다.
번들은 Homebrew `libexec/Krill.app`에, `krill`과 UI 실행 명령은 `bin`에 연결한다.
formula와 cask를 함께 설치하지 않는다. 기존 동명 CLI는
덮어쓰지 않는다. cask는 정상 파일과 끊어진 심링크 모두 설치 전에 검사한다.

개인 tap 저장소를 직접 만들고 파일을 검토·push·등록한 뒤 사용하는 명령:

```sh
brew install --formula OWNER/krill/krill
# 개인 cask 대안 (ad-hoc·공증 없음)
brew install --cask OWNER/krill/krill
command -v krill
```

기본 Gatekeeper/quarantine 동작은 유지한다. 내려받은 ad-hoc 앱 실행은 막힐 수 있다.
서명·공증 및 격리된 실제 설치 실행 검증은 MK-061·MK-104 이후 별도 출시 게이트다.
로컬 소스 빌드를 우선 사용한다.

앱 제거·업데이트가 세션 프로세스를 종료하지 않도록 cask에는 `pkill`·`killall`·
`uninstall launchctl`을 넣지 않는다. 현재 개발 모드의 detached holder는 launchd 잡이 아니다.
완전 삭제 전에는 사용자가 세션을 종료하고 해당 인스턴스의 holder·krilld를 멈춰야 한다.
실행 중 상태 디렉터리를 zap하면 다시 생성될 수 있다.

동의해 설치한 hook은 **상태 기록을 지우기 전에** `krill hook uninstall <agent>`로 되돌린다.
사용자 에이전트 설정·백업, 사용자 워크트리, 사용자 지정 `KRILL_STATE_DIR`·`KRILL_LOG_DIR`,
수동으로 만든 다른 CLI 링크는 zap 대상이 아니다. 이를 통째로 지우는 glob은 사용하지 않는다.

```sh
brew uninstall --cask --zap OWNER/krill/krill
```

cask의 앱·CLI 연결은 Homebrew가 제거하고, zap은 다음 기본 데이터 경로를 휴지통으로 옮긴다.
휴지통을 비우는 것은 사용자 작업이다. formula 제거는 사용자 데이터를 보존한다.

- `~/Library/Application Support/dev.krill.app` (세션 기록·설정·온보딩·안정 헬퍼 복사본)
- `~/Library/Caches/dev.krill.app`
- `~/Library/Logs/Krill`
- `~/Library/Preferences/dev.krill.app.plist`
- `~/Library/Saved Application State/dev.krill.app.savedState`
- 개명 전 같은 자리(`dev.minikrill.app`·`Logs/Minikrill`). MK-170 이관 뒤에는 새 경로를 가리키는 호환 링크다.

## 회귀 검증

```sh
python3 packaging/homebrew/test_release.py
python3 packaging/test_publish.py   # 게시 가드
```

표준 라이브러리 테스트가 URL·Ruby 보간 주입·체크섬·덮어쓰기 거부를 확인한다.
macOS 테스트는 임시 git 저장소·작은 C 실행 파일을 MK-061 번들 출력 스텁으로 써
실제 서명·DMG 조립·Homebrew formula 파싱·cask 설치·`command -v krill`·충돌 보존·제거를 확인한다.
실제 사용자 prefix·앱·PATH 설정·에이전트 CLI는 변경하지 않는다.
비어 있지 않은 zap 데이터에는 실제 Homebrew 경로 해석·artifact dispatcher를 쓰되,
Foundation의 실제 사용자 휴지통 이동만 임시 디렉터리 이동으로 대체한다.
완제품 앱 실행·Gatekeeper 승인·실제 Rust formula 빌드를 대체하는 테스트는 아니다.

---

# macOS 번들·서명 (MK-061)

2026-10-15 지휘자 결정: Apple Developer 미가입 상태에서는 **ad-hoc 서명만** 한다.
이 산출물은 공증된 배포판이 아니며 `spctl`의 `Notarized Developer ID`·`stapler validate`·DMG 공증을 충족하지 않는다.
그 세 항목은 MK-104 인증서와 MK-007 검증 후의 출시 게이트로 남는다.

## 실행

macOS, Command Line Tools(`clang`, `codesign`, `ditto`, `lipo`, `plutil`), Python 3, Perl,
저장소의 Rust 툴체인이 필요하다. 저장소 루트에서:

```sh
# 기본: --locked release 빌드 → 복사 → 안쪽부터 ad-hoc 서명 → 검증 → ZIP.
packaging/bundle.sh

# SemVer 버전 범프(patch/minor/major: Cargo.toml + Cargo.lock 갱신) 후 릴리스 번들 생성.
packaging/bundle.sh --bump patch

# 기존 target/release 바이너리로 빌드만 생략한다.
packaging/bundle.sh --bin-dir target/release

# 개발 빌드 재현. 기본 출력과 충돌하지 않도록 별도 디렉터리를 쓸 수도 있다.
packaging/bundle.sh --profile dev --output target/package-dev

# 기존 번들의 개별 서명과 identifier 재검증.
packaging/macos/verify.sh packaging/dist/Krill.app

# 실제 Mach-O로 ZIP 왕복·변조·identifier·입력 누락·기존 출력 보존 검사.
python3 packaging/macos/test_pipeline.py
```

기본 결과는 `packaging/dist/Krill.app`, `Krill-<version>-<arch>-adhoc.zip`,
그 ZIP의 `.sha256`이다. DMG·Homebrew(MK-072)는 `packaging/dist/Krill.app`을 입력으로 쓴다.
버전은 루트 `Cargo.toml`의 `[workspace.package].version`에서 읽고 현재는 `숫자.숫자.숫자` 형식만 허용한다.
아키텍처는 입력 Mach-O에서 읽으며 네 바이너리 모두 같은 `arm64` 또는 `x86_64`여야 한다.
소스 바이너리는 변경하지 않는다. 기존 앱·동명 ZIP·체크섬을 덮어쓰지 않으므로, 재실행 시 새 `--output` 경로를 지정한다.
Cargo 출력에서 실제 실행 파일 경로를 읽어 `CARGO_TARGET_DIR` 및 Cargo target 설정도 반영한다.
Cargo 호출은 `perl -e 'alarm shift; exec @ARGV' 300` 제한을 적용하고, 실패 시 패키징도 실패한다.

| 번들 경로 | 서명 identifier |
|---|---|
| `Contents/MacOS/krill-app` 및 앱 | `dev.krill.app` |
| `Contents/MacOS/krill` | `dev.krill.app.cli` |
| `Contents/Helpers/krill-holder` | `dev.krill.app.holder` |
| `Contents/Helpers/krilld` | `dev.krill.app.krilld` |

헬퍼·CLI를 먼저 개별 서명하고 앱을 마지막으로 서명한다. `--deep`은 사용하지 않는다.
각 실행 파일과 번들에 `codesign --verify --strict`를 실행하고, ZIP을 별도 임시 경로에 풀어 같은 검증을 반복한다.
임시 경로는 스크립트가 생성한 것만 종료 시 제거한다. 앱·데몬을 실행하거나 사용자 설정·launchd를 변경하지 않는다.
ad-hoc 서명에는 TeamIdentifier가 없다. 같은 팀의 Developer ID 서명·TCC 귀속을 검증했다는 의미가 아니다.

## CI

`.github/workflows/package-macos.yml`은 `workflow_dispatch`로만 실행한다.
Rust 1.98.1을 선택하고 회귀 검사(`test_pipeline.py`·`homebrew/test_release.py`·`test_publish.py`) 후 같은 로컬 진입점으로 빌드한다.
프로필은 `release`(기본) 또는 `dev`를 선택한다. ZIP과 체크섬만 Actions artifact로 보관하며 릴리스를 발행하지 않는다.
이 작업에서는 워크플로를 원격 실행하지 않았다.
액션 설정 근거: [checkout 공식 문서](https://github.com/actions/checkout),
[upload-artifact 공식 문서](https://github.com/actions/upload-artifact).

## Developer ID·공증 전환 지점 (미구현·미실행)

MK-104 가입·인증서 설치와 MK-007의 헬퍼·TCC·엔타이틀먼트 검증 후 별도 변경으로 활성화한다.
현재 스크립트는 환경 변수로 인증서를 선택하거나 공증 서버로 전송하지 않는다.

1. `bundle.sh`의 개별 `codesign` 호출 모두를 동일한 `Developer ID Application` identity로 바꾼다.
   기존 고유 identifier·안쪽부터 서명하는 순서는 유지하고 `--options runtime --timestamp`를 추가한다.
   엔타이틀먼트는 기본 비움이며 필요하다고 MK-007에서 확인한 권한만 추가한다.
2. 생성 ZIP을 `xcrun notarytool submit <ZIP> --keychain-profile <사전설정 프로필> --wait`로 제출한다.
   `Accepted` 상태를 확인한 뒤 `xcrun stapler staple <앱>` 및 `xcrun stapler validate <앱>`을 실행한다.
   `spctl -a -vv --type execute <앱>`가 `Notarized Developer ID`인지 확인한다.
3. staple 후 ZIP과 SHA-256을 **다시 생성**한다. MK-072의 DMG는 이 앱으로 만든 뒤 Developer ID로 서명하고,
   DMG도 `notarytool submit ... --wait` → `stapler staple` → `stapler validate` 순서로 검증한다.
   DMG 평가에는 `spctl -a -vv --type open --context context:primary-signature <DMG>`를 사용한다.
4. CI에는 인증서·키체인 임시 생성과 정리, 인증 정보 주입을 별도 구현한다. 인증 정보는 저장소에 넣지 않는다.
   공증 성공 후에만 `adhoc` 산출물 이름과 배포 게이트를 바꾼다.

CLI 옵션은 이 머신의 `codesign --help`, `xcrun notarytool submit --help`, `xcrun stapler --help`로 확인했다.
인증서 서명·서버 제출·staple·Gatekeeper 통과 및 DMG 공증은 미검증이다.

## 로컬 재현 기록 (2026-10-15)

macOS 27.0.1 arm64, Homebrew rustc 1.98.1, Command Line Tools 환경에서:

- `python3 packaging/macos/test_pipeline.py`: 6개 통과. ZIP 왕복·입력 무변경·헬퍼 변조·잘못된
  서명 identifier·재서명된 잘못된 plist·입력 누락·기존 출력 보존을 확인했다.
- `packaging/bundle.sh --profile dev`: 실제 네 바이너리로 기본 경로의 앱·ZIP·SHA-256 생성 성공.
  ZIP을 풀어 개별 실행 파일과 앱의 `codesign --verify --strict` 및 identifier 검증 성공.
- `packaging/macos/verify.sh packaging/dist/Krill.app`: 성공.
  `codesign -d --verbose=2` 결과는 `Signature=adhoc`, `TeamIdentifier=not set`이다.
- Bash 구문 검사와 workflow YAML 파싱, 수동 트리거만 존재하는지 확인했다.

`release` 프로필의 실제 전체 빌드와 GitHub Actions 원격 실행은 이 기록에 포함하지 않는다.

## Linux 원격 호스트 바이너리

Linux 머신은 GUI 없이 Mac 앱이 SSH로 붙는 원격 호스트다. `krill`·`krill-holder`·`krilld`만 묶는다.

```sh
# Linux 머신에서 직접: dist/krill-VERSION-linux-ARCH.tar.gz(+ .sha256)
bash packaging/linux/build.sh VERSION
bash packaging/linux/smoke.sh packaging/dist/krill-VERSION-linux-ARCH.tar.gz   # 임시 HOME 설치·세션·kill -9 생존
# 맥에서: GCP 임시 VM(Ubuntu 22.04, x86_64·aarch64)으로 빌드·스모크 후 VM 삭제(유료)
KRILL_GCP_PROJECT=<project> bash packaging/linux/build-gcp.sh VERSION
```

`publish-github.sh`는 `KRILL_GCP_PROJECT`가 있으면 위 GCP 빌드를 돌리고, Linux tar.gz와
`install-linux.sh`를 릴리스 자산에 넣는다. 원격 머신 설치는 `install-linux.sh`(→ `~/.local/bin`).

원격 Linux 요구사항(2026-10-18 실측):
- glibc 2.34 이상(Ubuntu 22.04+, Debian 12+, RHEL/Rocky 9+), 커널 5.9 이상. Rocky 8(glibc 2.28)에서는
  `install-linux.sh`가 설치 전에 멈추고 원인을 알린다.
- systemd-logind `KillUserProcesses=yes`이면 데몬을 깨운 ssh 로그인이 끝나는 순간 데몬이 죽어 연결할 수 없다
  (Ubuntu 22.04 GCP VM에서 재현). `KillExcludeUsers=<사용자>`를 넣으면 해결된다(같은 VM에서 확인).
  Ubuntu·Debian·RHEL 기본값은 `no`.
- 이미 설치된 머신에 다시 설치하면 `krill update`가 실행 중인 데몬을 세션을 유지한 채 교체한다.
