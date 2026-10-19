import { invoke, isTauri } from '@tauri-apps/api/core';
import { setTranslationLanguage } from './i18n';

export type Language = 'ko' | 'en' | 'ja';
export type Theme = 'system' | 'light' | 'dark';
const read = (key: string) => { try { return localStorage.getItem(key); } catch { return null; } };
const write = (key: string, value: string) => { try { localStorage.setItem(key, value); } catch { /* Session preferences still work without storage. */ } };
export function resolveLanguage(locale: string): Language {
  const language = locale.toLowerCase().split(/[-_]/)[0];
  return language === 'ko' || language === 'ja' ? language : 'en';
}
const savedLanguage = read('ltac.language');
const hasSavedLanguage = savedLanguage === 'ko' || savedLanguage === 'en' || savedLanguage === 'ja';
const savedTheme = read('ltac.theme');
export let language: Language = savedLanguage === 'ko' || savedLanguage === 'en' || savedLanguage === 'ja' ? savedLanguage : resolveLanguage(navigator.language);
export let theme: Theme = savedTheme === 'light' || savedTheme === 'dark' ? savedTheme : 'system';
const systemTheme = matchMedia('(prefers-color-scheme: dark)');
function applyTheme() {
  document.documentElement.dataset.theme = theme === 'system' ? systemTheme.matches ? 'dark' : 'light' : theme;
  window.dispatchEvent(new Event('ltac-theme-change'));
}
export async function initializePreferences() {
  if (!hasSavedLanguage && isTauri()) {
    try { language = resolveLanguage(await invoke<string>('system_locale')); } catch { /* WebView locale is the fallback. */ }
  }
  document.documentElement.lang = language;
  setTranslationLanguage(language);
  syncNativeLanguage();
  applyTheme();
}
export function setLanguage(value: Language) {
  language = value; write('ltac.language', value); document.documentElement.lang = value; setTranslationLanguage(value); syncNativeLanguage();
}
function syncNativeLanguage() {
  if (isTauri()) void invoke('set_ui_language', { language }).catch(() => {});
}
export function setTheme(value: Theme) { theme = value; write('ltac.theme', value); applyTheme(); }
systemTheme.addEventListener('change', () => { if (theme === 'system') applyTheme(); });
