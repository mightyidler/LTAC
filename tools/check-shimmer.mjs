import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdirSync } from 'node:fs';
import { createServer } from 'vite';
import sharp from 'sharp';

const { chromium } = createRequire(import.meta.url)('playwright');
const evidence = new URL('../local/shimmer/', import.meta.url);
mkdirSync(evidence, { recursive: true });
const server = await createServer({ server: { port: 1423, strictPort: true } });
let browser;
try {
  await server.listen();
  browser = await chromium.launch({ channel: 'msedge', headless: true });
  for (const theme of ['dark', 'light']) {
    const page = await browser.newPage({ colorScheme: theme, reducedMotion: 'no-preference', locale: 'ko-KR', viewport: { width: 1000, height: 850 } });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto('http://127.0.0.1:1423/?screen=home');
    await page.selectOption('#preview-delay', '2400');
    await page.locator('#check-update').click();
    async function check(selector, text, name) {
      const label = page.locator(`${selector} > .t-text-swap:not(.shortcut-copy-outgoing)`);
      await label.filter({ hasText: text }).waitFor();
      await page.waitForTimeout(650);
      assert.equal(await label.getAttribute('data-text'), text);
      const effect = await label.evaluate(node => ({
        base: getComputedStyle(node).color,
        highlight: getComputedStyle(node).getPropertyValue('--shimmer-highlight').trim(),
        animation: getComputedStyle(node, '::before').animationName,
        animations: node.getAnimations({ subtree: true }).map(a => ({ name: a.animationName, pseudo: a.effect?.pseudoElement })),
      }));
      assert.equal(effect.animation, 't-shimmer');
      const shots = [];
      for (const time of [0, 1000]) {
        await label.evaluate((node, time) => {
          for (const animation of node.getAnimations({ subtree: true })) if (animation.animationName === 't-shimmer') {
            animation.pause(); animation.currentTime = time;
          }
        }, time);
        shots.push(await label.screenshot({ animations: 'allow' }));
      }
      const a = await sharp(shots[0]).raw().toBuffer(), b = await sharp(shots[1]).raw().toBuffer();
      let changed = 0;
      for (let i = 0; i < a.length; i++) if (Math.abs(a[i] - b[i]) > 5) changed++;
      assert(changed > 20, `${name}: shimmer must visibly change rendered glyph pixels, changed ${changed}`);
      await sharp(shots[1]).resize({ width: 400 }).png().toFile(new URL(`${theme}-${name}.png`, evidence).pathname.replace(/^\/(\w:)/, '$1'));
    }
    await check('#check-update', '확인 중', 'checking');
    await check('#check-update', '업데이트 진행 중', 'installing');
    await page.waitForFunction(() => !document.querySelector('#check-update').disabled);
    await page.waitForFunction(() => !document.querySelector('#check-update .t-shimmer'));
    assert.equal(await page.locator('#check-update .t-shimmer').count(), 0);
    await page.locator('#shortcut').click();
    await check('#shortcut', '키를 눌러주세요', 'shortcut');
    await page.keyboard.press('Escape');
    await page.waitForFunction(() => !document.querySelector('#shortcut .t-shimmer'));
    assert.equal(await page.locator('#shortcut .t-shimmer').count(), 0);
    await page.emulateMedia({ reducedMotion: 'reduce' });
    await page.locator('#shortcut').click();
    assert.equal(await page.locator('#shortcut .t-shimmer').evaluate(node => getComputedStyle(node, '::before').animationName), 'none');
    assert.deepEqual(errors, []);
    await page.close();
  }
  console.log('Update checking/installing and shortcut capture visibly shimmer in both themes; exit cleanup and reduced motion passed.');
} finally { await browser?.close(); await server.close(); }
