# MK-074 고지 생성과 배포 전 확인

`LICENSE`는 MK-077(OWE-591, 2026-10-15) 결정에 따라 `Apache-2.0`으로 확정됐다. 이 문서는 서드파티 고지(`THIRD_PARTY_NOTICES.md`) 생성과 바이너리 배포 전 확인 절차를 다룬다.

```sh
python3 docs/licensing/generate_notices.py
python3 docs/licensing/generate_notices.py --check
python3 -m unittest discover -s docs/licensing -p 'test_*.py' -v
python3 -m unittest discover -s docs/benchmarks -v
```

생성기는 설치된 `cargo-deny`의 `list --format json --layout crate`와 `cargo metadata --locked`를
각각 `perl -e 'alarm shift; exec @ARGV' 300`으로 감싸 실행한다. 의존성을 추가하거나 도구를 설치하지 않는다.
`deny.toml`의 두 macOS target에 해당하는 보수적인 목록이며 build/dev 의존성도 포함한다.
워크스페이스 자체 크레이트는 제외하고, vendor된 gpui 묶음과 Lucide/Feather 고지도 포함한다.

`THIRD_PARTY_NOTICES.md`에는 Cargo.lock 해시·패키지별 upstream SPDX 표현식·저장소와
실제 배포된 crate 폴더의 LICENSE/LICENCE/COPYING/NOTICE/UNLICENSE 파일 원문을 넣는다.
줄바꿈과 줄 끝 공백만 정규화한 뒤 같은 원문은 SHA-256으로 합치며, 저작권 표시와 문구는 지우지 않는다.
SPDX의 `OR` 표현식을 임의로 선택하거나 `cargo deny`가 허용한 대안을 배포 승인으로 해석하지 않는다.
crate 배포본에 단독 라이선스 파일이 없으면 잠긴 Cargo 메타데이터의 SPDX ID에 따라
`spdx/`의 [SPDX License List v3.27.0 표준 본문](https://github.com/spdx/license-list-data/tree/v3.27.0/text)을 넣는다.
현재 폴백은 `Apache-2.0`·`MIT`·`Zlib`이며, `OR`·`AND` 표현식의 모든 ID가 지원될 때만 적용한다.
기존 파일은 우선 보존하며, 미지원 ID·예외가 포함된 표현식은 `UPSTREAM TEXT MISSING`으로 남긴다.
표준 본문의 저작권 자리표시자는 그대로 두고 Cargo authors를 저작권자로 추정하지 않는다.
이는 라이선스 조건의 보완이며 upstream 저작권 고지의 복구가 아니므로 배포 전 출처 확인은 여전히 필요하다.
이 목록의 완성 여부와 최종 배포물의 라이선스 충족 여부는 별도 검토가 필요하다.

배포 전 체크리스트:

- [x] MK-077 라이선스·가격 모델 결정 후 `LICENSE`(`Apache-2.0`) 확정.
- [ ] SPDX 폴백 패키지 및 `UPSTREAM TEXT MISSING` 패키지의 해당 버전 upstream 원문·저작권 확인 및 보완.
- [ ] deny.toml의 잠정 예외(MPL-2.0/0BSD/CC0-1.0)와 실제 배포물 범위 검토.
- [ ] 최종 고지·소스 제공 등 배포 의무 확인.
