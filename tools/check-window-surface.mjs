import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdirSync } from 'node:fs';
import { createServer } from 'vite';
import sharp from 'sharp';

const { chromium } = createRequire(import.meta.url)('playwright');
const evidence = new URL('../local/window-surface/', import.meta.url);
mkdirSync(evidence, { recursive: true });
const server = await createServer({ server: { port: 1424, strictPort: true } });
let browser;
try {
  await server.listen();
  browser = await chromium.launch({ channel: 'msedge', headless: true });
  for (const theme of ['light', 'dark']) for (const scale of [1, 1.25, 1.5, 1.75, 2]) {
    const page = await browser.newPage({ colorScheme: theme, deviceScaleFactor: scale, reducedMotion: 'reduce', locale: 'ko-KR', viewport: { width: 1000, height: 850 } });
    for (const screen of ['home', 'selected', 'manual', 'failure']) {
      await page.goto(`http://127.0.0.1:1424/?screen=${screen}`);
      await page.locator('.app-window').waitFor();
      await page.addStyleTag({ content: '.web-preview { background: transparent !important; }' });
      const image = await page.locator('#app').screenshot({ omitBackground: true });
      if (screen === 'home') await sharp(image).png().toFile(new URL(`${theme}-${scale}-window.png`, evidence).pathname.replace(/^\/(\w:)/, '$1'));
      const { data, info } = await sharp(image).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
      // Inspect pixel areas wholly outside the rounded outline. Pixels crossing
      // the curve legitimately mix the stroke and fill through antialiasing.
      let checked = 0;
      for (let y = 0; y < info.height; y++) for (let x = 0; x < info.width; x++) {
        const px = (x + .5) / scale - 4, py = (y + .5) / scale - 4;
        const cx = px < 12 ? 12 : px > 363 ? 363 : null;
        const cy = py < 12 ? 12 : py > 488 ? 488 : null;
        if (cx === null || cy === null) continue;
        const nearX = Math.max(x / scale - 4, Math.min(cx, (x + 1) / scale - 4));
        const nearY = Math.max(y / scale - 4, Math.min(cy, (y + 1) / scale - 4));
        if (Math.hypot(nearX - cx, nearY - cy) <= 12.05) continue;
        const offset = (y * info.width + x) * 4;
        // Unpremultiplying a low-alpha PNG edge magnifies 8-bit rounding.
        const pigmentTolerance = 2 + Math.ceil(255 / Math.max(1, data[offset + 3]));
        assert(data[offset + 3] <= 20 || Math.abs(data[offset] - 56) <= pigmentTolerance, `${theme} ${scale} ${screen}: fill beyond the corner stroke at ${px},${py}: ${[...data.subarray(offset, offset + 4)]}`);
        checked++;
      }
      assert(checked > 4, 'Inspect actual antialiased border pixels');
      if (screen === 'manual') {
        const inputs = page.locator('.segments input');
        await inputs.first().fill('19');
        const focused = await inputs.first().evaluate(node => ({ active: document.activeElement === node, outline: getComputedStyle(node).outlineStyle, shadow: getComputedStyle(node).boxShadow, value: node.value }));
        assert(focused.active && focused.value === '19', 'Focus and text entry remain functional');
        assert.equal(focused.outline, 'none');
        await page.keyboard.press('Tab');
        assert(await inputs.nth(1).evaluate(node => document.activeElement === node), 'Keyboard focus navigation remains functional');
        assert.equal(await inputs.first().evaluate(node => getComputedStyle(node).boxShadow), focused.shadow, 'Focus does not add a colored inset border');
      }
      if (screen === 'home') {
        await sharp(image).extract({ left: 0, top: 0, width: Math.ceil(22 * scale), height: Math.ceil(22 * scale) }).resize(352, 352, { kernel: 'nearest' }).png()
          .toFile(new URL(`${theme}-${scale}-corner.png`, evidence).pathname.replace(/^\/(\w:)/, '$1'));
      }
    }
    await page.close();
  }
  console.log('Window corners: no fill beyond the border at 100/125/150/175/200% in both themes across home/selection/manual/result. Focus, typing and Tab work without a blue outline.');
} finally { await browser?.close(); await server.close(); }
