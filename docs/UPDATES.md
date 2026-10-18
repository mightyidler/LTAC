# GitHub 업데이트와 릴리스

배포 저장소: https://github.com/mightyidler/LTAC

Bald와 동일하게 `v버전` 태그 푸시가 `.github/workflows/release.yml`을 실행합니다. 버전 일치·포맷·Clippy·Rust/UI 테스트를 검사하고 Windows x64 서비스·CLI·데스크톱을 빌드합니다. Tauri로 NSIS 설치 파일과 updater 서명을 생성한 뒤 공개 키 검증과 변조 거부 검사를 수행합니다. 초안 릴리스를 공개하고 `main/update.json`을 갱신하는 커밋을 자동 푸시합니다. 실패하면 새 manifest를 공개하지 않습니다.

서명 비밀 키는 로컬 `%USERPROFILE%\.tauri\ltac.key`와 GitHub Actions의 `TAURI_SIGNING_PRIVATE_KEY` secret에만 보관합니다. 공개 키는 `src-tauri/tauri.conf.json`에 있습니다. 키가 암호화되어 있으면 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` secret도 설정합니다. 이후 릴리스는 같은 키를 사용합니다.

앱은 `https://raw.githubusercontent.com/mightyidler/LTAC/main/update.json`을 확인합니다. 업데이트 버튼은 확인·설치 중 중복 요청을 막습니다. Tauri가 서명을 검증한 설치 파일만 실행하고 기존 앱을 종료합니다. NSIS는 일반 STOP으로 서비스를 중지하여 TV 상태를 유지하고 새 서비스와 앱을 함께 설치합니다. 설치 직후 재시작은 부팅 ON을 생략하며, 다음 실제 PC 부팅에는 부팅 제어가 활성화됩니다.

새 앱이 로컬에 기록된 대상 버전과 일치할 때만 업데이트 완료로 표시합니다. 다운로드·서명·설치 실패는 기존 토스트와 다시 확인 버튼으로 처리합니다. 첫 0.1.0은 최초 릴리스이므로 이전 공개 버전에서의 실제 업그레이드 시험은 없습니다.

## 다음 버전 배포

1. `package.json`, `package-lock.json`, 루트·`src-tauri`의 Cargo.toml/Cargo.lock, tauri.conf.json 버전을 맞춥니다.
2. 자동 검증 후 변경을 커밋하고 `main`과 `v버전` 태그를 푸시합니다.
3. GitHub Actions 성공, 공개 Release 설치 파일·sig, main의 update.json URL·서명을 확인합니다.
4. 이전 설치본의 실제 업데이트·재실행 검증은 별도로 기록합니다.

## 로컬 패키지 검증

```powershell
$env:TAURI_SIGNING_PRIVATE_KEY = Join-Path $env:USERPROFILE '.tauri/ltac.key'
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ''
npm run desktop:release
node tools/check-release.mjs --package
node tools/verify-update.mjs src-tauri/target/release/bundle/nsis/LTAC_0.1.0_x64-setup.exe
```

서명 검증 코드는 Tauri 런타임 검증에 더해 릴리스 산출물을 독립 검사합니다. 실제 설치나 서비스 시작·TV 전원 동작은 실행하지 않습니다.
