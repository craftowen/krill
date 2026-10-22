# 공개 릴리스 (`publish-github.sh`)

지원 플랫폼은 Apple Silicon(M 시리즈) Mac과 Windows x64다. Intel Mac·Linux 빌드/설치/게시 경로는 제거했다. Unix 공통 코드의 Linux 호환 분기는 기존 원격 세션 보존을 위해 남아 있지만 지원·검증·배포 대상이 아니다.

GitHub Releases 게시의 **공식 경로**는 로컬 맥에서 메인테이너가 돌리는 `packaging/publish-github.sh`다.
`.github/workflows/release.yml`은 수동 예비·Windows 자산 추가용이며 같은 규칙(main 끝 또는 태그 커밋에서만,
기존 자산 덮어쓰기 없음)을 따른다. 사용자가 배포·릴리스·push를 명시 지시하면 에이전트도 필요한 main·태그 push와 릴리스 생성을 수행한다(AGENTS.md 규칙 9).

## 야간 자동 배포

`packaging/nightly.py`는 로그인한 macOS 사용자의 `launchd`에 매일 **한국시간 00:00 검사 시작**을 등록한다.
시스템 시간대는 Asia/Seoul이어야 한다. 예약은 `launchd`가 깨우고, 실행은 **Orca의 일반 셸 터미널**에서 한다(AI 에이전트 호출 없음). 직접 실행한 launchd 자식은 macOS TCC 때문에 Documents·외장 볼륨 접근이 막히는 것을 실제 확인해 이 경로를 사용한다. Orca가 닫혀 있으면 시작하며 기존 원본 체크아웃의 파일·브랜치는 변경하지 않는다. 설치 즉시 빌드하거나 배포하지 않는다.

```sh
python3 packaging/nightly.py install       # 이 저장소의 dev를 대상으로 등록·갱신
python3 packaging/nightly.py install --after-release 0.1.23  # 기존 버전·macOS ARM·Windows 자산 게시 완료 뒤부터 후보 생성
python3 packaging/nightly.py preflight     # 인증·툴·기존 서명키·외장 볼륨 확인, 빌드·원격 쓰기 없음
python3 packaging/nightly.py status        # launchd 등록과 최근 실행 결과
python3 packaging/nightly.py uninstall     # 예약 해제, 로그·검사 기록 보존
# 필요할 때만 즉시 같은 경로를 실행:
python3 "$HOME/Library/Application Support/dev.krill.nightly/nightly.py" run
```

1. 실행 시작에 **원격 `dev`·`main`**을 fetch해 고정한다. 두 커밋이 모두 최신 게시 태그에 포함돼 있으면 생략한다. 원본 체크아웃은 건드리지 않는다. 작업 완료 후 `dev`에 커밋·push해 둔다. 미커밋 파일·push하지 않은 로컬 커밋·미통합 기능 브랜치는 포함하지 않으며 고정 이후 push는 다음 날 대상이다. `--after-release`로 지정한 기존 릴리스가 미게시 상태면 후보 생성도 생략한다.
2. `/Volumes/krill-build/nightly/repository` 전용 clone에서 통합한다. 병합 충돌은 중단한다. workspace와 최신 게시 버전 중 큰 버전의 patch를 올리고 Cargo.lock·고지문과 함께 **최종 후보 커밋을 먼저** 만든다.
3. 패키징 회귀 검사와 공개 표면 검사를 먼저 통과하고 고정 후보 `release/nightly`를 lease 조건으로 갱신한다. 강제 갱신 범위는 자동 후보 브랜치 하나이며 main·태그에는 사용하지 않는다. 후보 CI와 `release.yml build_only=true`(Windows/MSVC 네이티브 빌드·설치·생존 스모크)를 시작한다. 유료 VM을 호출하지 않고 서명키의 공개키만 전달한다.
4. 원격 검사가 진행되는 동안 로컬에서 `scripts/check.sh`·Windows 전체 clippy·release 빌드·10개 터미널 메모리 검사·실제 GUI kill -9/출력 재생·holder 생존/업그레이드 검사를 실행한다. release 테스트를 먼저 컴파일하고 최종 출고 바이너리를 빌드한 다음, Cargo 재빌드 없이 검사한다. 각 게이트 뒤 바이너리 해시가 같아야 한다. 전체 검사 checkpoint와 최종 바이너리 receipt는 SHA·툴체인/컴파일 플래그·검사 범위·해시를 Ed25519로 서명하며 유효 시간은 24시간이다. 재개할 때 서명과 내용을 다시 확인한다. 23시간이 지났거나 툴체인/플래그/공개키가 바뀌면 새 SHA 후보를 만들어 검사한다. 손상된 서명은 차단한다. 같은 SHA에서 실패·취소된 CI는 다음 실행 시 전체 workflow를 한 번 재시작하며, 실행 중/성공한 native 빌드는 재사용한다.
5. 후보의 저장소 검사·macOS ARM 필수 잡과 Windows 빌드가 모두 실제로 성공해야 한다. 판정은 저장소·워크플로 경로/ID·SHA·브랜치·최신 실행/attempt·잡 결과를 확인한다. 다른 PR·누락·skipped 잡은 전체 검사 증거가 아니다. 같은 HEAD의 네이티브 자산을 내려받아 출처 커밋·체크섬·Windows 버전/공개키를 검증한다. 자산 이름에 attempt를 넣고 GitHub의 artifact ID·SHA-256으로 ZIP 자체도 검증한다.
6. `main`이 그대로이고 후보가 깨끗하면 fast-forward push한다. main CI는 **동일 SHA 후보의 전체 검사 증거**가 있을 때만 그 결과를 재사용한다. 증거가 없거나 API 판정에 실패하면 전체 CI를 실행한다. 플랫폼·툴체인·컴파일 플래그별 의존성 캐시는 후보 브랜치에서 다음 날에도 재사용한다. PR 캐시 저장은 막고 검사 자체는 생략하지 않는다. 최적화 수준·LTO·테스트 기준은 낮추지 않는다.
7. `publish-github.sh --verified-release <receipt>`는 고정된 출고 바이너리를 검증해 번들로 복사하고 서명·DMG 조립한다. 이 경로는 임의 `--bin-dir` 게시와 다르며 서명된 전체 검사 증거가 필수다. 코드 서명을 정규화한 사본의 해시로 출고 바이너리와 최종 서명 번들의 동일성을 확인하고, 서명 번들의 CLI/GUI·앱/데몬 kill -9 생존 스모크도 통과해야 한다. 전체 사이클에서 실행기가 기존 BSD 게시 락을 직접 보유하고 자신이 시작한 작업을 정리한 뒤 해제한다. 최종 태그와 11개 게시 자산을 다시 내려받아 해시·버전·업데이트 서명·소스 커밋까지 확인한 뒤에만 게시 성공으로 기록한다. 기존 태그·릴리스는 덮어쓰지 않는다.
   Windows 스모크는 `smoke-ci.ps1`이 WMI로 테스트 프로세스를 Actions 러너 Job 밖에서 실행한다. 설치·CLI 자동 기동·입출력·데몬 kill 검증은 기존 `smoke.ps1`로 수행하며 테스트 경로의 프로세스만 정리한다.

