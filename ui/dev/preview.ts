import { previewOptions, setPreview } from './bridge';
import { demoChoices, demoDevices } from './fixtures';
import { initialState, type Snapshot } from '../src/model';
import './preview.css';

// Test controls live outside #app, and are imported only by the web dev build.
export function installPreview(show: (state: Snapshot, screen: string) => Promise<void>) {
  document.body.classList.add('web-preview');
  const panel = document.createElement('aside');
  panel.className = 'preview-tools'; panel.setAttribute('aria-label', '웹 UI 테스트 도구');
  panel.innerHTML = `<p class="preview-eyebrow">LTAC · WEB DEV UI</p><h2>화면 테스트</h2><p class="preview-description">앱 화면 375 × 500px · 바깥 여백 없음<br>Windows 테두리·라운딩은 데스크톱 앱에서 표시됩니다.<br>테스트 데이터로 동작하며 실제 TV를 제어하지 않습니다.</p><label>화면<select id="preview-screen"><option value="choose">장치 리스트</option><option value="selected">장치 선택</option><option value="scroll">스크롤 예시</option><option value="empty">장치 없음</option><option value="manual">직접 입력</option><option value="manual-filled">직접 입력 완료</option><option value="pair">연결 요청</option><option value="success">연결 완료 모션</option><option value="failure-motion">연결 실패 모션</option><option value="failure">연결 실패</option><option value="home">홈 (설정)</option><option value="home-empty">홈 (기기 없음)</option><option value="error">설정 오류</option></select></label><label>연결 결과<select id="preview-outcome"><option value="success">성공</option><option value="failure">실패</option></select></label><label>응답 대기 시간<select id="preview-delay"><option value="2400">2.4초</option><option value="600">0.6초</option><option value="8000">8초</option></select></label><label>검색 결과<select id="preview-count"><option value="2">2대</option><option value="9">9대</option><option value="0">없음</option></select></label><label>바깥 배경<select id="preview-background"><option value="neutral">회색</option><option value="checker">투명 영역 확인</option><option value="light">밝은색</option><option value="dark">어두운색</option></select></label><button id="preview-reset">선택한 화면 다시 보기</button><p class="preview-description">기기 선택 → 연결 → 완료 → 홈까지 직접 진행할 수 있습니다. 홈 상단 +로 기기를 추가하고 More 메뉴에서 정보 수정·삭제를 확인하세요.</p>`;
  panel.querySelector('#preview-count')!.closest('label')!.insertAdjacentHTML('afterend', '<label>홈 기기 수<input id="preview-home-count" type="number" min="0" max="100" step="1" value="2"></label>');
  panel.querySelector('#preview-screen option[value="home"]')!.insertAdjacentHTML('afterend', '<option value="screen-protection">화면 보호 · 시간 모달</option>');
  panel.querySelector('#preview-home-count')!.closest('label')!.insertAdjacentHTML('afterend', '<label>업데이트 결과<select id="preview-update"><option value="available">업데이트 있음</option><option value="latest">최신 버전</option><option value="error">확인 실패</option></select></label>');
  document.body.append(panel);
  const get = (id: string) => document.getElementById(id) as HTMLSelectElement;
  const homeCount = document.getElementById('preview-home-count') as HTMLInputElement;
  async function select(screen: string) {
    const choices = screen === 'scroll' ? Array.from({ length: 9 }, (_, i) => ({ ...demoChoices[0], id: `tv-${i}` })) : demoChoices;
    const patch: Snapshot = { ...initialState, choices, stage: 'choose', name: 'OLED42C4ENA', shortcut_enabled: true };
    if (screen === 'empty') { patch.stage = 'not_found'; patch.choices = []; }
    if (['pair', 'success', 'failure-motion'].includes(screen)) patch.stage = 'pairing';
    if (screen === 'failure') patch.stage = 'pair_error';
    if (['home', 'home-empty', 'screen-protection', 'error'].includes(screen)) {
      if (screen === 'home-empty') homeCount.value = '0';
      const count = Math.max(0, Math.min(100, Math.trunc(homeCount.valueAsNumber || 0)));
      homeCount.value = String(count);
      patch.stage = 'ready'; patch.configured = count > 0;
      patch.devices = Array.from({ length: count }, (_, i) => ({ ...demoDevices[i % demoDevices.length], id: `tv-${i}`, ip: `192.168.1.${21 + i}` }));
    }
    if (screen === 'screen-protection') { patch.screen_enabled = true; patch.screen_timeout_seconds = 90; }
    if (screen === 'error') patch.error = '설정을 저장하지 못했어요. 다시 시도해주세요.';
    setPreview(patch);
    const url = new URL(location.href); url.searchParams.set('screen', screen); history.replaceState(null, '', url);
    await show(patch, screen);
    if (screen === 'screen-protection') {
      const section = document.querySelector<HTMLElement>('.screen-section')!;
      const scroll = section.closest<HTMLElement>('.home-scroll')!;
      scroll.scrollTop += section.getBoundingClientRect().top - scroll.getBoundingClientRect().top - 64;
      window.scrollTo(0, 0);
    }
  }
  get('preview-screen').onchange = () => void select(get('preview-screen').value);
  get('preview-outcome').onchange = () => { previewOptions.outcome = get('preview-outcome').value as 'success' | 'failure'; };
  get('preview-delay').onchange = () => { previewOptions.delay = Number(get('preview-delay').value); };
  get('preview-count').onchange = () => { previewOptions.count = Number(get('preview-count').value); };
  get('preview-update').onchange = () => { previewOptions.update = get('preview-update').value as typeof previewOptions.update; };
  homeCount.oninput = () => {
    const screen = get('preview-screen');
    if (screen.value === 'home-empty' && homeCount.valueAsNumber > 0) screen.value = 'home';
    if (['home', 'home-empty', 'screen-protection', 'error'].includes(screen.value)) void select(screen.value);
  };
  get('preview-background').onchange = () => { document.body.dataset.background = get('preview-background').value; };
  document.getElementById('preview-reset')!.onclick = () => void select(get('preview-screen').value);
  const screen = new URLSearchParams(location.search).get('screen') ?? 'choose';
  get('preview-screen').value = Array.from(get('preview-screen').options).some(o => o.value === screen) ? screen : 'choose';
  void select(get('preview-screen').value);
}
