import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { build, preview } from 'vite';

// Browser regression test; Playwright can be supplied through NODE_PATH.
// Test the MINIFIED production app/CSS, never Vite's uncompressed dev styles.
// The renderer runs against a simulated desktop event stream (no TV traffic).
// Korean text assertions require an explicit locale on every test page.
const { chromium } = createRequire(import.meta.url)('playwright');
const bridge = `
export const desktop = true;
let listener;
let attempts = 0;
let current = { configured:false, name:'OLED42C4ENA', stage:'choose', choices:[{id:'tv-1',name:'OLED42C4ENA',model:'OLED42C4ENA'}], devices:[], automatic:true, shortcut_enabled:false, shortcut:null, busy:false, error:null };
export const appUpdateState = async () => ({version:'0.1.1',completed:false});
export const checkAppUpdate = async () => ({status:'latest',version:'0.1.1'});
export const windowAction = async () => {};
export const connect = async callback => { listener = callback; callback(current); };
export async function command(name) {
  if (name === 'cancel_connection') { current = {...current, stage:'choose', busy:false}; listener(current); return current; }
  if (name !== 'pair_tv') return current;
  current = {...current,stage:'pairing',busy:true}; listener(current);
  attempts++;
  const fail = attempts === 1 && new URLSearchParams(location.search).has('failure');
  await new Promise(resolve => setTimeout(resolve, 100));
  current = {...current,stage:fail?'pair_error':'ready',configured:!fail,busy:false,devices:fail?[]:[{id:'tv-1',name:'OLED42C4ENA',ip:'192.0.2.1',mac:null}]};
  // Tauri broadcasts terminal state before invoke resolves, and service updates
  // continue during the ribbon, check drawing and confirmation hold.
  listener(current);
  setTimeout(() => { current = {...current,automatic:false}; listener(current); }, 500);
  const timer = setInterval(() => listener({...current}), 30);
  setTimeout(() => clearInterval(timer), 10000);
  await new Promise(resolve => setTimeout(resolve, 50));
  return current;
}
`;
const config = {
  configFile: fileURLToPath(new URL('../vite.config.ts', import.meta.url)),
  preview: { port: 1421, strictPort: true },
  build: { outDir: fileURLToPath(new URL('../local/connection-production-test', import.meta.url)), emptyOutDir: true, minify: true, cssMinify: true },
  plugins: [{
    name: 'simulated-desktop-events', enforce: 'pre',
    load(id) {
      const path = id.replaceAll('\\', '/').split('?')[0];
      if (path.endsWith('/ui/src/bridge.ts')) return bridge;
      if (path.endsWith('/ui/src/main.ts') && process.env.LTAC_PAIRING_BASELINE) return readFileSync(process.env.LTAC_PAIRING_BASELINE, 'utf8');
    },
  }],
};
const rendererChunk = '**/assets/connection-renderer-*.js';
let browser, server;
try {
  await build(config);
  server = await preview(config);
  browser = await chromium.launch({ channel: 'msedge', headless: true });
  for (const reducedMotion of ['no-preference', 'reduce']) {
    const page = await browser.newPage({ locale: 'ko-KR', reducedMotion, viewport: { width: 520, height: 650 } });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto('http://127.0.0.1:1421');
    await page.evaluate(() => {
      window.connectionTrace = [];
      new MutationObserver(() => {
        const app = document.querySelector('#app');
        const indicator = document.querySelector('#connection-indicator');
        const home = document.querySelector('.t-page[data-view="home"]');
        const phase = app?.dataset.view === 'home' ? home?.inert ? 'home' : 'home-visible' : indicator?.dataset.state;
        if (phase && window.connectionTrace.at(-1)?.phase !== phase) window.connectionTrace.push({phase,time:performance.now()});
      }).observe(document.querySelector('#app'), {subtree:true,attributes:true,childList:true});
    });
    await page.getByRole('option', {name:'OLED42C4ENA'}).click();
    await page.getByRole('button', {name:'연결',exact:true}).click();
    if (reducedMotion === 'no-preference') {
      await page.waitForSelector('#connection-indicator[data-state="completing"]');
      assert.equal(await page.locator('#connection-indicator').evaluate(node => getComputedStyle(node).getPropertyValue('--indicator-complete-dur').trim()), '4s');
    }
    await page.waitForSelector('#connection-indicator[data-state="success"]');
    assert.equal(await page.locator('#app').getAttribute('data-view'), 'connection');
    // Let the result title's resize finish before comparing screen pixels.
    await page.waitForTimeout(400);
    const ribbonBefore = await page.locator('#connection-indicator canvas').screenshot();
    await page.waitForTimeout(45);
    const ribbonAfter = await page.locator('#connection-indicator canvas').screenshot();
    assert(ribbonAfter.equals(ribbonBefore), 'The completed O must remain still');
    await page.waitForFunction(() => document.querySelector('#connection-title')?.textContent === '연결 완료');
    await page.waitForSelector('#app[data-view="home"]', {timeout:10000});
    await page.waitForFunction(() => document.querySelector('.t-page[data-view="home"]')?.inert === false);
    const trace = await page.evaluate(() => window.connectionTrace);
    const completion = trace.find(event => event.phase === 'success').time - trace.find(event => event.phase === 'completing').time;
    const homeStart = trace.find(event => event.phase === 'home') ?? trace.find(event => event.phase === 'home-visible');
    const visible = homeStart.time - trace.find(event => event.phase === 'success').time;
    assert.ok(visible >= (reducedMotion === 'reduce' ? 850 : 1400), `${reducedMotion}: completion dismissed after only ${visible}ms`);
    if (reducedMotion === 'no-preference') {
      assert.ok(completion >= 3990, `4s production completion shortened to ${completion}ms`);
      const fade = trace.find(event => event.phase === 'home-visible').time - homeStart.time;
      assert.ok(fade >= 240, `250ms production exit shortened to ${fade}ms`);
    }
    assert.equal(await page.getByRole('switch', {name:'전원 동기화'}).getAttribute('aria-checked'), 'false', 'home must receive the latest deferred service snapshot');
    assert.deepEqual(errors, []);
    console.log(`${reducedMotion}: production completion ${Math.round(completion)}ms, check/hold ${Math.round(visible)}ms; latest service settings preserved.`);
    await page.close();
  }
  const failure = await browser.newPage({ locale: 'ko-KR' });
  await failure.route(rendererChunk, route => route.abort());
  await failure.goto('http://127.0.0.1:1421/?failure');
  await failure.getByRole('option', {name:'OLED42C4ENA'}).click();
  await failure.getByRole('button', {name:'연결',exact:true}).click();
  await failure.waitForSelector('#connection-indicator[data-state="failure"]');
  const retry = failure.getByRole('button', {name:'다시 시도'});
  await retry.waitFor({state:'visible'});
  assert.equal(await failure.locator('.indicator-ring').evaluate(node => getComputedStyle(node).opacity), '1');
  assert.equal(await failure.locator('.connection-indicator canvas').count(), 0);
  assert.equal(await failure.locator('#app').getAttribute('data-view'), 'connection');
  await retry.click();
  await failure.waitForSelector('#app[data-view="home"]', {timeout:10000});
  console.log('The fallback exposes a working retry even if the renderer cannot load.');
  await failure.close();
  // A cancellation must invalidate completion even if the renderer is still loading.
  const page = await browser.newPage({ locale: 'ko-KR' });
  let releaseRenderer;
  const rendererCanLoad = new Promise(resolve => { releaseRenderer = resolve; });
  await page.route(rendererChunk, async route => {
    await rendererCanLoad;
    await route.continue();
  });
  await page.goto('http://127.0.0.1:1421');
  await page.getByRole('option', {name:'OLED42C4ENA'}).click();
  const rendererRequested = page.waitForRequest(rendererChunk);
  await page.getByRole('button', {name:'연결',exact:true}).click();
  await rendererRequested;
  await page.waitForSelector('#connection-indicator[data-state="completing"]');
  assert.equal(await page.locator('#connection-indicator .indicator-fallback').isVisible(), true);
  await page.getByRole('button', {name:'뒤로'}).click();
  await page.waitForSelector('#app[data-view="choose"]');
  const rendererResponse = page.waitForResponse(rendererChunk);
  releaseRenderer();
  const response = await rendererResponse;
  await page.evaluate(url => import(url).then(() => {}), response.url());
  assert.equal(await page.locator('.connection-indicator canvas').count(), 0);
  assert.equal(await page.locator('#app').getAttribute('data-view'), 'choose');
  console.log('Cancelled pairing cannot attach a late renderer or complete a stale animation.');
} finally {
  await browser?.close();
  if (server) await new Promise(resolve => server.httpServer.close(resolve));
}
