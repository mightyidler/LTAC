# Resources

앱과 문서의 리소스는 이 폴더에서 관리합니다.

| 경로 | 용도 |
| --- | --- |
| `icons/ui/` | UI용 단색 SVG. 파일명은 소문자 kebab-case와 기준 크기(px)를 사용합니다. |
| `icons/app/icon.png` | 제공된 앱 아이콘 원본. 이 파일을 축소본으로 덮어쓰지 않습니다. |
| `icons/app/icon.ico` | Windows 앱·트레이·설치 프로그램용 아이콘. 원본 PNG에서 생성합니다. |
| `public/icons/app/` | 웹 favicon과 512px 앱 아이콘. 원본 PNG에서 생성합니다. |
| `public/fonts/pretendard/` | 웹에서 사용하는 Pretendard 가변 폰트와 OFL 라이선스. |
| `images/` | README 등 문서용 이미지. 앱 번들에는 포함하지 않습니다. |

## UI 아이콘

- `ui/src/icons.ts`를 통해 사용합니다. SVG import와 장식용 접근성 속성은 이 모듈에서 한 번 처리합니다.
- 단색 `fill`/`stroke`는 SVG 자체에서 `currentColor`를 사용합니다. 색상은 화면의 CSS가 결정합니다. `fill="none"`과 도형·선 두께·viewBox는 유지합니다.
- `check-16.svg`와 `check-24.svg`는 크기와 선 두께가 다른 디자인입니다. 단순 확대·축소로 통합하지 않습니다.
- 현재 내비게이션은 더하기↔뒤로가기 경로를 보간하므로 `navigation.ts`에서 SVG를 생성합니다. 연결 표시기의 원과 실패 경로도 애니메이션 치수에 맞춰 생성합니다.

## 정적 리소스와 생성

Vite의 `publicDir`은 `resources/public`입니다. 이 폴더만 웹 출력에 그대로 복사합니다. 예를 들어 폰트 URL은 `/fonts/pretendard/PretendardVariable.woff2`입니다. UI SVG는 `?raw` import로 번들에 포함하며 원본 폴더 전체를 복사하지 않습니다.

앱 아이콘 변경 시 `node tools/make-icon.mjs <새 원본 PNG 경로>`를 실행합니다. 기존 원본으로 다시 생성할 때는 인자를 생략합니다. Windows ICO, 웹 favicon, 웹 PNG를 함께 갱신합니다. Tauri 설정도 이 폴더를 직접 참조합니다.
