import { t, localizeError } from './i18n';
import { appUpdateState, checkAppUpdate, connect, command, desktop, windowAction } from './bridge';
import { initialState, type Device, type Snapshot } from './model';
import { ConnectionIndicator } from './connection-indicator';
import { cleanSegment, validMacSegments, shortcutFromEvent, shortcutLabel } from './input';
import { cssTimeMs, pause, swapText, swapButtonLabel, swapShortcutLabel, syncRows, transitionPages, pageTransitionMode, revealChoices } from './motion';
import { AppUpdate } from './update';
import { Toast } from './toast';
import { initializePreferences, language, theme, setLanguage, setTheme, type Language, type Theme } from './preferences';
import { Navigation } from './navigation';
import { WindowTitle } from './window-title';
import { HdmiMenu } from './hdmi-menu';
import './motion.css';
import './style.css';
import './tokens.css';
import './typography.css';
import './status-typography.css';
import './thinking-states.css';
import './status-appearance.css';
import './hdmi-menu.css';
import { icons } from './icons';
import { screenMarkup, screenTimeLabel, screenTimeOptions } from './screen-protection';
import { updateScreenTimeLabel, fitScreenTimeClose, copyScreenTimeLabel, trackScreenTimeClose, followScreenTimeClose, stopScreenTimeClose, mountScreenTimeScrollbar, unmountScreenTimeScrollbar } from './screen-time-control';
import './number-pop-in.css';
import './screen-protection.css';

// Prepare preview styles and both text weights before the first visible screen.
const previewModule = !desktop && import.meta.env.DEV ? await import('../dev/preview') : null;
const initialUpdate = await appUpdateState().catch(() => ({ version: __APP_VERSION__, completed: false }));
await Promise.all([document.fonts.load('300 15px Pretendard'), document.fonts.load('400 15px Pretendard'), document.fonts.load('500 24px Pretendard')]).catch(() => {});
await initializePreferences();
const app = document.querySelector<HTMLElement>('#app')!;
const check = icons.check;
const escape = (value: string) => value.replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
const el = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
app.innerHTML = `<div class="app-window"><div class="app-surface"><header class="window-controls"><button id="navigation" class="window-button" aria-label="${t("기기 추가")}" hidden></button><span id="compact-title">${t("연결할 장치를 선택하세요")}</span><div class="window-actions"><button id="minimize" class="window-button" aria-label="${t("최소화")}">${icons.minus}</button><button id="close" class="window-button" aria-label="${t("닫기")}">${icons.close}</button></div></header><div id="pages" class="t-page-slide" data-page="1"><section id="page-a" class="t-page" data-page-id="1"></section><section id="page-b" class="t-page" data-page-id="2" inert></section></div><div class="toast-placement"><div id="toast" class="app-toast t-toast" role="alert" aria-live="assertive" aria-atomic="true" aria-hidden="true" inert><span class="toast-message"></span></div></div></div></div>`;
const toast = new Toast(el('toast'));
const navigation = new Navigation(el<HTMLButtonElement>('navigation'));
const windowTitle = new WindowTitle(el('compact-title'));
function disableTabStops(root: HTMLElement) {
  root.querySelectorAll<HTMLElement>('button, input, select, textarea, a[href], [tabindex]').forEach(node => node.tabIndex = -1);
}
disableTabStops(app);
app.addEventListener('dragstart', event => event.preventDefault());

let state: Snapshot = { ...initialState };
let view = '';
let preferencePage: 'language' | 'theme' | null = null;
let slot = 1;
let selected: string | null = null;
let pairingLabel = '';
let manual = false;
let addingDevice = false;
let editing: Device | null = null;
let localBusy = false;
let generation = 0;
// Native ready/status events must not own navigation during the result motion.
let pairingToken: number | null = null;
let pendingPairSnapshot: Snapshot | null = null;
let indicator: ConnectionIndicator | null = null;
let capture = false;
let captureTarget: 'sleep' | 'screen' = 'sleep';
let screenDraft: number | null = null;
let screenSaveTimer: ReturnType<typeof setTimeout> | undefined;
let shortcutSaving = false;
let shortcutHint = t("키를 눌러주세요");
let activeMenu: HTMLElement | null = null;
const closingMenus = new WeakMap<HTMLElement, object>();
const hdmiMenus = new WeakMap<HTMLElement, HdmiMenu>();
let pairArgs: Record<string, unknown> = {};
let hasMounted = false;
let lastError: string | null = null;
const appUpdate = new AppUpdate(initialUpdate.version, initialUpdate.completed, checkAppUpdate, () => {
  if (view !== 'home') return;
  updateUpdateButton();
  if (appUpdate.error) toast.show(localizeError(appUpdate.error));
});
function reportError(message: string | null) {
  if (shortcutSaving) return;
  if (message && message !== lastError) toast.show(localizeError(message));
  lastError = message;
}
const current = () => el(slot === 1 ? 'page-a' : 'page-b');
const title = (text: string) => `<div class="page-heading"><p>LGTV Auto Control</p><h1>${text}</h1></div>`;
const action = (text: string, id: string, primary = false, disabled = false) => `<button id="${id}" class="action ${primary ? 'primary' : ''}" ${disabled ? 'disabled' : ''}>${text}</button>`;

function mount(key: string, html: string, backward = false) {
  if (view === key) return false;
  if (view === 'home' && screenDraft !== null) queueScreenTime(0);
  closeMenu(); capture = false; shortcutSaving = false; toast.hide(true); lastError = null; indicator?.destroy(); indicator = null;
  const initial = !hasMounted; hasMounted = true;
  const previous = current(); previous.inert = true;
  const mode = pageTransitionMode(previous.dataset.view ?? '', key, backward);
  slot = slot === 1 ? 2 : 1;
  const next = current();
  // Remove duplicate IDs while retaining the outgoing page for its transition.
  previous.querySelectorAll('[id]').forEach(node => node.removeAttribute('id'));
  next.innerHTML = html; next.inert = false;
  next.dataset.view = key;
  disableTabStops(next);
  el('pages').dataset.page = String(slot); view = key;
  navigation.setMode(key === 'home' ? 'add' : preferencePage || manual || editing || key === 'connection' || (key === 'choose' && addingDevice) ? 'back' : null);
  windowTitle.set(preferencePage ? t(preferencePage === 'language' ? '언어' : '테마') : null, !!preferencePage);
  app.classList.remove('scrolled');
  app.dataset.view = key;
  transitionPages(previous, next, mode, initial);
  return true;
}

