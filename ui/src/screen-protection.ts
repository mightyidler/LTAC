import { t } from './i18n.ts';
export const screenTime = { min: 10, max: 1800, default: 90, presets: [10, 30, 60, 90, 120, 180, 300, 600, 900, 1200, 1800] } as const;
export function screenSeconds(value: number): number { return Math.min(screenTime.max, Math.max(screenTime.min, Math.round(value))); }
export function screenTimeOptions(value: number): number[] {
  return [...new Set<number>([...screenTime.presets, screenSeconds(value)])].sort((a, b) => a - b);
}
export function screenTimeLabel(seconds: number): string {
  const minutes = Math.floor(seconds / 60), remainder = seconds % 60;
  return [minutes ? `${minutes}${t('분')}` : '', remainder ? `${remainder}${t('초')}` : ''].filter(Boolean).join(' ');
}
export function screenMarkup(enabled = false, shortcutEnabled = false): string {
  const toggle = (id: string, label: string) => {
    const on = id === 'screen-enabled' ? enabled : shortcutEnabled;
    return `<button id="${id}" class="t-toggle" role="switch" aria-label="${label}" aria-checked="${on}" data-on="${on}"><span class="t-toggle-thumb"></span></button>`;
  };
  return `<section class="screen-section"><h2>${t('화면 보호')}</h2><div class="setting-row"><div class="setting-copy"><p>${t('자동 화면 보호')}</p><small>${t('PC 미사용 시 화면을 끕니다')}</small></div>${toggle('screen-enabled', t('자동 화면 보호'))}</div><div id="screen-settings" class="t-acc shortcut-panel screen-panel" data-open="false" inert aria-hidden="true"><div class="t-acc-panel"><div class="t-acc-panel-inner"><div class="screen-time-controls"><div class="screen-time-setting"><p id="screen-time-label">${t('화면 보호 시작 시간')}</p><button id="screen-time" class="shortcut-button" aria-labelledby="screen-time-label screen-time-value" aria-haspopup="menu" aria-expanded="false"><span id="screen-time-value" class="t-digit-group"></span></button></div></div><div class="divider"></div><div class="shortcut-group"><div class="setting-row"><div class="setting-copy"><p>${t('화면 보호 단축키')}</p><small>${t('PC 단축키로 TV 화면을 끄고 켭니다')}</small></div>${toggle('screen-shortcut-enabled', t('화면 보호 단축키'))}</div><div id="screen-shortcut-settings" class="t-acc shortcut-panel" data-open="false" inert aria-hidden="true"><div class="t-acc-panel"><div class="t-acc-panel-inner"><div class="shortcut-setting"><p>${t('단축키 설정')}</p><button id="screen-shortcut" class="shortcut-button"><span class="t-text-swap"></span></button></div></div></div></div></div></div></div></div></section>`;
}
