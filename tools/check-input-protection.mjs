import { demoDevices } from '../ui/dev/fixtures.ts';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { build, preview } from 'vite';

const { chromium } = createRequire(import.meta.url)('playwright');
const evidence = new URL('../local/test-results/input-protection/', import.meta.url);
mkdirSync(evidence, { recursive: true });
// Exercise production UI against a persistent simulated desktop, without TV I/O.
const bridge = `
import { initialState } from './model';
const demoDevices = ${JSON.stringify(demoDevices)};
export const desktop = true;
let state = JSON.parse(localStorage.getItem('test.state') || 'null') || { ...initialState, configured:true, stage:'ready', devices:demoDevices };
export const appUpdateState = async () => ({version:'0.1.4',completed:false});
export const checkAppUpdate = async () => ({status:'latest',version:'0.1.4'});
export const windowAction = async () => {};
export const connect = async callback => callback(state);
export async function command(name, args) {
  if (name === 'set_keep_tv_on') state.keep_tv_on = args.enabled;
  if (name === 'set_automatic') state.automatic = args.enabled;
  if (name === 'set_shortcut') state.shortcut_enabled = args.enabled;
  if (name === 'edit_device') state.devices = state.devices.map(device => device.id === args.id ? { ...device, ip:args.ip, mac:args.mac } : device);
  localStorage.setItem('test.state', JSON.stringify(state));
  return { ...state };
}
`;
const config = {
  configFile: fileURLToPath(new URL('../vite.config.ts', import.meta.url)),
  preview: { port:1423, strictPort:true },
  build: { outDir:fileURLToPath(new URL('../local/input-protection-test', import.meta.url)), emptyOutDir:true, minify:true, cssMinify:true },
  plugins: [{ name:'input-policy-desktop', enforce:'pre', load(id) {
    if (id.replaceAll('\\','/').split('?')[0].endsWith('/ui/src/bridge.ts')) return bridge;
  } }],
};
let browser, server;
try {
  await build(config); server = await preview(config);
  browser = await chromium.launch({ channel:'msedge', headless:true });
  for (const locale of ['ko-KR','en-US','ja-JP']) for (const colorScheme of ['dark','light']) {
    const context = await browser.newContext({ locale, colorScheme, reducedMotion:'reduce', viewport:{width:375,height:760} });
    const page = await context.newPage(); const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto('http://127.0.0.1:1423');
    const toggle = page.locator('#keep-tv-on');
    await toggle.waitFor();
    assert.equal(await toggle.getAttribute('aria-checked'), 'false');
    const bounds = await Promise.all(['#automatic','#keep-tv-on','#shortcut-enabled'].map(selector => page.locator(selector).boundingBox()));
    assert(bounds[0].y < bounds[1].y && bounds[1].y < bounds[2].y, 'Protection belongs between sync and sleep shortcut');
    assert.equal(await page.locator('#keep-tv-on-hint, #pc-hdmi').count(), 0);
    assert.equal(await page.locator('#automatic').getAttribute('aria-checked'), 'true');
    assert.equal(await page.locator('#shortcut-enabled').getAttribute('aria-checked'), 'false');
    if (locale === 'ko-KR' && colorScheme === 'dark') await page.locator('#app').screenshot({path:fileURLToPath(new URL('home-default.png', evidence))});
    await toggle.click();
    assert.equal(await toggle.getAttribute('aria-checked'), 'true');
    assert.equal(await page.locator('#keep-tv-on-hint').count(), 0);
    await page.locator('.more-button').first().click();
    await page.locator('[data-action=edit]').first().click();
    assert.equal(await page.locator('#pc-hdmi').count(), 0, 'Device editing keeps its original inputs');
    await page.locator('#submit-manual').click();
    await toggle.waitFor();
    await page.reload(); await toggle.waitFor();
    assert.equal(await toggle.getAttribute('aria-checked'), 'true');
    await page.locator('.more-button').first().click();
    await page.locator('[data-action=edit]').first().click();
    assert.equal(await page.locator('#pc-hdmi').count(), 0);
    assert.deepEqual(errors, []);
    await context.close();
  }
  console.log('Input protection: only the requested toggle, sync ON, protection and shortcut OFF, placement, toggle save/reload, KO/EN/JA and light/dark passed.');
} finally { await browser?.close(); await server?.httpServer.close(); }
