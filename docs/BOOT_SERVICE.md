# LTAC 0.1.0 Windows 전원 서비스

## 설치와 소유권

공식 NSIS 설치 파일은 Program Files에 데스크톱·CLI·서비스를 함께 설치합니다. 관리자 권한이 필요하며 `LTACPower`를 LocalService 계정의 즉시 자동 시작 서비스로 등록합니다. 초기에는 TV 설정이 없으므로 서비스가 켜져도 TV 전원 명령을 보내지 않습니다. 앱에서 페어링을 완료하면 권한이 제한된 로컬 named pipe로 machine DPAPI 사본을 전달합니다. 서비스가 직접 보호된 설정을 원자적으로 저장합니다.

서비스는 UI와 독립적입니다. 로그인 전 제어와 시스템 종료 OFF에는 WebView2나 사용자 로그인·트레이 시작이 필요하지 않습니다. 서비스가 설치되어 있으면 UI·CLI는 별도 전원 watcher를 시작하지 않습니다. 서비스 연결 실패는 오류로 표시하며 중복 제어로 전환하지 않습니다.

UI의 기기 추가·수정·삭제는 자동 작업을 먼저 취소하고 설정을 반영한 뒤 기존 자동 정책을 재개합니다. 취소된 페어링은 임시 디렉터리에서 폐기되어 기존 설정을 덮어쓰지 않습니다. 같은 TV를 직접 입력 후 검색으로 다시 추가해도 기존 행을 갱신합니다. 최대 16대이며 동기화에는 각 TV의 실제 사용 인터페이스 MAC이 필요합니다. 물리적 ON/OFF 확인 여부는 별도 기록으로 유지하고 페어링 성공을 물리적 검증으로 표시하지 않습니다.

## 이벤트와 제한시간

| 상황 | 동작 |
| --- | --- |
| 실제 PC 부팅 | UI보다 먼저 ON 목표 적용. WoL은 0·1·2·4·8·15·25·40·60초에 최대 9회 버스트. 인증 재연결과 병행하며 90초에 종료 |
| ON 확인 | 인증된 Active/Screen Saver 응답 필요. 연결이 이미 활성 상태면 WoL 생략 |
| 일반 화면 OFF | 1.2초 debounce, 전송 작업 최대 12초 |
| 절전 | 인증 연결 재사용, OFF 최대 1.4초, Win32 콜백 대기 최대 1.7초 |
| 유지보수 자동 복귀 | 단독으로 ON 예약하지 않음 |
| 종료/재시작 | preshutdown/shutdown 수신 시 ON 취소·차단. 모든 TV OFF 병렬 처리 전체 3.5초, SCM preshutdown 설정 6초 |
| 일반 STOP / 앱 업데이트 | TV 전원 변경 없이 중지. 설치 직후 서비스 재시작은 부팅 ON 생략 |
| 짧은 화면 OFF 취소 | 소켓에 접근하기 전이라면 인증 연결 보존. 다시 ON에서 실제 상태 확인 후 불필요한 WoL 생략 |
| 유휴 연결 종료 | 제한된 WoL 없는 재연결·상태 조회. 상시 폴링 없음 |
| 늦은 NIC 준비 | ON 제한시간 안에서 재시도. 그 이후 실제 NIC 변경 알림에 따라 미확인 ON 재시도 |

TV별 OFF 전송 표식을 분리합니다. 일부 기기에 이미 OFF가 전송된 경우 그 기기는 다시 보내지 않고 나머지 기기만 처리합니다. 네트워크 불통·응답 시간 초과를 실물 OFF 확인으로 표시하지 않습니다. 자동 동기화 OFF는 부팅 ON·시스템 종료 OFF에도 적용됩니다.

## 파일과 IPC

`%ProgramData%\LTAC\Service`는 관리자·SYSTEM·LocalService와 설치 사용자에게만 접근을 줍니다. 서비스 바이너리는 Program Files에 있어 LocalService가 교체할 수 없습니다. 사용자 키 원본은 `%LOCALAPPDATA%\LTAC`의 계정 DPAPI 상태를 유지합니다. pipe는 로컬 전용이며 설치 사용자 SID·관리자·LocalService를 허용합니다. 클라이언트는 SCM의 서비스 PID와 pipe 서버 PID를 비교합니다. 요청은 정해진 enum으로 제한하고 64KiB 프레임·16개 연결/대기 슬롯·95초 응답 한도를 둡니다. 키를 로그에 쓰지 않습니다.

설치/업데이트/제거는 `tools/Manage-Service.ps1`과 NSIS hooks가 수행합니다. 예전 수동 배포용 `Install-Service.ps1`/`Uninstall-Service.ps1`은 이전 패키지 호환 도구이며 공식 0.1.0 설치 경로에 사용하지 않습니다. 제거는 서비스를 중지·삭제하고 설정은 보존합니다.

## 검증 경계

이번 릴리스는 자동 검증만 진행했습니다. 서비스 설치·시작, 실제 TV OFF/ON, PC 절전·재부팅·종료는 실행하지 않았습니다. 소프트웨어 제한시간과 모의 WebSocket 테스트 성공은 실제 NIC·TV·펌웨어의 성공률 또는 로그인 전 화면 표시 속도를 입증하지 않습니다. 상세 결과는 [VALIDATION.md](VALIDATION.md)를 참조합니다.

근거: [Microsoft 서비스 제어](https://learn.microsoft.com/en-us/windows/win32/services/service-control-handler-function), [전원 이벤트 등록](https://learn.microsoft.com/en-us/windows/win32/power/registering-for-power-events), [Tauri NSIS hooks](https://v2.tauri.app/distribute/windows-installer/).
