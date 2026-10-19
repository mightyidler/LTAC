import assert from 'node:assert/strict';
import { t, localizeError, setTranslationLanguage } from '../ui/src/i18n.ts';
import { shortcutLabel } from '../ui/src/input.ts';

for (const [locale, heading, shortcut] of [['ko','언어','음소거'],['en','Language','Mute'],['ja','言語','ミュート']]) {
  setTranslationLanguage(locale);
  assert.equal(t('언어'), heading);
  assert.equal(shortcutLabel('Ctrl+AudioVolumeMute'), `Ctrl + ${shortcut}`, 'Shortcut labels must follow live language changes');
  const nativeError = '자동 동기화: TV에 연결할 수 없어요. TV 전원과 네트워크를 확인하고 다시 시도해 주세요. (192.0.2.1:3000)';
  const translated = localizeError(nativeError);
  assert(translated.includes('192.0.2.1:3000'), 'Keep device addresses and diagnostics');
  if (locale === 'ko') assert.equal(translated, nativeError);
  else assert(!/[가-힣]/.test(translated), 'Translate native error context as well as screen copy');
  assert.equal(localizeError('ECONNREFUSED (192.0.2.1:3000)'), 'ECONNREFUSED (192.0.2.1:3000)');
}
setTranslationLanguage('ko');
console.log('Localization: live copy/shortcut changes and native error translation preserve technical diagnostics.');