실패하면 새 릴리스를 게시하지 않고 단계·대상 HEAD·오류를 기록한다. `main` 반영 뒤 CI나 게시가 실패하면 그 검증 커밋은 `main`에 남을 수 있다. 게시 중 네트워크가 끊어지면 태그만 남는 등 일부 원격 쓰기가 끝났을 수 있으며, 다음 실행도 기존 태그·자산을 덮어쓰지 않는다. 게시 계획·readback은 clone 밖 `/Volumes/krill-build/nightly/publications/<SHA>/`에 보존한다. 원격 릴리스 생성 뒤 readback만 실패한 경우 다음 실행은 기존 자산을 다운로드해 검증하는 단계만 재개한다. 태그만 남은 경우는 중단하고 운영자가 상태를 확인한다.

- 등록 파일: `~/Library/LaunchAgents/dev.krill.nightly.plist`.
- 설치된 실행기·설정·HEAD별 통과 기록: `~/Library/Application Support/dev.krill.nightly/`. 실행기를 수정하면 `install`로 갱신한다. `dispatch.json`은 Orca 셸 시작 접수만 증명하며 배포 성공은 `status.json`으로 확인한다.
- 실행 로그: `~/Library/Logs/Krill/nightly/`. 최근 결과는 설정 폴더의 `status.json`(명령별 소요 초·검사 run/attempt·재사용 출처 포함)이며 실패 후보와 자산은 외장 볼륨에 남는다.
- 맥이 켜져 있고 사용자가 로그인돼 있어야 한다(GUI 생존·메모리 테스트). 잠자기 중 예약은 깨어날 때 실행될 수 있다. 종료·로그아웃 상태에서는 자정 실행을 보장하지 않는다.
- 처음 설치 전 원격 `dev`를 준비하고, Orca 앱·CLI와 등록된 workspace, 기존 업데이트 서명키·GitHub 인증·Windows 크로스 검사 툴체인을 유지한다. 개발 중 검사는 AGENTS.md와 CONTRIBUTING.md의 부분 검사 범위를 따른다.

회귀 검증: `python3 -m unittest discover -s packaging -p 'test_*.py'`, `actionlint`, `shellcheck packaging/publish-github.sh`.

`dev` push에서는 macOS 전체 CI를 실행하지 않는다. 개발 중에는 변경 범위 검사만 하며, 전체 원격 CI는 `release/**` 야간 후보·`main` push와 PR에서 실행한다.

## 수동 게시