function render(backward = false) {
  if (preferencePage) { renderPreference(backward); return; }
  if (manual || editing) { renderForm(backward); return; }
  if (state.stage === 'ready') { renderHome(backward); return; }
  if (state.stage === 'pairing') { renderPair(); return; }
  if (state.stage === 'pair_error') { renderFailure(); return; }
  const mounted = mount('choose', `<div id="list-scroll" class="page-scroll choice-scroll">${title(t("연결할 장치를 선택하세요"))}<div id="discovery-state" class="discovery-state"><div id="choices-panel" class="discovery-panel t-panel-slide" data-open="false" inert aria-hidden="true"><div id="choice-rows" class="choices" role="listbox" aria-label="${t("장치 목록")}"></div></div><div id="empty-state" class="discovery-panel t-panel-slide" data-open="false" inert aria-hidden="true"><div class="empty-state"><span class="empty-icon" aria-hidden="true">${icons.tv}</span><div class="empty-copy"><p>${t("장치를 찾을 수 없어요")}</p><small>${t("같은 네트워크에 연결되어 있는지 확인해주세요")}</small></div></div></div><div id="search-status" class="discovery-panel t-panel-slide" data-open="false" inert aria-hidden="true"><div class="search-status" role="status" aria-label="${t("장치 검색 중")}"><div class="search-progress" aria-hidden="true"><span></span></div></div></div></div></div><footer class="page-footer"><button id="refresh" class="action refresh" aria-label="${t("다시 검색")}">${icons.refresh}</button>${action(t("직접 입력"), 'manual')}${action(t("연결"), 'pair', true, true)}</footer>`, backward);
  if (mounted) {
    el('manual').onclick = () => { manual = true; render(); };
    el('refresh').onclick = () => void search();
    el('pair').onclick = () => { const choice = state.choices.find(c => c.id === selected); if (choice) void pair({ deviceId: choice.id, ip: null, mac: null }, choice.name); };
    el('list-scroll').onscroll = () => { const scrolled = el('list-scroll').scrollTop > 58; windowTitle.set(scrolled ? t('연결할 장치를 선택하세요') : null, addingDevice); app.classList.toggle('scrolled', scrolled); };
  }
  const busy = localBusy || state.busy || state.stage === 'searching';
  // Keep outgoing rows intact while their panel fades into the search state.
  if (!busy || mounted) syncRows(el('choice-rows'), state.choices, choice => choice.id, choice => {
    const row = document.createElement('div'); row.className = 'device-choice-row'; row.setAttribute('role', 'presentation');
    const button = document.createElement('button'); button.type = 'button'; button.tabIndex = -1; button.className = 'device-choice'; button.setAttribute('role', 'option');
    button.innerHTML = `${icons.tv}<span class="choice-name"></span><span class="selection-check">${check}</span>`;
    button.onclick = () => {
      if (localBusy || state.busy || state.stage === 'searching') return;
      selected = choice.id; render();
    };
    row.append(button);
    return row;
  }, (row, choice) => {
    const button = row.querySelector<HTMLButtonElement>('.device-choice')!;
    row.querySelector('.choice-name')!.textContent = choice.name;
    button.setAttribute('aria-selected', String(selected === choice.id));
    button.disabled = busy;
  }, !mounted && el('choices-panel').dataset.open === 'true');
  const activePanel = busy ? 'search-status' : state.stage === 'not_found' ? 'empty-state' : 'choices-panel';
  const discovery = el('discovery-state');
  const showResults = activePanel === 'choices-panel' && discovery.dataset.state === 'search-status';
  // Only this content region changes; heading and footer keep their DOM and position.
  for (const panel of Array.from(discovery.children) as HTMLElement[]) {
    const open = panel.id === activePanel;
    panel.inert = !open;
    panel.setAttribute('aria-hidden', String(!open));
    panel.dataset.open = String(open);
  }
  discovery.dataset.state = activePanel;
  if (showResults) revealChoices(el('choice-rows'));
  el('refresh').classList.toggle('spinning', busy);
  el<HTMLButtonElement>('refresh').disabled = busy;
  el<HTMLButtonElement>('pair').disabled = !selected || busy;
}
function renderForm(backward: boolean) {
  const key = editing ? `edit-${editing.id}` : 'manual';
  const segments = (kind: 'ip' | 'mac', count: number, placeholder: string) => `<div class="segments ${kind}" data-kind="${kind}">${Array.from({ length: count }, (_, i) => `${i ? `<span class="separator ${kind === 'mac' ? 'colon' : ''}" aria-hidden="true"></span>` : ''}<span class="segment-cell"><input type="text" aria-label="${kind === 'ip' ? t("IP 주소") : t("MAC 주소")} ${i + 1}" data-index="${i}" inputmode="${kind === 'ip' ? 'numeric' : 'text'}" maxlength="${kind === 'ip' ? 3 : 2}" placeholder="${placeholder}" autocomplete="off" autocapitalize="characters" spellcheck="false"><span class="empty-caret" aria-hidden="true">${placeholder}</span></span>`).join('')}</div>`;
  if (!mount(key, `<form id="manual-form" class="form-page"><div class="page-scroll">${title(editing ? t("정보 수정") : t("직접 입력"))}<div class="field-group"><label>${t("IP 주소")}</label>${segments('ip', 4, '0')}</div><div class="field-group"><label>${editing ? t("MAC 주소") : t("MAC 주소 (선택)")}</label>${segments('mac', 6, '00')}</div><p id="form-error" class="form-error" role="alert"></p></div><footer class="page-footer">${action(editing ? t("저장") : t("연결"), 'submit-manual', true, !editing)}</footer></form>`, backward)) return;
  const form = el<HTMLFormElement>('manual-form');
  const allFields = Array.from(form.querySelectorAll<HTMLInputElement>('input'));
  allFields.forEach(input => input.tabIndex = 0);
  if (editing) {
    form.querySelectorAll<HTMLInputElement>('.ip input').forEach((input, i) => input.value = editing!.ip.split('.')[i] ?? '');
    form.querySelectorAll<HTMLInputElement>('.mac input').forEach((input, i) => input.value = editing!.mac?.split(':')[i] ?? '');
  }
  const values = (kind: string) => Array.from(form.querySelectorAll<HTMLInputElement>(`.${kind} input`), i => i.value);
  const validate = () => {
    const ip = values('ip'), mac = values('mac');
    const validIp = ip.every(v => /^\d{1,3}$/.test(v) && Number(v) <= 255) && !['0.0.0.0', '255.255.255.255'].includes(ip.join('.')) && Number(ip[0]) < 224;
    const validMac = validMacSegments(mac);
    el('form-error').textContent = mac.some(v => /[G-Z]/.test(v)) ? t("MAC 주소에는 0–9와 A–F를 사용해주세요.") : '';
    el<HTMLButtonElement>('submit-manual').disabled = !validIp || !validMac || localBusy;
    return validIp && validMac;
  };
  allFields.forEach((input, fieldIndex) => {
    const kind = input.closest<HTMLElement>('.segments')!.dataset.kind as 'ip' | 'mac';
    const fields = Array.from(form.querySelectorAll<HTMLInputElement>(`.${kind} input`));
    const nextField = () => allFields[fieldIndex + 1]?.focus({ preventScroll: true });
    const accept = () => {
      input.value = cleanSegment(kind, input.value);
      // A two-digit octet above 25 cannot accept a valid third digit.
      const complete = input.value.length === input.maxLength || (kind === 'ip' && Number(input.value) > 25);
      if (complete && (kind !== 'ip' || Number(input.value) <= 255)) nextField();
      validate();
    };
    input.oninput = event => { if (!(event as InputEvent).isComposing) accept(); };
    input.addEventListener('compositionend', accept);
    input.onkeydown = event => {
      if ((event.key === 'Backspace' && !input.value) || (event.key === 'ArrowLeft' && input.selectionStart === 0)) {
        if (fieldIndex > 0) { event.preventDefault(); allFields[fieldIndex - 1].focus({ preventScroll: true }); }
      }
      if (event.key === '.' || event.key === ':' || event.key === '-' || (event.key === 'Enter' && allFields[fieldIndex + 1]) || (event.key === 'ArrowRight' && input.selectionStart === input.value.length)) { event.preventDefault(); nextField(); }
    };
    input.onpaste = event => {
      const text = event.clipboardData?.getData('text').trim() ?? '';
      const chunks = kind === 'mac' && /^[a-z0-9]{12}$/i.test(text) ? text.match(/.{2}/g)! : text.split(kind === 'ip' ? /\./ : /[:-]/);
      event.preventDefault();
      if (chunks.length === fields.length) {
        fields.forEach((field, i) => field.value = cleanSegment(kind, chunks[i]).slice(0, field.maxLength));
        const next = kind === 'ip' ? allFields[fields.length] : fields.at(-1);
        next?.focus({ preventScroll: true }); validate();
      }
      else { const start = input.selectionStart ?? 0, end = input.selectionEnd ?? start; input.value = (input.value.slice(0, start) + cleanSegment(kind, text) + input.value.slice(end)).slice(0, input.maxLength); accept(); }
    };
  });
  el<HTMLButtonElement>('submit-manual').type = 'submit';
  form.onsubmit = async event => {
    event.preventDefault(); if (!validate()) return;
    const ip = values('ip').join('.'), mac = values('mac').some(Boolean) ? values('mac').join(':') : null;
    if (editing) {
      const result = await run('edit_device', { id: editing.id, ip, mac });
      if (result) { editing = null; manual = false; view = ''; render(true); }
    } else { manual = false; await pair({ deviceId: null, ip, mac }, ip); }
  };
  validate();
  allFields[0]?.focus({ preventScroll: true });
}

