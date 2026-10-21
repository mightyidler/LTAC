import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { build, preview } from 'vite';
import sharp from 'sharp';

// Exercise the shipped CSS in Windows Chromium with simulated desktop commands.
// No discovery, TV control, window closing, or user configuration is involved.
const { chromium } = createRequire(import.meta.url)('playwright');
const bridge = `
export const desktop = true;
let listener;
let current = { configured:true, name:'OLED42C4ENA', stage:'ready', choices:[{id:'tv-2',name:'OLED42C4ENA',model:'OLED42C4ENA'}], devices:[{id:'tv-1',name:'OLED42C4ENA',ip:'192.0.2.1',mac:null}], automatic:true, shortcut_enabled:false, shortcut:null, busy:false, error:null };
if (new URLSearchParams(location.search).has('empty')) current = {...current,configured:false,devices:[]};
export const appUpdateState = async () => ({version:'0.1.2',completed:false});
export const checkAppUpdate = async () => ({status:'latest',version:'0.1.2'});
export const windowAction = async () => {};
export const connect = async (callback, onOpen) => {
  listener = callback; callback(current);
  window.reopenLTAC = () => { current = {...current,stage:'ready',busy:false}; listener(current); onOpen(current); };
};
export async function command(name, args = {}) {
  if (name === 'discover_tvs') current = {...current, stage:'choose'};
  if (name === 'cancel_connection') current = {...current, stage:current.configured || args.home ? 'ready' : 'choose'};
  listener(current); return current;
}
`;
const config = {
  configFile: fileURLToPath(new URL('../vite.config.ts', import.meta.url)),
  preview: { port: 1422, strictPort: true },
  build: { outDir: fileURLToPath(new URL('../local/icon-production-test', import.meta.url)), emptyOutDir: true },
  plugins: [{
    name: 'simulated-icon-actions', enforce: 'pre',
    load(id) { if (id.replaceAll('\\', '/').split('?')[0].endsWith('/ui/src/bridge.ts')) return bridge; },
  }],
};
let browser, server;
try {
  await build(config);
  server = await preview(config);
  browser = await chromium.launch({ channel: 'msedge', headless: true });
  for (const deviceScaleFactor of (process.env.LTAC_ICON_SCALE ? [Number(process.env.LTAC_ICON_SCALE)] : [1, 1.25, 1.5, 1.75, 2])) {
    const page = await browser.newPage({ viewport: { width: 520, height: 650 }, deviceScaleFactor, colorScheme:'dark' });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto('http://127.0.0.1:1422');
    await page.waitForSelector('.saved-device');
    await page.evaluate(() => new Promise(resolve => setTimeout(resolve, 500)));
    async function insetEdges(selectors) {
      const edges = await page.evaluate(selectors => selectors.flatMap(selector => Array.from(document.querySelectorAll(selector), node => {
        const style = getComputedStyle(node);
        return { selector, borders:[style.borderTopWidth, style.borderRightWidth, style.borderBottomWidth, style.borderLeftWidth], shadow:style.boxShadow, outline:style.outlineStyle };
      })), selectors);
      for (const edge of edges) {
        assert.deepEqual(edge.borders, ['0px', '0px', '0px', '0px'], `${edge.selector}: no layout border`);
        assert.match(edge.shadow, /0px 0px 0px 1px inset/, `${edge.selector}: 1px inner edge`);
        assert.equal(edge.outline, 'none');
      }
    }
    await insetEdges(['.shortcut-button', '.device-menu:not(.hdmi-menu)', '.app-toast']);

    async function press(selector, surface = selector, raster = false) {
      const button = page.locator(selector).first();
      const bounds = await button.boundingBox();
      await page.mouse.move(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2);
      const clip = { x:Math.floor(bounds.x) - 2, y:Math.floor(bounds.y) - 2, width:Math.ceil(bounds.width) + 4, height:Math.ceil(bounds.height) + 4 };
      // DOM rectangles miss Chromium's fractional-DPI raster snapping: the
      // painted glyph can jump a physical pixel without any layout movement.
      const inkCenter = async () => {
        const {data, info} = await sharp(await page.screenshot({clip})).ensureAlpha().raw().toBuffer({resolveWithObject:true});
        let mass = 0, xsum = 0, ysum = 0;
        for (let y = 0; y < info.height; y++) for (let x = 0; x < info.width; x++) {
          const weight = Math.max(0, data[(y * info.width + x) * 4] - 75);
          mass += weight; xsum += (x + .5) * weight; ysum += (y + .5) * weight;
        }
        assert.ok(mass > 0, `${selector}: visible glyph pixels`);
        return { x:xsum / mass, y:ysum / mass };
      };
      const paintedBefore = raster ? await inkCenter() : null;
      // Record every painted frame, including the press and release transitions.
      await page.evaluate(({ selector, surface }) => {
        const button = document.querySelector(selector), svg = button.querySelector('svg');
        const shell = document.querySelector(surface);
        window.iconTrace = [];
        window.traceIcons = true;
        const tick = () => {
          if (!window.traceIcons) return;
          const b = button.getBoundingClientRect(), s = svg.getBoundingClientRect(), box = shell.getBoundingClientRect();
          const geometry = svg.querySelector('path').getBBox();
          const ink = new DOMPoint(geometry.x + geometry.width / 2, geometry.y + geometry.height / 2).matrixTransform(svg.getScreenCTM());
          window.iconTrace.push({ dx:s.x + s.width / 2 - b.x - b.width / 2, dy:s.y + s.height / 2 - b.y - b.height / 2, inkX:ink.x - b.x - b.width / 2, inkY:ink.y - b.y - b.height / 2, x:s.x + s.width / 2, y:s.y + s.height / 2, glyphWidth:s.width, buttonWidth:b.width, buttonHeight:b.height, width:box.width, height:box.height });
          requestAnimationFrame(tick);
        };
        tick();
      }, { selector, surface });
      await page.mouse.down();
      await page.waitForTimeout(200);
      const held = await page.evaluate(() => window.iconTrace.at(-1));
      const paintedHeld = raster ? await inkCenter() : null;
      return { held, release: async (cancel = false) => {
        if (cancel) await page.mouse.move(450, 550);
        await page.mouse.up();
        await page.waitForTimeout(400);
        if (raster) {
          const paintedAfter = await inkCenter();
          for (const painted of [paintedHeld, paintedAfter]) for (const axis of ['x', 'y']) assert.ok(Math.abs(painted[axis] - paintedBefore[axis]) < .2, `${deviceScaleFactor}× ${selector}: painted ${axis} jumped ${Math.abs(painted[axis] - paintedBefore[axis]).toFixed(3)} physical pixels`);
        }
        return page.evaluate(() => { window.traceIcons = false; return window.iconTrace; });
      } };
    }
    function centered(trace, label, ink = true) {
      const keys = ink ? ['dx', 'dy', 'inkX', 'inkY'] : ['dx', 'dy'];
      for (const key of keys) assert.ok(Math.max(...trace.map(frame => Math.abs(frame[key]))) < .05, `${label}: ${key} drift ${Math.max(...trace.map(frame => Math.abs(frame[key])))}`);
    }
    function scalesTogether(trace, label, fixedCenter = true) {
      const first = trace[0];
      assert.ok(Math.min(...trace.map(frame => frame.glyphWidth)) < first.glyphWidth * .99, `${label}: the glyph must shrink with its button`);
      for (const frame of trace) {
        assert.ok(Math.abs(frame.glyphWidth / first.glyphWidth - frame.buttonWidth / first.buttonWidth) < .005, `${label}: glyph and button must share the same scale`);
        assert.ok(Math.abs(frame.dx / frame.buttonWidth - first.dx / first.buttonWidth) < .001, `${label}: horizontal alignment stays proportional`);
        assert.ok(Math.abs(frame.dy / frame.buttonHeight - first.dy / first.buttonHeight) < .001, `${label}: vertical alignment stays proportional`);
        if (fixedCenter) for (const key of ['x', 'y']) assert.ok(Math.abs(frame[key] - first[key]) < .05, `${label}: ${key} center stays fixed`);
      }
    }

    for (const selector of ['#navigation', '#minimize', '#close']) {
      const action = await press(selector, selector, true);
      const trace = await action.release(true);
      centered(trace, selector); scalesTogether(trace, selector);
    }
    const more = await press('.more-button', '.device-menu:not(.hdmi-menu)');
    centered(await more.release(true), 'more press / cancelled release');
    assert.ok(more.held.width < 23, 'The whole More surface must shrink while held');
    assert.equal(await page.locator('.device-menu:not(.hdmi-menu)').getAttribute('data-open'), 'false');
    const open = await press('.more-button', '.device-menu:not(.hdmi-menu)');
    await open.release();
    assert.equal(await page.locator('.device-menu:not(.hdmi-menu)').getAttribute('data-open'), 'true');
    assert.equal(await page.locator('.more-button').getAttribute('aria-expanded'), 'true');
    assert.equal(await page.locator('.device-menu:not(.hdmi-menu) [role=menu]').evaluate(node => node.inert), false);
    const backgrounds = () => page.locator('.device-menu:not(.hdmi-menu) [role=menuitem]').evaluateAll(items => items.map(item => getComputedStyle(item).backgroundColor));
    const settledBackgrounds = async expected => {
      const deadline = Date.now() + 5000;
      while (Date.now() < deadline) { if (JSON.stringify(await backgrounds()) === JSON.stringify(expected)) return; await page.waitForTimeout(50); }
      assert.deepEqual(await backgrounds(), expected);
    };
    await page.mouse.move(1, 1); await page.waitForTimeout(200);
    await settledBackgrounds(['rgba(0, 0, 0, 0)', 'rgba(0, 0, 0, 0)']);
    assert.deepEqual(await backgrounds(), ['rgba(0, 0, 0, 0)', 'rgba(0, 0, 0, 0)'], 'Neither menu action is highlighted by default');
    for (const [index, action] of ['edit', 'remove'].entries()) {
      await page.locator(`.device-menu:not(.hdmi-menu) [data-action="${action}"]`).hover(); await page.waitForTimeout(200);
      await settledBackgrounds(index === 0 ? ['rgba(255, 255, 255, 0.04)', 'rgba(0, 0, 0, 0)'] : ['rgba(0, 0, 0, 0)', 'rgba(255, 255, 255, 0.04)']);
      const colors = await backgrounds();
      assert.equal(colors[index], 'rgba(255, 255, 255, 0.04)', 'Only the hovered action is highlighted');
      assert.equal(colors[1 - index], 'rgba(0, 0, 0, 0)');
      await page.mouse.move(1, 1); await page.waitForTimeout(200);
      await settledBackgrounds(['rgba(0, 0, 0, 0)', 'rgba(0, 0, 0, 0)']);
      assert.deepEqual(await backgrounds(), ['rgba(0, 0, 0, 0)', 'rgba(0, 0, 0, 0)'], 'Leaving the action clears its highlight');
    }
    const expanded = await page.locator('.device-menu:not(.hdmi-menu)').boundingBox();
    assert.ok(Math.abs(expanded.width - 140) < .05 && Math.abs(expanded.height - 84) < .05);
    const menuStyle = await page.locator('.device-menu:not(.hdmi-menu)').evaluate(node => {
      const panel = getComputedStyle(node), content = getComputedStyle(node.querySelector('.t-morph-menu'));
      return { radius:panel.borderRadius, blur:panel.backdropFilter, padding:content.padding, rowGap:content.gap, rows:[...node.querySelectorAll('[role=menuitem]')].map(item => {
        const style = getComputedStyle(item), icon = item.querySelector('svg').getBoundingClientRect(), text = item.querySelector('span').getBoundingClientRect(), box = item.getBoundingClientRect();
        return { radius:style.borderRadius, weight:style.fontWeight, gap:style.gap, paddingY:style.paddingTop, height:box.height, iconInset:icon.x - box.x, textGap:text.x - icon.right };
      }) };
    });
    assert.equal(menuStyle.radius, '24px'); assert.equal(menuStyle.blur, 'blur(24px)'); assert.equal(menuStyle.padding, '8px'); assert.equal(menuStyle.rowGap, '4px');
    for (const row of menuStyle.rows) { assert.equal(row.radius, '16px'); assert.equal(row.weight, '400'); assert.equal(row.gap, '8px'); assert.equal(row.paddingY, '8px'); assert.equal(row.height, 32); assert.ok(Math.abs(row.iconInset - 8) < .05); assert.ok(Math.abs(row.textGap - 8) < .05); }
    const itemPress = await press('.device-menu:not(.hdmi-menu) [data-action="edit"]');
    assert.equal((await backgrounds())[0], 'rgba(255, 255, 255, 0.04)', 'Pressed action is highlighted');
    scalesTogether(await page.evaluate(() => window.iconTrace), 'menu item', false);
    await itemPress.release(true);
    // The cancelled release outside also dismisses the menu; reopen for capture.
    await page.locator('.more-button').click();
    await page.waitForTimeout(400);
    await page.mouse.move(1, 1); await page.waitForTimeout(200);
    if (deviceScaleFactor === 1) await page.locator('#app').screenshot({ path:fileURLToPath(new URL('../local/icons-menu-after.png', import.meta.url)) });
    await page.keyboard.press('Escape');
    await page.waitForTimeout(400);
    centered(await (await press('.more-button', '.device-menu:not(.hdmi-menu)')).release(true), 'more after close');

    await page.locator('#navigation').click();
    await page.waitForSelector('#refresh');
    await page.waitForTimeout(500);
    const backTrace = await (await press('#navigation')).release(true);
    centered(backTrace, 'back'); scalesTogether(backTrace, 'back');
    // The refresh glyph is asymmetric; its SVG viewport must stay centered.
    const refreshTrace = await (await press('#refresh')).release(true);
    centered(refreshTrace, 'refresh', false); scalesTogether(refreshTrace, 'refresh');
    scalesTogether(await (await press('.device-choice')).release(true), 'device row', false);
    await page.locator('#manual').click();
    await page.waitForSelector('.segments input');
    await page.waitForTimeout(400);
    await insetEdges(['.segments input']);
    const input = page.locator('.segments input').first();
    assert.equal((await input.boundingBox()).height, 34, 'Input height is preserved');
    await input.fill('192');
    await input.focus();
    await page.waitForTimeout(200);
    await insetEdges(['.segments input:focus']);
    if (deviceScaleFactor === 1) await page.locator('#app').screenshot({ path:fileURLToPath(new URL('../local/icons-inputs-after.png', import.meta.url)) });
    await page.evaluate(() => window.reopenLTAC());
    await page.waitForSelector('#app[data-view="home"]');
    assert.equal(await page.locator('.t-page:not([inert])').evaluate(node => node.getAnimations().length), 0, 'Reopening home does not animate the previous page into view');
    assert.equal(await page.locator('.t-page:not([inert]) #manual-form').count(), 0, 'Reopening clears the manual form');
    await page.locator('#navigation').click();
    await page.waitForSelector('#refresh');
    await page.evaluate(() => window.reopenLTAC());
    await page.waitForSelector('#app[data-view="home"]');
    assert.equal(await page.locator('#navigation').getAttribute('data-mode'), 'add');
    assert.deepEqual(errors, []);
    console.log(`${deviceScaleFactor}×: glyphs and buttons scale together around a fixed center; More press / release / reopen; 1px inset edges and input focus with preserved height.`);
    await page.close();
  }
  const reduced = await browser.newPage({ reducedMotion: 'reduce' });
  await reduced.goto('http://127.0.0.1:1422');
  await reduced.locator('.more-button').click();
  assert.equal(await reduced.locator('.device-menu:not(.hdmi-menu)').getAttribute('data-open'), 'true');
  await reduced.keyboard.press('Escape');
  assert.equal(await reduced.locator('.device-menu:not(.hdmi-menu)').getAttribute('data-open'), 'false');
  console.log('Reduced motion: More opens and closes successfully.');
  const empty = await browser.newPage();
  await empty.goto('http://127.0.0.1:1422/?empty');
  await empty.waitForSelector('#app[data-view="home"]');
  await empty.locator('#navigation').click();
  await empty.waitForSelector('#refresh');
  await empty.waitForTimeout(400);
  await empty.locator('#navigation').click();
  await empty.waitForSelector('#app[data-view="home"]');
  console.log('Returning empty home: add-device flow returns home; reopening resets discovery and manual forms.');
} finally {
  await browser?.close();
  await new Promise(resolve => server ? server.httpServer.close(resolve) : resolve());
}