```sh
bash packaging/publish-github.sh --preflight      # 가드만 확인(빌드·업로드 없음, 실패 시 1)
bash packaging/publish-github.sh --bump patch     # 가드 통과 후 버전·Cargo.lock·THIRD_PARTY_NOTICES.md를 한 커밋으로 만들고 멈춤
#   → 메인테이너 또는 사용자 지시를 받은 에이전트가 그 커밋을 main에 올리고 CI 통과를 기다린다
bash packaging/publish-github.sh --dry-run        # 조립만(가드 실패는 경고). --bin-dir은 여기서만 허용
# 최종 후보에서 같은 env/target으로 전체 게이트·release 메모리/생존 검증 receipt 생성:
CARGO_TARGET_DIR=/Volumes/krill-build/targets/krill-publish python3 packaging/release_check.py validate \
  --head "$(git rev-parse HEAD)" --directory /Volumes/krill-build/nightly/staging/manual-final \
  --key-file "$HOME/Library/Keychains/krill-codesign.keychain-db.ed25519.key"
bash packaging/publish-github.sh --verified-release /Volumes/krill-build/nightly/staging/manual-final/validation.json
```

사용자 지시에 따라 보호 ref를 push할 때는 사유를 지정한다. 게시 스크립트 내부 태그 push에도 같은 사유가 전달된다.

```sh
KRILL_ALLOW_PROTECTED_PUSH='사용자 배포 지시: 검증한 릴리스 후보 main 반영' git push origin HEAD:main
KRILL_ALLOW_PROTECTED_PUSH='사용자 배포 지시: 검증한 릴리스 태그 게시' bash packaging/publish-github.sh
```

Claude Code에서는 원격 명령의 첫 줄에 `# KRILL_ALLOW_REMOTE: <사용자 지시와 작업 사유>`를 추가한다. 같은 작업의 승인을 다시 요청하거나 세션을 재시작할 필요는 없다. 이 표시는 실제 사용자 지시가 있을 때만 사용하며, 아래 가드는 그대로 적용된다.

빌드 전에 모든 가드를 보고, 하나라도 실패하면 아무것도 빌드·업로드하지 않는다(v0.1.6 덮어쓰기 사고 대응).

| 가드 | 실패 시 | 탈출구 |
|---|---|---|
| `/tmp/krill-publish.lock` `lockf` — 이 맥에서 동시에 한 번만(dry-run·preflight 포함) | 즉시 종료(75) | 없음 |
| `git fetch origin` 후 HEAD == `origin/main` | 중단 | `KRILL_PUBLISH_FORCE` |
| 동일 SHA main의 필수 저장소 검사·macOS 잡 또는 검증된 후보 재사용 증거 | 중단 | 없음 |
| HEAD의 GitHub check-runs가 모두 `success`(또는 `skipped`), 0개면 실패 | 중단 | `KRILL_PUBLISH_FORCE` |
| `THIRD_PARTY_NOTICES.md`가 최신(`generate_notices.py --check`) | 중단 | `KRILL_PUBLISH_FORCE` |
| 원격 태그(`git ls-remote --tags`)·GitHub 릴리스가 이미 있음 — 빌드 뒤 태그 직전에 한 번 더 확인 | 중단 | 없음(버전을 올린다) |
| 로컬 태그가 있으면 HEAD를 가리켜야 함 | 중단 | 없음 |
| 모든 실제 게시의 전체 게이트 서명 receipt·동결 바이너리·최종 서명 번들 lineage/실행 smoke | 중단 | 없음 |
| 소스 tarball과 같은 `HEAD` 트리에 `docs/boost-*`·`docs/reports/`·`.night/` 없음, `scripts/public-surface.sh`가 있으면 `git archive HEAD`를 푼 경로로 실행 | 중단 | 없음 |

- 기존 릴리스에 자산을 올리는 경로(`gh release upload --clobber`)는 없다. 태그만 푸시하고 `gh release create --verify-tag`로 만든다.
- 게시 빌드는 전용 `CARGO_TARGET_DIR`(외장 볼륨 `/Volumes/krill-build/targets/krill-publish`; 외장 볼륨이 없으면 중단)을 써서
  다른 워크트리 빌드가 게시 바이너리를 덮어쓰지 못하게 한다. 수동 전체 사이클도 `/usr/bin/lockf -k -t 0 /tmp/krill-publish.lock <실행 스크립트>`로 감싸고, 안쪽 publisher에는 `KRILL_PUBLISH_LOCKED=1`을 전달한다. 같은 env/target을 유지한다.
- `KRILL_PUBLISH_FORCE=<사유>`는 사람 전용이다. 대화형 터미널에서 태그를 직접 입력해 확인해야 하고 사유를 출력한다.
- 회귀 테스트: `python3 packaging/test_publish.py`(임시 git 저장소·로컬 bare origin·가짜 `gh`, 네트워크·빌드 없음).
- 승인·push 훅 회귀 테스트: `python3 scripts/test_hooks.py`(로컬 bare origin에 실제 push, Claude 훅 호출, 네트워크·빌드 없음).

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
아키텍처는 입력 Mach-O에서 읽으며 네 바이너리 모두 `arm64`여야 한다.
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
