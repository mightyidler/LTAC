import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdirSync } from 'node:fs';
import { createServer } from 'vite';
import sharp from 'sharp';

const { chromium } = createRequire(import.meta.url)('playwright');
const evidence = new URL('../local/window-surface/', import.meta.url);
mkdirSync(evidence, { recursive: true });
const server = await createServer({ server: { port: 1428, strictPort: true } });
let browser;
try {
  await server.listen();
  browser = await chromium.launch({ channel: 'msedge', headless: true });
  for (const theme of ['light', 'dark']) for (const scale of [1, 1.25, 1.5, 1.75, 2]) {
    const page = await browser.newPage({ colorScheme: theme, deviceScaleFactor: scale, reducedMotion: 'reduce', locale: 'ko-KR', viewport: { width: 1000, height: 850 } });
    for (const screen of ['home', 'selected', 'manual', 'failure']) {
      await page.goto(`http://127.0.0.1:1428/?screen=${screen}`);
      await page.locator('.app-window').waitFor();
      await page.addStyleTag({ content: '.web-preview { background: transparent !important; justify-content: flex-start; }' });
      const image = await page.locator('#app').screenshot({ omitBackground: true });
      if (screen === 'home') await sharp(image).png().toFile(new URL(`${theme}-${scale}-window.png`, evidence).pathname.replace(/^\/(\w:)/, '$1'));
      const { data, info } = await sharp(image).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
      // The webview is an opaque rectangle. Only DWM rounds the actual HWND.
      assert.equal(info.width, Math.round(375 * scale));
      assert.equal(info.height, Math.round(500 * scale));
      const geometry = await page.locator('#app').evaluate(app => {
        const frame = app.querySelector('.app-window'), surface=app.querySelector('.app-surface');
        const box = node => {const r=node.getBoundingClientRect();return {x:r.x,y:r.y,width:r.width,height:r.height};};
        const css=getComputedStyle(frame),padding=getComputedStyle(app).padding;
        return {app:box(app),frame:box(frame),surface:box(surface),padding,border:css.borderWidth,radius:css.borderRadius,shadow:css.boxShadow};
      });
      assert.equal(geometry.padding,'0px');assert.equal(geometry.border,'0px');
      assert.equal(geometry.radius,'0px');assert.equal(geometry.shadow,'none');
      assert.deepEqual(geometry.frame,geometry.app);assert.deepEqual(geometry.surface,geometry.app);
      for(const [x,y] of [[1,1],[info.width-2,1],[1,info.height-2],[info.width-2,info.height-2]]) {
        assert.equal(data[(y*info.width+x)*4+3],255,`${theme} ${scale} ${screen}: no transparent fake gutter or CSS-rounded corner`);
      }
      if(await page.locator('.page-scroll').count()) {
      const track=await page.locator('.page-scroll').first().evaluate(node=>({margin:getComputedStyle(node).marginRight,top:getComputedStyle(node,'::-webkit-scrollbar-track').marginTop,bottom:getComputedStyle(node,'::-webkit-scrollbar-track').marginBottom}));
      assert.equal(track.margin,'3px');assert.equal(track.bottom,'5px');
      assert.equal(track.top,screen==='home'||screen==='selected'?'52px':'56px','Preserve existing scrollbar top placement');
      }
      if (screen === 'manual') {
        const inputs = page.locator('.segments input');
        await inputs.first().fill('19');
        const focused = await inputs.first().evaluate(node => ({ active: document.activeElement === node, outline: getComputedStyle(node).outlineStyle, shadow: getComputedStyle(node).boxShadow, value: node.value }));
        assert(focused.active && focused.value === '19', 'Focus and text entry remain functional');
        assert.equal(focused.outline, 'none');
        await page.keyboard.press('Tab');
        assert(await inputs.nth(1).evaluate(node => document.activeElement === node), 'Keyboard focus navigation remains functional');
        assert.match(focused.shadow, /0px 0px 0px 1px inset$/, 'Existing focus edge remains inside the input');
      }
      if (screen === 'home') {
        await sharp(image).extract({ left: 0, top: 0, width: Math.ceil(22 * scale), height: Math.ceil(22 * scale) }).resize(352, 352, { kernel: 'nearest' }).png()
          .toFile(new URL(`${theme}-${scale}-corner.png`, evidence).pathname.replace(/^\/(\w:)/, '$1'));
      }
    }
    await page.close();
  }
  console.log('Native frame web surface: opaque 375×500 without CSS frame/gutter at 100/125/150/175/200% across both themes and home/selection/manual/result; preserved scroll top, 3px right/5px bottom, focus/typing/Tab passed.');
} finally { await browser?.close(); await server.close(); }
