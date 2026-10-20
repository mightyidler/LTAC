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

버전 태그의 릴리스 워크플로는 서명 검증 후 릴리스를 공개하고 `update.json`을
갱신합니다. 로컬 검증 중에는 태그 푸시나 manifest 게시 도구를 실행하지 않습니다.

## 저장소 위생

`npm run setup:hooks`로 커밋 훅을 활성화합니다. 훅은 Git 인덱스의 실제 staged
내용을 검사하며, CI는 일반 push와 pull request에서도 파일명·비밀정보 패턴을
검사합니다. `node tools/check-repository.mjs --history`는 로컬 Git 이력을 검사합니다.
패턴 검사만으로 모든 정보 유출을 탐지할 수 없으므로 공개 자료도 직접 검토합니다.

실기 기록, 설정 백업, 캡처와 임시 산출물은 Git에서 제외되는 `local/`에 둡니다.
앱 소스는 `src/`, 데스크톱·설치 설정은 `src-tauri/`, UI는 `ui/src/`,
아이콘·폰트는 `resources/`, 회귀 테스트는 `tests/`와 `tools/check-*`에 둡니다.
폰트의 OFL 라이선스는 리소스와 함께 유지합니다.