function mountConnection() {
  const failed = state.stage === 'pair_error';
  const initialTitle = failed ? t("연결하지 못했어요") : t("TV에서 연결을 허용해 주세요");
  const mounted = mount('connection', `<div class="connection-page" data-outcome="${failed ? 'failure' : 'loading'}"><div class="connection-copy"><div id="connection-title-slot" class="connection-title-slot t-resize" style="height:32px"><h1 id="connection-title" class="t-text-swap">${escape(initialTitle)}</h1></div><p id="connection-subtitle" class="t-text-swap">${escape(failed ? t("TV 전원과 네트워크를 확인해주세요") : pairingLabel || state.name)}</p></div><div id="connection-indicator" class="connection-indicator" role="status" aria-label="${t("연결 요청 중")}"></div></div><footer id="connection-footer" class="page-footer connection-footer" data-visible="${failed}" ${failed ? '' : 'inert'}>${action(t("다시 시도"), 'retry', true)}</footer>`);
  if (mounted) {
    indicator = new ConnectionIndicator(el('connection-indicator'));
    el('retry').onclick = () => void pair(pairArgs, pairingLabel);
  }
  return mounted;
}
function connectionCopy(outcome: 'loading' | 'success' | 'failure') {
  const page = current().querySelector<HTMLElement>('.connection-page')!;
  const changed = page.dataset.outcome !== outcome;
  page.dataset.outcome = outcome;
  const title = outcome === 'loading' ? t("TV에서 연결을 허용해 주세요") : outcome === 'success' ? t("연결 완료") : t("연결하지 못했어요");
  const subtitle = outcome === 'failure' ? t("TV 전원과 네트워크를 확인해주세요") : pairingLabel || state.name;
  void swapText(el('connection-title'), title, el('connection-title-slot'), changed);
  void swapText(el('connection-subtitle'), subtitle, undefined, changed);
  const footer = el('connection-footer');
  footer.dataset.visible = String(outcome === 'failure'); footer.inert = outcome !== 'failure';
  el('connection-indicator').setAttribute('aria-label', outcome === 'loading' ? t("연결 요청 중") : outcome === 'success' ? t("연결 완료") : t("연결 실패"));
}
function renderPair(reset = false) {
  const mounted = mountConnection();
  if (mounted || reset) { connectionCopy('loading'); indicator!.start(); }
}
function renderFailure() {
  const mounted = mountConnection();
  connectionCopy('failure');
  if (mounted) indicator!.showResult('failure');
}
function latestPairResult(result: Snapshot): Snapshot {
  return pendingPairSnapshot?.stage === result.stage ? pendingPairSnapshot : result;
}
async function finishConnection(result: Snapshot, token: number) {
  if (token !== generation) return;
  pendingPairSnapshot = null;
  // Pairing may succeed while automatic-sync setup reports a separate home error.
  const outcome = result.stage === 'ready' ? 'success' : 'failure';
  await indicator!.finish(outcome, () => { if (token === generation) connectionCopy(outcome); });
  if (token !== generation) return;
  if (outcome === 'success') {
    // This is reading time, including when motion itself is reduced.
    const hold = cssTimeMs(getComputedStyle(el('connection-indicator')).getPropertyValue('--connection-result-hold'), 900);
    await pause(hold);
  }
  if (token !== generation) return;
  state = latestPairResult(result);
  pairingToken = null; pendingPairSnapshot = null;
  if (outcome === 'success') { render(); reportError(state.error); }
  else { state.stage = 'pair_error'; renderFailure(); }
}
async function pair(args: Record<string, unknown>, label: string) {
  const token = ++generation;
  pairingToken = token; pendingPairSnapshot = null;
  pairArgs = args; pairingLabel = label; manual = false; editing = null;
  state = { ...state, stage: 'pairing', error: null }; renderPair(true);
  try { await finishConnection(await command('pair_tv', args), token); }
  catch { await finishConnection({ ...state, stage: 'pair_error', busy: false }, token); }
}
function updateShortcutButton() {
  updateShortcut('shortcut', state.shortcut, capture && captureTarget === 'sleep');
  if (document.getElementById('screen-shortcut')) updateShortcut('screen-shortcut', state.screen_shortcut, capture && captureTarget === 'screen');
}
function updateShortcut(id: string, value: string | null, capturing: boolean) {
  const button = el<HTMLButtonElement>(id);
  button.setAttribute('aria-pressed', String(capturing));
  button.setAttribute('aria-busy', String(capturing && shortcutSaving));
  const waitingForShortcut = capturing && shortcutHint === t("키를 눌러주세요");
  swapShortcutLabel(button, capturing ? shortcutHint : value ? shortcutLabel(value) : t("단축키 지정"), capturing, waitingForShortcut, waitingForShortcut);
}
function updateUpdateButton() {
  const button = el<HTMLButtonElement>('check-update');
  button.disabled = appUpdate.busy;
  button.setAttribute('aria-busy', String(appUpdate.busy));
  swapButtonLabel(button, t(appUpdate.label), appUpdate.busy, appUpdate.busy, appUpdate.busy);
  const version = el('app-version');
  if (version.textContent !== appUpdate.version) void swapText(version, appUpdate.version);
}
const languageNames: Record<Language, string> = { ko: '한국어', en: 'English', ja: '日本語' };
const themeName = (value: Theme) => t(value === 'system' ? '시스템' : value === 'light' ? '라이트' : '다크');
const chevron = icons.chevronRight.replace('<svg ', '<svg class="preference-chevron" ');
function refreshWindowLabels() {
  app.setAttribute('aria-label', t('LG TV 전원 제어'));
  el('minimize').setAttribute('aria-label', t('최소화'));
  el('close').setAttribute('aria-label', t('닫기'));
  windowTitle.set(preferencePage ? t(preferencePage === 'language' ? '언어' : '테마') : null, !!preferencePage);
  el('navigation').setAttribute('aria-label', t(view === 'home' ? '기기 추가' : '뒤로'));
}
function renderPreference(backward = false) {
  const page = preferencePage!;
  const options = page === 'language' ? Object.entries(languageNames) : (['system', 'light', 'dark'] as Theme[]).map(value => [value, themeName(value)]);
  const selected = page === 'language' ? language : theme;
  const heading = t(page === 'language' ? '언어' : '테마');
  if (!mount(page, `<div class="page-scroll preference-page"><div role="radiogroup" aria-label="${heading}">${options.map(([value, label]) => `<button class="preference-option" role="radio" data-value="${value}" aria-checked="${value === selected}"><span lang="${page === 'language' ? value : language}">${label}</span><span class="selection-check">${check}</span></button>`).join('')}</div></div>`, backward)) return;
  refreshWindowLabels();
  const buttons = Array.from(current().querySelectorAll<HTMLButtonElement>('.preference-option'));
  buttons.forEach(button => {
    button.tabIndex = button.dataset.value === selected ? 0 : -1;
    button.onclick = () => {
      const value = button.dataset.value!;
      if (page === 'language') {
        setLanguage(value as Language); view = ''; renderPreference();
        current().querySelector<HTMLButtonElement>(`[data-value="${value}"]`)!.focus({ preventScroll: true });
      } else {
        setTheme(value as Theme);
        buttons.forEach(option => { option.setAttribute('aria-checked', String(option === button)); option.tabIndex = option === button ? 0 : -1; });
      }
    };
    button.onkeydown = event => {
      if (!['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
      event.preventDefault();
      const index = buttons.indexOf(button);
      const next = buttons[event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : (index + (['ArrowUp', 'ArrowLeft'].includes(event.key) ? -1 : 1) + buttons.length) % buttons.length];
      next.focus(); next.click();
    };
  });
}
function renderHome(backward: boolean) {
  addingDevice = false;
  const toggle = (id: string, on: boolean, label: string) => `<button id="${id}" class="t-toggle" role="switch" aria-label="${label}" aria-checked="${on}" data-on="${on}"><span class="t-toggle-thumb"></span></button>`;
  const mounted = mount('home', `<div class="page-scroll home-scroll"><section class="device-section"><h2>${t("기기 목록")}</h2><div id="saved-devices" class="saved-devices"></div><div id="home-devices-empty" class="home-devices-empty" data-open="${state.devices.length === 0}" aria-hidden="${state.devices.length > 0}"><div><p>${t("연결된 기기가 없습니다")}</p></div></div></section><section class="settings-section"><h2>${t("전원")}</h2><div class="setting-row"><div class="setting-copy"><p>${t("전원 동기화")}</p><small>${t("PC 전원에 맞춰 TV도 자동으로 켜지고 꺼집니다")}</small></div>${toggle('automatic', state.automatic, t("전원 동기화"))}</div><div class="divider"></div><div class="setting-row"><div class="setting-copy"><p>${t("TV 단독 사용 시 전원 유지")}</p><small>${t("PC가 아닌 TV 화면을 사용 중일 때는 TV를 끄지 않습니다")}</small></div>${toggle('keep-tv-on', state.keep_tv_on, t("TV 단독 사용 시 전원 유지"))}</div><div class="divider"></div><div class="shortcut-group"><div class="setting-row"><div class="setting-copy"><p>${t("절전 단축키")}</p><small>${t("PC와 TV를 함께 절전 상태로 전환합니다")}</small></div>${toggle('shortcut-enabled', state.shortcut_enabled, t("절전 단축키"))}</div><div id="shortcut-settings" class="t-acc shortcut-panel" data-open="${state.shortcut_enabled}"><div class="t-acc-panel"><div class="t-acc-panel-inner"><div class="shortcut-setting"><p>${t("단축키 설정")}</p><button id="shortcut" class="shortcut-button"><span id="shortcut-label" class="t-text-swap">${escape(state.shortcut ? shortcutLabel(state.shortcut) : t("단축키 지정"))}</span></button></div></div></div></div></div></section></div>`, backward);
  if (mounted) {
    const settings = current().querySelector<HTMLElement>('.settings-section')!;
    settings.insertAdjacentHTML('afterend', screenMarkup(state.screen_enabled, state.screen_shortcut_enabled));
    settings.insertAdjacentHTML('afterend', `<section class="general-section"><h2>${t('일반')}</h2><div class="setting-row preference-row"><span id="language-label" class="setting-copy">${t('언어')}</span><button id="language-setting" class="preference-link" aria-labelledby="language-label language-value"><span id="language-value" class="preference-value">${languageNames[language]}</span>${chevron}</button></div><div class="divider"></div><div class="setting-row preference-row"><span id="theme-label" class="setting-copy">${t('테마')}</span><button id="theme-setting" class="preference-link" aria-labelledby="theme-label theme-value"><span id="theme-value" class="preference-value">${themeName(theme)}</span>${chevron}</button></div></section><section class="about-section"><div class="setting-row update-row"><div class="setting-copy"><p>LTAC</p><small id="app-version" class="t-text-swap">${escape(appUpdate.version)}</small></div><button id="check-update" class="shortcut-button"><span class="t-text-swap" aria-live="polite" aria-atomic="true">${t(appUpdate.label)}</span></button></div></section>`);
    for (const page of ['language', 'theme'] as const) el(`${page}-setting`).onclick = () => { preferencePage = page; render(); };
    // The screen section precedes General in the home layout.
    const section = current().querySelector('.screen-section')!;
    settings.after(section);
    el('screen-enabled').onclick = () => void saveScreen({ enabled: !state.screen_enabled });
    el('screen-shortcut-enabled').onclick = () => void saveScreen({ shortcutEnabled: !state.screen_shortcut_enabled });
    el('screen-shortcut').onclick = () => {
      if (shortcutSaving) return;
      captureTarget = 'screen'; capture = true; shortcutHint = t('키를 눌러주세요');
      updateShortcutButton(); el('screen-shortcut').focus({ preventScroll: true }); toast.hide();
    };
    el('screen-shortcut').onblur = () => {
      if (!capture || captureTarget !== 'screen' || shortcutSaving) return;
      capture = false; updateShortcutButton();
    };
    el('screen-time').onclick = event => { event.stopPropagation(); openScreenTime(); };
    for (const id of ['screen-enabled', 'screen-shortcut-enabled', 'screen-shortcut', 'screen-time']) el(id).tabIndex = 0;
    refreshWindowLabels();
    disableTabStops(settings);
    el('check-update').onclick = () => void appUpdate.check();
    const scroll = current().querySelector<HTMLElement>('.home-scroll')!;
    scroll.onscroll = () => {
      if (view === 'home' && current().contains(scroll)) app.classList.toggle('scrolled', scroll.scrollTop > 0);
    };
    el('automatic').onclick = async () => { await run('set_automatic', { enabled: !state.automatic }); if (view === 'home') renderHome(false); };
    el('keep-tv-on').onclick = async () => { await run('set_keep_tv_on', { enabled: !state.keep_tv_on }); if (view === 'home') renderHome(false); };
    el('shortcut-enabled').onclick = async () => { await run('set_shortcut', { enabled: !state.shortcut_enabled, shortcut: state.shortcut }); if (view === 'home') renderHome(false); };
    el('shortcut').onclick = () => {
      if (shortcutSaving) return;
      captureTarget = 'sleep'; capture = true; shortcutHint = t("키를 눌러주세요");
      updateShortcutButton();
      el('shortcut').focus({ preventScroll: true });
      toast.hide();
    };
    el('shortcut').onblur = () => {
      if (!capture || captureTarget !== 'sleep' || shortcutSaving) return;
      capture = false; updateShortcutButton();
    };
  }
  syncRows(el('saved-devices'), state.devices, device => device.id, device => {
    const row = document.createElement('div'); row.className = 'saved-device';
    row.innerHTML = `<span class="tv-tile">${icons.tv}</span><div class="device-info"><p class="saved-name"></p><div class="hdmi-anchor"></div></div><div class="more-anchor"><div class="t-morph device-menu" data-open="false"><div class="t-morph-menu" role="menu" inert><button role="menuitem" data-action="edit">${icons.edit}<span>${t("정보 수정")}</span></button><button role="menuitem" class="destructive" data-action="remove">${icons.delete}<span>${t("삭제")}</span></button></div><button class="t-morph-plus more-button" aria-label="${escape(device.name)} ${t("메뉴")}" aria-expanded="false" aria-haspopup="menu">${icons.more}</button></div></div>`;
    hdmiMenus.set(row, new HdmiMenu(row.querySelector('.hdmi-anchor')!, (menu, trigger) => {
      if (localBusy) return;
      const opening = menu.dataset.open !== 'true'; closeMenu();
      if (opening) {
        const bottom = current().querySelector('.home-scroll')!.getBoundingClientRect().bottom;
        const height = parseFloat(getComputedStyle(menu).getPropertyValue('--menu-open-height'));
        menu.dataset.side = bottom - trigger.getBoundingClientRect().bottom < height + 6 ? 'up' : 'down';
        closingMenus.delete(menu); delete menu.dataset.closing;
        const dropdown = menu.querySelector<HTMLElement>('.t-dropdown')!;
        dropdown.dataset.origin = menu.dataset.side === 'up' ? 'bottom-left' : 'top-left';
        dropdown.classList.remove('is-closing');
        fitMenuWidth(menu); activeMenu = menu; menu.dataset.open = 'true';
        dropdown.classList.add('is-open');
        menu.querySelector<HTMLElement>('[role=menu]')!.inert = false;
        trigger.setAttribute('aria-expanded', 'true');
        (menu.querySelector<HTMLButtonElement>('[aria-checked=true]') ?? menu.querySelector<HTMLButtonElement>('[role=menuitemradio]'))!.focus({ preventScroll: true });
      }
    }, async port => {
      const saved = await run('set_device_hdmi', { id: device.id, pcHdmi: port });
      if (view === 'home') renderHome(false);
      return saved;
    }, menu => { if (activeMenu === menu) closeMenu(true); }));
    const button = row.querySelector<HTMLButtonElement>('.more-button')!;
    disableTabStops(row);
    button.onclick = event => {
      event.stopPropagation(); if (localBusy) return;
      const menu = button.closest<HTMLElement>('.t-morph')!;
      const opening = menu.dataset.open !== 'true'; closeMenu();
      if (opening) { closingMenus.delete(menu); delete menu.dataset.closing; fitMenuWidth(menu); activeMenu = menu; menu.dataset.open = 'true'; menu.querySelector<HTMLElement>('.t-morph-menu')!.inert = false; button.setAttribute('aria-expanded', 'true'); menu.querySelector<HTMLButtonElement>('[role=menuitem]')!.focus({ preventScroll: true }); }
    };
    row.querySelectorAll<HTMLButtonElement>('[role=menuitem]').forEach(item => item.onclick = async () => {
      if (localBusy) return;
      const saved = state.devices.find(d => d.id === device.id); closeMenu();
      if (!saved) return;
      if (item.dataset.action === 'edit') { editing = saved; render(); }
      else {
        await run('remove_device', { id: saved.id });
        if (view === 'home') renderHome(false);
      }
    });
    return row;
  }, (row, device) => {
    for (const [selector, text] of [['.saved-name', device.name]]) {
      const node = row.querySelector<HTMLElement>(selector)!;
      if (!node.textContent) node.textContent = text;
      else if (node.textContent !== text) void swapText(node, text);
    }
    hdmiMenus.get(row)!.update(device);
  }, !mounted);
  const empty = state.devices.length === 0;
  current().querySelector<HTMLElement>('.device-section')!.dataset.empty = String(empty);
  const emptyNotice = el('home-devices-empty');
  emptyNotice.dataset.open = String(empty);
  emptyNotice.setAttribute('aria-hidden', String(!empty));
  for (const [id, on] of [['automatic', state.automatic], ['keep-tv-on', state.keep_tv_on], ['shortcut-enabled', state.shortcut_enabled], ['screen-enabled', state.screen_enabled], ['screen-shortcut-enabled', state.screen_shortcut_enabled]] as const) {
    const button = el<HTMLButtonElement>(id);
    button.dataset.on = String(on);
    button.setAttribute('aria-checked', String(on)); button.disabled = localBusy;
  }
  const settings = el('shortcut-settings');
  if (!state.shortcut_enabled && capture && captureTarget === 'sleep') {
    capture = false; toast.hide();
    if (settings.contains(document.activeElement)) el('shortcut-enabled').focus({ preventScroll: true });
  }
  settings.dataset.open = String(state.shortcut_enabled);
  settings.inert = !state.shortcut_enabled;
  settings.setAttribute('aria-hidden', String(!state.shortcut_enabled));
  el('shortcut-enabled').setAttribute('aria-controls', 'shortcut-settings');
  for (const [id, open, trigger] of [['screen-settings', state.screen_enabled, 'screen-enabled'], ['screen-shortcut-settings', state.screen_enabled && state.screen_shortcut_enabled, 'screen-shortcut-enabled']] as const) {
    const panel = el(id);
    if (!open && panel.contains(document.activeElement)) el(trigger).focus({ preventScroll: true });
    panel.dataset.open = String(open); panel.inert = !open; panel.setAttribute('aria-hidden', String(!open));
    el(trigger).setAttribute('aria-controls', id);
  }
  if ((!state.screen_enabled || !state.screen_shortcut_enabled) && captureTarget === 'screen') capture = false;
  if (!state.screen_enabled && activeMenu?.classList.contains('screen-time-menu')) closeMenu();
  updateScreenTime();
  updateShortcutButton();
  updateUpdateButton();
  reportError(state.error);
}
async function saveScreen(patch: Record<string, unknown> = {}, announce = true): Promise<boolean> {
  const saved = await run('set_screen_protection', {
    enabled: state.screen_enabled, timeoutSeconds: screenDraft ?? state.screen_timeout_seconds,
    shortcutEnabled: state.screen_shortcut_enabled, shortcut: state.screen_shortcut, ...patch,
  }, announce);
  if (view === 'home') renderHome(false);
  return saved;
}
function queueScreenTime(delay = 200) {
  clearTimeout(screenSaveTimer);
  screenSaveTimer = setTimeout(async () => {
    if (screenDraft === null) return;
    if (localBusy) { queueScreenTime(); return; }
    const value = screenDraft;
    const saved = await saveScreen({ timeoutSeconds: value });
    if (screenDraft === value) screenDraft = null;
    if (!saved || view === 'home') updateScreenTime();
  }, delay);
}
function updateScreenTime() {
  if (view !== 'home') return;
  const value = screenDraft ?? state.screen_timeout_seconds;
  const label = screenTimeLabel(value);
  updateScreenTimeLabel(el<HTMLButtonElement>('screen-time'), label);
  app.querySelectorAll<HTMLElement>('.screen-time-menu[data-closing="true"]').forEach(menu => fitScreenTimeClose(menu, el<HTMLButtonElement>('screen-time')));
  if (activeMenu?.classList.contains('screen-time-menu')) {
    activeMenu.querySelectorAll<HTMLElement>('[role=menuitemradio]').forEach(item => item.setAttribute('aria-checked', String(Number(item.dataset.seconds) === value)));
  }
}
function openScreenTime() {
  const wasOpen = activeMenu?.classList.contains('screen-time-menu'); closeMenu();
  if (wasOpen || localBusy) return;
  const button = el<HTMLButtonElement>('screen-time');
  const box = button.getBoundingClientRect();
  const bounds = app.getBoundingClientRect();
  const menu = document.createElement('div');
  menu.className = 't-morph device-menu screen-time-menu'; menu.dataset.open = 'false';
  const anchor = button.parentElement!.getBoundingClientRect();
  menu.dataset.anchorTop = String(anchor.top); menu.dataset.anchorRight = String(anchor.right);
  menu.dataset.viewportWidth = String(innerWidth); menu.dataset.viewportHeight = String(innerHeight);
  const value = screenDraft ?? state.screen_timeout_seconds;
  menu.innerHTML = `<div class="t-morph-menu app-scrollbar" role="menu" aria-labelledby="screen-time-label" inert>${screenTimeOptions(value).map(seconds => `<button role="menuitemradio" aria-checked="${seconds === value}" data-seconds="${seconds}"><span>${screenTimeLabel(seconds)}</span><span class="screen-preset-check" aria-hidden="true">${check}</span></button>`).join('')}</div><span class="t-morph-plus" aria-hidden="true">${escape(screenTimeLabel(value))}</span>`;
  app.querySelector('.app-window')!.append(menu);
  fitMenuWidth(menu);
  mountScreenTimeScrollbar(menu);
  const width = Math.max(box.width, parseFloat(menu.style.getPropertyValue('--menu-open-width')));
  const height = Math.min(parseFloat(getComputedStyle(menu).getPropertyValue('--screen-menu-max-height')), bounds.height - 32);
  menu.style.setProperty('--menu-open-height', `${height}px`);
  menu.style.setProperty('--menu-open-width', `${width}px`);
  menu.style.setProperty('--screen-trigger-width', `${box.width}px`);
  menu.style.setProperty('--screen-trigger-height', `${box.height}px`);
  const left = Math.max(bounds.left + 16, Math.min(box.left, bounds.right - width - 16));
  const top = Math.max(bounds.top + 16, Math.min(box.top, bounds.bottom - height - 16));
  menu.style.left = `${box.left}px`; menu.style.top = `${box.top}px`;
  menu.style.setProperty('--screen-menu-x', `${left - box.left}px`);
  menu.style.setProperty('--screen-menu-y', `${top - box.top}px`);
  menu.style.transformOrigin = `${box.width / 2}px ${box.height / 2}px`;
  copyScreenTimeLabel(menu, button);
  menu.getBoundingClientRect();
  button.style.visibility = 'hidden'; button.setAttribute('aria-expanded', 'true');
  activeMenu = menu; menu.dataset.open = 'true';
  menu.querySelector<HTMLElement>('[role=menu]')!.inert = false;
  menu.querySelectorAll<HTMLButtonElement>('[role=menuitemradio]').forEach(item => item.onclick = () => {
    screenDraft = Number(item.dataset.seconds); updateScreenTime(); closeMenu(true); queueScreenTime(0);
  });
  const selected = (menu.querySelector<HTMLElement>('[aria-checked=true]') ?? menu.querySelector<HTMLElement>('[role=menuitemradio]'))!;
  const choices = menu.querySelector<HTMLElement>('[role=menu]')!;
  selected.focus({ preventScroll: true });
  choices.scrollTop = Math.max(0, selected.offsetTop - (height - selected.offsetHeight) / 2);
  // Hidden trigger digits can still be popping during a fast reopen. Only the
  // surface/list reveal determines when the scrollbar is safe to show.
  void Promise.allSettled([...menu.getAnimations(), ...choices.getAnimations()].map(animation => animation.finished)).then(() => {
    if (activeMenu === menu && menu.dataset.open === 'true') menu.dataset.scrollReady = 'true';
  });
}
function fitMenuWidth(menu: HTMLElement) {
  const content = getComputedStyle(menu.querySelector<HTMLElement>('[role=menu]')!);
  const outerPadding = parseFloat(content.paddingLeft) + parseFloat(content.paddingRight);
  const widths = Array.from(menu.querySelectorAll<HTMLButtonElement>('[role=menuitem], [role=menuitemradio]'), item => {
    const style = getComputedStyle(item);
    const icon = item.querySelector('svg');
    const iconWidth = icon ? parseFloat(getComputedStyle(icon).width) : 0;
    const textWidth = item.querySelector<HTMLElement>('span')!.scrollWidth;
    return outerPadding + parseFloat(style.paddingLeft) + iconWidth + parseFloat(style.columnGap) + textWidth + parseFloat(style.paddingRight);
  });
  const minimum = parseFloat(getComputedStyle(menu).getPropertyValue('--menu-min-width'));
  menu.style.setProperty('--menu-open-width', `${Math.ceil(Math.max(minimum, ...widths))}px`);
}
function closeMenu(focus = false) {
  if (!activeMenu) return;
  const menu = activeMenu;
  const screenTime = menu.classList.contains('screen-time-menu');
  if (screenTime) {
    fitScreenTimeClose(menu, el<HTMLButtonElement>('screen-time'));
    unmountScreenTimeScrollbar(menu);
  }
  const closing = {}; closingMenus.set(menu, closing);
  menu.dataset.closing = 'true'; menu.dataset.open = 'false';
  if (screenTime) trackScreenTimeClose(menu, el<HTMLButtonElement>('screen-time'));
  delete menu.dataset.scrollReady;
  menu.querySelector<HTMLElement>('[role=menu]')!.inert = true;
  const dropdown = menu.querySelector<HTMLElement>('.t-dropdown');
  dropdown?.classList.remove('is-open');
  dropdown?.classList.add('is-closing');
  const button = menu.classList.contains('screen-time-menu') ? el<HTMLButtonElement>('screen-time') : menu.querySelector<HTMLButtonElement>('.more-button, .hdmi-button')!; button.setAttribute('aria-expanded', 'false');
  if (focus && !screenTime) button.focus({ preventScroll: true }); activeMenu = null;
  // Preserve glass and stacking until the native surface transition finishes.
  // Cancelled/reversed animations cannot clean up a newer closing operation.
  void (async () => {
    do {
      await Promise.allSettled((dropdown ?? menu).getAnimations().map(animation => animation.finished));
    } while (screenTime && closingMenus.get(menu) === closing && menu.getAnimations().some(animation => animation.playState === 'running' || animation.pending));
    if (closingMenus.get(menu) === closing) {
      closingMenus.delete(menu); delete menu.dataset.closing;
      dropdown?.classList.remove('is-closing');
      if (screenTime) {
        followScreenTimeClose(menu); stopScreenTimeClose(menu);
        button.style.removeProperty('visibility'); menu.remove();
        if (focus) button.focus({ preventScroll: true });
      }
    }
  })();
}
function closeScreenTimeOnViewportChange(event: Event) {
  const button = document.getElementById('screen-time');
  const source = event.target;
  // A menu's own native scrolling must not dismiss it. Only its trigger's
  // scrolling ancestors (including the Web UI document) move the anchor.
  if (!button || (source !== window && (!(source instanceof Node) || !source.contains(button)))) return;
  const anchor = button.parentElement!.getBoundingClientRect();
  // A click can auto-scroll the trigger into view before opening the menu, yet
  // deliver that scroll event afterward. Dismiss only if its row really moved.
  const menus = Array.from(app.querySelectorAll<HTMLElement>('.screen-time-menu')).filter(menu =>
    Math.abs(anchor.top - Number(menu.dataset.anchorTop)) > .1 || Math.abs(anchor.right - Number(menu.dataset.anchorRight)) > .1
    || innerWidth !== Number(menu.dataset.viewportWidth) || innerHeight !== Number(menu.dataset.viewportHeight));
  for (const menu of menus) {
    if (activeMenu === menu) closeMenu(menu.contains(document.activeElement));
    else followScreenTimeClose(menu);
  }
}
async function run(name: string, args?: Record<string, unknown>, announce = true) {
  if (localBusy) return false;
  localBusy = true;
  state.error = null; lastError = null; toast.hide();
  const token = generation;
  try { const next = await command(name, args); if (token !== generation) return false; state = next; if (announce) reportError(state.error); return !state.error; }
  catch (error) { if (token !== generation) return false; state.error = error instanceof Error ? error.message : String(error); if (announce) reportError(state.error); return false; }
  finally { if (token === generation) localBusy = false; }
}
async function search() {
  const token = ++generation;
  pairingToken = null; pendingPairSnapshot = null;
  localBusy = false; capture = false; selected = null; manual = false; editing = null;
  if (view === 'choose') el('list-scroll').scrollTop = 0;
  state = { ...state, stage: 'searching', choices: [], busy: true }; render();
  await run('discover_tvs');
  if (token !== generation) return;
  state.busy = false;
  if (state.error) state.stage = 'not_found';
  render();
}
async function goBack() {
  if (preferencePage) { preferencePage = null; render(true); return; }
  // Returning from direct entry must keep an ongoing discovery request alive.
  if (manual) { manual = false; render(true); return; }
  generation++; pairingToken = null; pendingPairSnapshot = null; localBusy = false; indicator?.stop(); capture = false;
  if (editing) { editing = null; render(true); return; }
  if (await run('cancel_connection', { home: addingDevice })) { addingDevice = false; view = ''; render(true); }
}
el('navigation').onclick = () => {
  if (el('navigation').dataset.mode === 'add') { addingDevice = true; void search(); }
  else void goBack();
};
el('minimize').onclick = () => void windowAction('minimize');
el('close').onclick = () => void windowAction('close');
app.querySelector('header')!.addEventListener('pointerdown', event => { if (event.button === 0 && !(event.target as HTMLElement).closest('button')) void windowAction('drag'); });
document.addEventListener('click', event => { if (activeMenu && !activeMenu.contains(event.target as Node)) closeMenu(); });
window.addEventListener('scroll', closeScreenTimeOnViewportChange, { capture: true, passive: true });
window.addEventListener('resize', closeScreenTimeOnViewportChange);
window.addEventListener('keydown', async event => {
  if (capture) {
    event.preventDefault(); event.stopPropagation();
    if (shortcutSaving || event.repeat) return;
    if (event.key === 'Escape' && !event.ctrlKey && !event.altKey && !event.shiftKey && !event.metaKey) { capture = false; renderHome(false); return; }
    let shortcut: string | null;
    try { shortcut = shortcutFromEvent(event); }
    catch { shortcutHint = t("다른 키 사용"); updateShortcutButton(); return; }
    if (!shortcut) return;
    const token = generation;
    shortcutSaving = true; shortcutHint = t("저장 중");
    updateShortcutButton();
    const saved = captureTarget === 'screen'
      ? await saveScreen({ shortcut }, false)
      : await run('set_shortcut', { enabled: state.shortcut_enabled, shortcut }, false);
    if (token !== generation || view !== 'home') return;
    shortcutSaving = false; capture = !saved;
    if (!saved) { state.error = null; lastError = null; shortcutHint = t("다른 키 사용"); }
    renderHome(false); return;
  }
  if (event.key === 'Escape' && preferencePage) { event.preventDefault(); void goBack(); return; }
  if (event.key === 'Tab' && app.contains(document.activeElement) && current().querySelector('#manual-form')) {
    event.preventDefault();
    const fields = Array.from(current().querySelectorAll<HTMLInputElement>('#manual-form input'));
    const index = fields.indexOf(document.activeElement as HTMLInputElement);
    if (index >= 0) fields[(index + (event.shiftKey ? -1 : 1) + fields.length) % fields.length].focus({ preventScroll: true });
    return;
  }
  if (activeMenu) {
    if (event.key === 'Tab') closeMenu();
    if (event.key === 'Escape') { event.preventDefault(); closeMenu(true); }
    if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault(); const items = Array.from(activeMenu.querySelectorAll<HTMLButtonElement>('[role=menuitem], [role=menuitemradio]'));
      const index = items.indexOf(document.activeElement as HTMLButtonElement);
      items[event.key === 'Home' ? 0 : event.key === 'End' ? items.length - 1 : (index + (event.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length].focus();
    }
  }
}, true);
refreshWindowLabels();
if (previewModule) {
  previewModule.installPreview(async (patch, screen) => {
    generation++; preferencePage = null; pairingToken = null; pendingPairSnapshot = null; localBusy = false; indicator?.stop(); manual = screen.startsWith('manual'); addingDevice = false; editing = null; selected = screen === 'selected' || screen === 'scroll' ? 'tv-0' : null;
    pairingLabel = screen.includes('manual') ? '192.168.0.21' : patch.name;
    pairArgs = screen.includes('manual') ? { deviceId: null, ip: '192.168.0.21', mac: null } : { deviceId: 'tv-0', ip: null, mac: null };
    state = { ...initialState, ...patch }; view = ''; render();
    if (screen === 'manual-filled') {
      current().querySelectorAll<HTMLInputElement>('.ip input').forEach((input, i) => input.value = ['192', '168', '1', '21'][i]);
      Array.from(current().querySelectorAll<HTMLInputElement>('.ip input')).at(-1)!.dispatchEvent(new Event('input'));
    }
    if (screen === 'success' || screen === 'failure-motion') {
      const token = generation;
      const outcome = screen === 'success' ? 'success' : 'failure';
      await indicator?.finish(outcome, () => { if (token === generation) connectionCopy(outcome); });
      if (token === generation && outcome === 'failure') { state.stage = 'pair_error'; renderFailure(); }
    }
  });
} else {
  try { await connect(next => {
    if (pairingToken !== null) { pendingPairSnapshot = next; return; }
    state = { ...initialState, ...next }; if (!manual && !editing) render(); reportError(state.error);
  }, next => {
    // A hidden window may still hold a manual form or an add-device flow.
    // Opening LTAC again is a fresh entry into settings, not a resume of it.
    generation++; pairingToken = null; pendingPairSnapshot = null;
    localBusy = false; preferencePage = null; manual = false; editing = null; addingDevice = false; selected = null;
    closeMenu(); capture = false; shortcutSaving = false; toast.hide(true);
    // Reopening starts on home immediately, without revealing the old page
    // through an ordinary in-app navigation animation.
    hasMounted = false; view = '';
    state = { ...initialState, ...next }; render(); reportError(state.error);
  }); } catch { state.stage = 'not_found'; render(); }
}
