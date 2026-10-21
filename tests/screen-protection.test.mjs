import assert from 'node:assert/strict';
import { test } from 'node:test';
import { screenTime, screenSeconds, screenTimeLabel, screenTimeOptions, screenMarkup } from '../ui/src/screen-protection.ts';
import { setTranslationLanguage } from '../ui/src/i18n.ts';

test('screen delay preserves saved integer values, preset choices and localized labels', () => {
  assert.equal(screenTime.default, 90);
  assert.equal(screenSeconds(9), 10);
  assert.equal(screenSeconds(29), 29);
  assert.equal(screenTime.presets[0], 10);
  assert.equal(screenSeconds(1801), 1800);
  assert.equal(screenSeconds(91.2), 91);
  for (const preset of screenTime.presets) assert.equal(screenSeconds(preset), preset);
  assert.deepEqual(screenTimeOptions(90), [...screenTime.presets]);
  const custom = screenTimeOptions(91);
  assert.equal(custom.filter(value => value === 91).length, 1);
  assert(custom.indexOf(90) < custom.indexOf(91) && custom.indexOf(91) < custom.indexOf(120));
  setTranslationLanguage('ko');
  assert.equal(screenTimeLabel(10), '10초');
  assert.equal(screenTimeLabel(30), '30초');
  assert.equal(screenTimeLabel(90), '1분 30초');
  assert.equal(screenTimeLabel(91), '1분 31초');
  assert.equal(screenTimeLabel(1800), '30분');
  setTranslationLanguage('en'); assert.equal(screenTimeLabel(91), '1m 31s');
  setTranslationLanguage('ja'); assert.equal(screenTimeLabel(91), '1分 31秒');
  setTranslationLanguage('ko');
});

test('screen protection markup contains time selection without a slider or range captions', () => {
  const markup=screenMarkup();
  assert(markup.includes('id="screen-time"'));
  assert(!markup.includes('screen-slider'));
  assert(!markup.includes('screen-time-bounds'));
  assert(!markup.includes('type="range"'));
});
