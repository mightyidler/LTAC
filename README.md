# LTAC

**LG TV Auto Control · 0.1.0**

Windows PC의 시작·화면 상태·절전·종료에 맞춰 LG webOS TV 전원을 제어합니다. Rust 전원 코어와 Windows 서비스, Tauri 데스크톱 UI로 구성됩니다.

## 설치

[최신 릴리스](https://github.com/mightyidler/LTAC/releases/latest)에서 Windows x64 `LTAC_버전_x64-setup.exe`를 실행합니다. 설치 시 관리자 권한이 필요합니다. 설치 파일은 앱·CLI·`LTACPower` 서비스를 함께 설치하며 WebView2가 없으면 설치를 안내합니다.

TV를 켜고 같은 네트워크에서 LTAC을 실행한 뒤 검색된 TV를 선택하고 TV의 연결 요청을 승인합니다. 직접 IP/MAC 입력도 지원합니다. TV의 네트워크 전원 켜기 기능을 활성화해야 하며, 자동 확인된 MAC이 없으면 기기 정보에서 추가합니다. 전원 동기화는 기본 ON입니다. UI의 기기 추가·수정·삭제, 동기화 설정, 절전 단축키와 업데이트가 실제 기능에 연결됩니다.

앱 창을 닫으면 트레이에 남습니다. 앱과 단축키는 로그인 시 시작하며, TV의 부팅·종료 제어는 로그인 전부터 독립 서비스가 담당합니다. 트레이에서 앱을 종료해도 서비스의 자동 제어는 계속됩니다. 동기화를 끄면 부팅 ON과 종료 OFF도 중지됩니다. 다른 TV 자동 제어 프로그램과 동시에 사용하지 마세요.

## 전원 제어

- PC 부팅 시 제한된 WoL 재시도와 인증 재연결을 병행합니다. 인증된 TV의 Active 응답이 있어야 ON으로 확인합니다.
- 화면 OFF는 1.2초 동안 짧은 변화를 합칩니다. 반대 이벤트는 이전 작업을 취소하고 최신 상태를 적용합니다. 이미 활성 상태인 인증 연결이 있으면 불필요한 WoL을 생략합니다.
- 유지보수용 자동 복귀만으로 TV를 켜지 않습니다. 절전·종료는 ON보다 우선하며, 기기별 OFF 전송 기록으로 중복을 억제합니다.
- 절전 OFF는 최대 1.4초, 서비스 콜백은 최대 1.7초, 종료 OFF는 전체 최대 3.5초입니다. 일반 서비스 중지는 TV 전원을 변경하지 않습니다.
- 최대 16대의 TV를 병렬 제어합니다. 평상시 상태 폴링 없이 Windows 이벤트와 인증 WebSocket 수신만 기다립니다.
- 네트워크 불통이나 WoL 전송을 전원 성공으로 기록하지 않습니다. OFF 요청·TV 응답과 실물 화면 확인은 구별합니다.

TV·NIC·공유기 상태와 강제 전원 차단까지 소프트웨어가 보장할 수는 없습니다. **0.1.0의 실제 TV 전원, 절전, 부팅·종료 시험은 사용자 요청으로 수행하지 않았습니다.** 자동 검증의 범위는 [검증 기록](docs/VALIDATION.md)에 있습니다.

## 업데이트

홈의 업데이트 확인 버튼으로 새 버전을 확인·설치합니다. Bald와 동일하게 태그 푸시 → GitHub Actions Windows 빌드·서명 → GitHub Release 게시 → `main/update.json` 갱신으로 배포합니다. 앱 내 업데이트는 공개 키로 서명을 검증하며, 설치 프로그램이 앱과 서비스를 함께 교체합니다. 새 앱이 실제 대상 버전으로 실행된 뒤에만 완료로 표시합니다. [배포 설명](docs/UPDATES.md)

## 데이터와 보안

사용자 설정·페어링 정보: `%LOCALAPPDATA%\LTAC`. 서비스 설정·로그: `%ProgramData%\LTAC\Service`. 사용자 키는 계정 DPAPI, 서비스 사본은 machine DPAPI로 보호합니다. 서비스 데이터는 관리자·LocalService·설치 사용자로 접근을 제한하며 로컬 pipe의 클라이언트 권한과 서비스 PID를 검증합니다. 인증 키를 로그나 GitHub에 올리지 않습니다. TLS TV 인증서는 최초 승인 후 지문을 고정합니다.

앱 제거 시 서비스도 제거합니다. 재설치를 위해 보호된 설정은 보존합니다. 관리자 권한 설치는 사용할 Windows 계정으로 진행합니다. 다른 관리자 계정으로 설치한 경우 해당 관리자 계정이 서비스 제어 소유자가 됩니다.

## 개발

Windows, Rust 1.89 이상, Node.js 24 이상을 사용합니다.

```powershell
npm ci
npm test
cargo test --locked --all-targets
npm run core:release
cargo test --locked --manifest-path src-tauri/Cargo.toml --all-targets
npm run desktop
```

서명 릴리스는 저장소 밖의 키를 `TAURI_SIGNING_PRIVATE_KEY`로 지정하고 `npm run desktop:release`로 만듭니다. 결과는 `src-tauri/target/release/bundle/nsis`에 생성합니다. `npm run dev`의 브라우저는 테스트 데이터를 사용하고 실제 TV를 제어하지 않습니다. 기존 UI·모션은 유지합니다.

CLI는 `target/release/ltac.exe --help`를 참조합니다. 서비스가 설치된 환경에서는 중복 제어를 시작하지 않고 서비스에 위임합니다. 수동 `verify`, `off`, `on`, `watch`는 실제 전원을 제어하므로 자동 검증 명령에 포함하지 않습니다.
