# 개발과 검증

Windows에서 Node.js 24 이상, `rust-toolchain.toml`의 Rust 도구 모음,
Visual Studio C++ 빌드 도구와 WebView2가 필요합니다.

```powershell
npm ci
npm run dev
npm run desktop
```

브라우저 개발용 화면과 모의 응답은 `ui/dev/`에서 관리합니다. 실제 데스크톱은
Tauri IPC를 사용하며, 배포 빌드에는 개발용 모듈을 포함하지 않습니다.

```powershell
npm run check:repository
npm run lint
npm test
npm run test:browser
cargo test --locked --all-targets
cargo test --locked --manifest-path src-tauri/Cargo.toml --all-targets
npm run build
npm run desktop:build -- -- --locked
```

브라우저 검증은 설치된 Microsoft Edge를 사용하며 실제 TV를 제어하지 않습니다.
`npm run test:appearance`는 언어·테마·타이포그래피를 추가 검사합니다.
핵심 Rust 테스트는 전원 제어·입력 보호·설정·IPC를, UI 테스트는 연결 흐름,
업데이트, 입력, 모션과 표시 상태를 검증합니다. 실제 TV와 PC 전원 동작은
하드웨어 환경에서 별도로 확인해야 합니다.

## 설치와 업데이트

`npm run desktop:release`는 서비스와 CLI를 빌드한 뒤 NSIS 설치 파일을 만듭니다.
`src-tauri/windows/hooks.nsh`와 `tools/Manage-Service.ps1`이 서비스 설치·제거를
처리합니다. 사용자·서비스 설정과 인증 정보는 저장소 밖의 앱 데이터 폴더에 둡니다.

업데이트 공개 키는 `src-tauri/tauri.conf.json`에 유지합니다. 비밀 키는 저장소
밖에서 관리하고 CI에는 GitHub Actions Secrets로 전달합니다.
`tools/check-release.mjs --package`와 `tools/verify-update.mjs <설치파일>`로
패키지 리소스와 서명을 검증할 수 있습니다.

## 버전 배포

변경 사항을 먼저 커밋한 뒤 아래 명령 하나로 배포합니다.

```powershell
npm run setup:hooks
npm run release -- 0.1.7
```

버전 번호는 예시입니다. 현재보다 높은 새 번호를 지정하세요. 명령은 원격 main을
fast-forward로 동기화하고 npm·Cargo·Tauri 버전 7곳을 맞춘 다음 UI·브라우저·Rust
검증을 수행합니다. 통과하면 버전 커밋과 태그를 만들고 main과 태그를 원자적으로
함께 푸시합니다. 원격 이력이 갈라졌으면 덮어쓰지 않고 중단합니다.
검증 실패 시 버전 파일을 원래대로 복구하고 커밋·태그·푸시는 만들지 않습니다.
푸시만 실패했다면 원인을 해결한 뒤 `git push --atomic origin main v0.1.7`로 재시도합니다.
이미 공개한 버전 태그는 이동하거나 재사용하지 않습니다.

일반 main 푸시는 Actions를 실행하지 않고, 버전 태그는 `Release LTAC` 하나를
실행합니다. PR과 필요 시 수동 실행으로 별도 검사할 수 있습니다. 릴리스에서도 브라우저 검증을 포함한 전체
검사를 수행하고, 설치 파일·서명·실제 설치 검증이 끝나야 공개합니다.
로컬 pre-push 훅은 main과 버전 태그를 올리기 전에 UI 검증을 수행합니다.
새 clone에서는 `npm run setup:hooks`를 한 번 실행해야 합니다.

새 앱은 GitHub 최신 릴리스에 첨부한 `update.json`으로 업데이트를 확인합니다.
릴리스 생성 파일은 `local/release/`에만 쓰며 매번 main을 갱신하지 않습니다.
기존 앱은 raw main/update.json을 사용하므로 전환 후 첫 릴리스에서만 호환용
manifest를 main에 한 번 기록합니다. 다음 배포 명령이 이 커밋도 자동으로 동기화합니다.
이 manifest의 `release_asset_migration` 표시를 유지해야 합니다. 기존 앱은 이
전환 버전을 설치한 다음부터 최신 릴리스 첨부 파일을 사용합니다.

## 저장소 위생

`npm run setup:hooks`로 커밋 훅을 활성화합니다. 훅은 Git 인덱스의 실제 staged
내용을 검사하며, CI는 일반 push와 pull request에서도 파일명·비밀정보 패턴을
검사합니다. `node tools/check-repository.mjs --history`는 로컬 Git 이력을 검사합니다.
패턴 검사만으로 모든 정보 유출을 탐지할 수 없으므로 공개 자료도 직접 검토합니다.

실기 기록, 설정 백업, 캡처와 임시 산출물은 Git에서 제외되는 `local/`에 둡니다.
앱 소스는 `src/`, 데스크톱·설치 설정은 `src-tauri/`, UI는 `ui/src/`,
아이콘·폰트는 `resources/`, 회귀 테스트는 `tests/`와 `tools/check-*`에 둡니다.
폰트의 OFL 라이선스는 리소스와 함께 유지합니다.
