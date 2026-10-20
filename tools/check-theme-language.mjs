import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdirSync, readFileSync } from 'node:fs';
import { createServer } from 'vite';
import sharp from 'sharp';
const { chromium } = createRequire(import.meta.url)('playwright');
const evidence = new URL('../local/test-results/theme-language/', import.meta.url);
mkdirSync(evidence, { recursive: true });
let server, browser;
const errors = [];
const typography = JSON.parse(readFileSync(new URL('../tests/fixtures/typography.json', import.meta.url), 'utf8'));
assert(typography.entries.every(entry => entry.weight === entry.originalWeight - 100), 'Apply exactly one correction to raw JSON weights');
async function checkTypography(page) {
  await page.waitForFunction(() => getComputedStyle(document.documentElement).fontWeight === (document.documentElement.dataset.theme === 'light' ? '400' : '300'));
  const mismatches = await page.evaluate(entries => entries.flatMap(entry => [...document.querySelectorAll(entry.selector)].flatMap(node => {
    if (node.closest('.t-page[inert]') || !node.getClientRects().length) return [];
    const actual = getComputedStyle(node).fontWeight;
    const expected = node.classList.contains('is-thinking') ? 500 : entry.weight + (document.documentElement.dataset.theme === 'light' ? 100 : 0);
    return actual === String(expected) ? [] : [{ selector: entry.selector, original: entry.originalWeight, expected, actual }];
  })), typography.entries);
  assert.deepEqual(mismatches, [], 'Rendered weights use JSON minus 100, with one +100 correction only in light mode');
}
try {
  server = await createServer({ server: { port: 1422, strictPort: true } });
  await server.listen();
  browser = await chromium.launch({ channel: 'msedge', headless: true });
  for (const locale of ['ko-KR', 'en-US', 'ja-JP', 'zh-CN']) {
    const context = await browser.newContext({ locale, colorScheme: 'dark', reducedMotion: 'reduce', viewport: { width: 1000, height: 850 } });
    const page = await context.newPage();
    page.on('pageerror', error => errors.push(error.message));
    await page.goto('http://127.0.0.1:1422/?screen=home');
    await page.locator('#language-setting').waitFor();
    const expected = locale.startsWith('ko') ? 'ko' : locale.startsWith('ja') ? 'ja' : 'en';
    assert.equal(await page.locator('html').getAttribute('lang'), expected);
    assert.equal(await page.locator('html').getAttribute('data-theme'), 'dark');
    assert.equal(await page.evaluate(() => localStorage.getItem('ltac.theme')), null);
    await page.locator('#theme-setting').click();
    assert.equal(await page.locator('[data-value=system]').getAttribute('aria-checked'), 'true');
    await page.emulateMedia({ colorScheme: 'light' });
    await page.waitForFunction(() => document.documentElement.dataset.theme === 'light');
    assert.equal(await page.locator('html').getAttribute('data-theme'), 'light');
    await page.locator('[data-value=dark]').click();
    await page.emulateMedia({ colorScheme: 'light' });
    assert.equal(await page.locator('html').getAttribute('data-theme'), 'dark');
    await page.reload();
    await page.locator('#theme-setting').waitFor();
    assert.equal(await page.locator('html').getAttribute('data-theme'), 'dark');
    await page.locator('#theme-setting').click();
    await page.locator('[data-value=system]').click();
    assert.equal(await page.locator('html').getAttribute('data-theme'), 'light');
    await page.emulateMedia({ colorScheme: 'dark' });
    await page.waitForFunction(() => document.documentElement.dataset.theme === 'dark');
    await page.reload();
    await page.locator('#theme-setting').waitFor();
    assert.equal(await page.evaluate(() => localStorage.getItem('ltac.theme')), 'system');
    // Test every shipped locale and both palettes across discovery, inputs, results, menus and settings.
    if (!locale.startsWith('zh')) for (const theme of ['dark', 'light']) {
      await page.locator('#theme-setting').click();
      await page.locator(`[data-value=${theme}]`).click();
      await checkTypography(page);
      const backgroundAlpha = async selector => page.locator(selector).evaluate(node => {
        const canvas = document.createElement('canvas'); canvas.width = canvas.height = 1;
        const ctx = canvas.getContext('2d'); ctx.fillStyle = getComputedStyle(node).backgroundColor; ctx.fillRect(0,0,1,1);
        return ctx.getImageData(0,0,1,1).data[3];
      });
      await page.locator('#minimize').hover();
      await page.waitForTimeout(200);
      assert.equal(await backgroundAlpha('#minimize'), theme === 'light' ? 15 : 31, 'General hover uses 6% light / 12% dark');
      await page.mouse.down();
      await page.waitForTimeout(200);
      assert.equal(await backgroundAlpha('#minimize'), theme === 'light' ? 20 : 41, 'Pressed controls use 8% light / 16% dark');
      await page.mouse.up();
      await page.locator('#app').screenshot({ path: new URL(`${expected}-${theme}-theme.png`, evidence).pathname.replace(/^\/(\w:)/, '$1') });
      await page.locator('#navigation').click();
      const headings = await page.locator('.t-page:not([inert]) h2').allTextContents();
      assert.deepEqual(headings, expected === 'ko' ? ['기기 목록','전원','일반'] : expected === 'ja' ? ['デバイス一覧','電源','一般'] : ['Devices','Power','General']);
      await checkTypography(page);
      await page.locator('#shortcut').click();
      await checkTypography(page);
      await page.keyboard.press('Escape');
      assert.deepEqual(await page.locator('.t-page:not([inert]) h2').evaluateAll(nodes => nodes.map(node => [getComputedStyle(node).paddingTop, getComputedStyle(node).paddingBottom])), [['0px','0px'],['0px','0px'],['0px','0px']]);
      const trigger = page.locator('#language-setting');
      assert(await trigger.evaluate(node => node.offsetWidth < node.parentElement.offsetWidth / 2), 'Only value and chevron form the settings trigger');
      await page.locator('#language-label').click();
      assert.equal(await page.locator('#app').getAttribute('data-view'), 'home', 'Clicking the left label must not navigate');
      await page.locator('#language-setting').click();
      assert.equal(await page.locator('[role=radio]').count(), 3);
      await checkTypography(page);
      assert.equal(await page.locator('.preference-page h1').count(), 0, 'Preference title belongs in the window header');
      assert(await page.locator('#compact-title').evaluate(node => node.classList.contains('visible') && !!node.closest('header')));
      assert.deepEqual(await page.locator('.preference-option').first().evaluate(node => { const css = getComputedStyle(node); return [css.paddingTop, css.paddingRight, css.paddingBottom, css.paddingLeft]; }), ['12px','8px','12px','8px']);
      await page.locator('.preference-option').first().hover();
      await page.waitForTimeout(200);
      const hoverAlpha = await page.locator('.preference-option').first().evaluate(node => {
        const canvas = document.createElement('canvas'); canvas.width = canvas.height = 1;
        const ctx = canvas.getContext('2d'); ctx.fillStyle = getComputedStyle(node).backgroundColor; ctx.fillRect(0,0,1,1);
        return ctx.getImageData(0,0,1,1).data[3];
      });
      assert.equal(hoverAlpha, theme === 'light' ? 10 : 8, 'List hover uses 4% light / 3% dark rather than generic button hover');
      const red = await page.locator('html').evaluate(node => getComputedStyle(node).getPropertyValue('--status-error').trim());
      assert.equal(red, theme === 'light' ? '#ff383c' : '#ff4245');
      await page.locator('#app').screenshot({ path: new URL(`${expected}-${theme}-language.png`, evidence).pathname.replace(/^\/(\w:)/, '$1') });
      await page.locator('#navigation').click();
      const toggle = await page.locator('#automatic').evaluate(node => getComputedStyle(node).backgroundColor);
      assert.equal(toggle, theme === 'dark' ? 'rgb(49, 209, 88)' : 'rgb(52, 199, 89)');
      const tokenAudit = await page.evaluate(() => {
        const root = getComputedStyle(document.documentElement);
        const names = [...document.styleSheets].flatMap(sheet => {
          try { return [...sheet.cssRules].flatMap(rule => [...rule.cssText.matchAll(/var\((--[\w-]+)/g)].map(m => m[1])); } catch { return []; }
        });
        return [...new Set(names)].filter(name => !root.getPropertyValue(name).trim() && !['--shortcut-resize-delay'].includes(name));
      });
      // Some motion aliases intentionally have per-component fallbacks, checked separately by existing motion tests.
      assert(!tokenAudit.some(name => /^(--p-|--bg-|--text-|--border-|--interaction-|--status-|--color-)/.test(name)), tokenAudit.join(', '));
      await page.locator('#app').screenshot({ path: new URL(`${expected}-${theme}-home.png`, evidence).pathname.replace(/^\/(\w:)/, '$1') });
      await page.locator('.home-scroll').evaluate(node => node.scrollTop = node.scrollHeight);
      await page.locator('#app').screenshot({ path: new URL(`${expected}-${theme}-home-general.png`, evidence).pathname.replace(/^\/(\w:)/, '$1') });
      for (const screen of ['choose', 'selected', 'empty', 'manual', 'manual-filled', 'pair', 'success', 'failure', 'home-empty', 'error']) {
        await page.selectOption('#preview-screen', screen);
        await page.waitForTimeout(70);
        if (screen === 'success') await page.locator('.connection-indicator[data-state=success]').waitFor();
        await checkTypography(page);
        const overflow = await page.locator('.t-page:not([inert])').evaluate(root => [...root.querySelectorAll('h1,h2,p,small,button,input')].filter(node => {
          if (!node.getClientRects().length || node.closest('[inert], [aria-hidden="true"]')) return false;
          return node.scrollWidth > node.clientWidth + 2;
        }).map(node => `${node.tagName}: ${node.textContent}`));
        assert.deepEqual(overflow, [], `${expected}/${theme}/${screen} clips content`);
        const lowContrast = await page.locator('.t-page:not([inert])').evaluate(root => {
          const canvas = document.createElement('canvas'); canvas.width = canvas.height = 1;
          const ctx = canvas.getContext('2d', { willReadFrequently: true });
          const base = getComputedStyle(document.documentElement).getPropertyValue('--bg-canvas').trim();
          const pixel = () => [...ctx.getImageData(0, 0, 1, 1).data].slice(0, 3);
          const luminance = values => values.map(v => v / 255).map(v => v <= .04045 ? v / 12.92 : ((v + .055) / 1.055) ** 2.4).reduce((sum, v, i) => sum + v * [.2126,.7152,.0722][i], 0);
          return [...root.querySelectorAll('h1,h2,p,small,label,input,.preference-value,.choice-name,.shortcut-button')].flatMap(node => {
            const input = node instanceof HTMLInputElement;
            const style = input && node.matches(':focus:placeholder-shown') ? getComputedStyle(node.parentElement.querySelector('.empty-caret')) : getComputedStyle(node, input && !node.value ? '::placeholder' : null), rect = node.getBoundingClientRect(), bounds = root.getBoundingClientRect();
            const text = input ? node.value || node.placeholder : node.textContent;
            if (!text?.trim() || node.closest('[inert], [aria-hidden="true"], :disabled') || rect.bottom < bounds.top || rect.top > bounds.bottom || style.visibility === 'hidden' || +style.opacity < .9 || !rect.width || !rect.height) return [];
            ctx.clearRect(0, 0, 1, 1); ctx.fillStyle = base; ctx.fillRect(0, 0, 1, 1);
            const ancestors = []; for (let parent = node; parent && parent !== document.body; parent = parent.parentElement) ancestors.unshift(parent);
            for (const parent of ancestors) { ctx.fillStyle = getComputedStyle(parent).backgroundColor; ctx.fillRect(0, 0, 1, 1); }
            const bg = luminance(pixel());
            ctx.fillStyle = style.color; ctx.fillRect(0, 0, 1, 1); const fg = luminance(pixel());
            const ratio = (Math.max(bg,fg) + .05) / (Math.min(bg,fg) + .05);
            return ratio < 4.5 ? [{ text, ratio: +ratio.toFixed(2) }] : [];
          });
        });
        assert.deepEqual(lowContrast, [], `${expected}/${theme}/${screen} text contrast`);
        await page.locator('#app').screenshot({ path: new URL(`${expected}-${theme}-${screen}.png`, evidence).pathname.replace(/^\/(\w:)/, '$1') });
        if (screen === 'pair') {
          const shot = await page.locator('.connection-indicator').screenshot();
          const { data } = await sharp(shot).removeAlpha().raw().toBuffer({ resolveWithObject: true });
          const values = Array.from({ length: data.length / 3 }, (_, i) => (data[i*3] + data[i*3+1] + data[i*3+2])/3);
          assert(theme === 'light' ? Math.min(...values) < 90 : Math.max(...values) > 190, `${theme} WebGL ribbon must contrast with canvas`);
        }
      }
      await page.locator('#preview-home-count').fill('2');
      await page.selectOption('#preview-screen', 'home');
      await page.locator('.t-page:not([inert]) .more-button').first().click();
      const menuOverflow = await page.locator('.t-page:not([inert]) [data-open=true] [role=menuitem]').evaluateAll(nodes => nodes.filter(node => node.scrollWidth > node.clientWidth + 1).map(node => node.textContent));
      assert.deepEqual(menuOverflow, []);
      const menuGeometry = await page.locator('.t-page:not([inert]) .device-menu:not(.hdmi-menu)[data-open=true]').evaluate(node => {
        const panel = getComputedStyle(node), content = getComputedStyle(node.querySelector('.t-morph-menu'));
        const canvas = document.createElement('canvas'); canvas.width = canvas.height = 1; const ctx = canvas.getContext('2d');
        ctx.fillStyle = panel.getPropertyValue('--shadow-menu').trim(); ctx.fillRect(0,0,1,1);
        return { radius: panel.borderRadius, width: node.offsetWidth, height: node.offsetHeight, blur: panel.backdropFilter, padding: content.padding, gap: content.gap, shadow: panel.boxShadow, shadowRgba: [...ctx.getImageData(0,0,1,1).data], rows: [...node.querySelectorAll('[role=menuitem]')].map(item => { const css = getComputedStyle(item); return { radius: css.borderRadius, padding: css.padding, gap: css.gap, align: css.justifyContent, height: item.offsetHeight }; }) };
      });
      assert.equal(menuGeometry.radius, '23px'); assert.equal(menuGeometry.height, 84);
      assert.equal(menuGeometry.width, 140, 'All locales use the same 140px minimum');
      assert.equal(menuGeometry.padding, '4px'); assert.equal(menuGeometry.gap, '4px'); assert.equal(menuGeometry.blur, 'blur(24px)');
      assert(menuGeometry.shadow.includes('0px 4px 8px'), 'Menu drop shadow uses the supplied offset and radius');
      assert.deepEqual(menuGeometry.shadowRgba, [0,0,0, theme === 'light' ? 10 : 20], 'Light shadow #0000000A; retain dark shadow color');
      for (const row of menuGeometry.rows) assert.deepEqual(row, { radius: '18px', padding: '10px', gap: '8px', align: 'flex-start', height: 36 });
      const menuColors = await page.locator('.t-page:not([inert]) .device-menu:not(.hdmi-menu)[data-open=true]').evaluate(node => {
        const canvas = document.createElement('canvas'); canvas.width = canvas.height = 1; const ctx = canvas.getContext('2d');
        ctx.fillStyle = getComputedStyle(node).backgroundColor; ctx.fillRect(0,0,1,1);
        return { rgba: [...ctx.getImageData(0,0,1,1).data], text: getComputedStyle(node.querySelector('[role=menuitem]')).color, weight: getComputedStyle(node.querySelector('[role=menuitem]')).fontWeight };
      });
      assert.deepEqual(menuColors.rgba, theme === 'light' ? [255,255,255,153] : [47,47,47,153], 'Menu background must match supplied node props, not a guessed palette');
      assert.equal(menuColors.text, theme === 'light' ? 'rgb(0, 0, 0)' : 'rgb(255, 255, 255)');
      assert.equal(menuColors.weight, theme === 'light' ? '500' : '400', 'Menu uses the base weight plus 100 only in light mode');
      await checkTypography(page);
      await page.locator('#app').screenshot({ path: new URL(`${expected}-${theme}-menu.png`, evidence).pathname.replace(/^\/(\w:)/, '$1') });
      await page.keyboard.press('Escape');
      const editLabel = page.locator('.t-page:not([inert]) .device-menu:not(.hdmi-menu)').first().locator('[data-action=edit] span');
      const originalLabel = await editLabel.textContent();
      await editLabel.evaluate(node => node.textContent = 'Long menu action for width sizing');
      await page.locator('.t-page:not([inert]) .more-button').first().click();
      const expandedMenu = await page.locator('.t-page:not([inert]) .device-menu:not(.hdmi-menu)[data-open=true]').evaluate(node => {
        const item = node.querySelector('[data-action=edit]'), text = item.querySelector('span'), icon = item.querySelector('svg');
        return { width: node.offsetWidth, overflow: item.scrollWidth > item.clientWidth, iconInset: icon.getBoundingClientRect().left - item.getBoundingClientRect().left, textEnd: text.getBoundingClientRect().right, rowEnd: item.getBoundingClientRect().right };
      });
      assert(expandedMenu.width > 140, 'Long content must expand the menu beyond its minimum');
      assert.equal(expandedMenu.overflow, false); assert.equal(expandedMenu.iconInset, 10);
      assert(expandedMenu.textEnd <= expandedMenu.rowEnd - 10 + 1, 'Keep the right padding when content expands');
      await page.keyboard.press('Escape');
      await editLabel.evaluate((node, text) => node.textContent = text, originalLabel);
    }
    // Live language switching, translated chrome, keyboard selection, back and persistence.
    await page.locator('#language-setting').click();
    await page.locator('[data-value=ja]').click();
    assert.equal(await page.locator('html').getAttribute('lang'), 'ja');
    assert.equal(await page.locator('#compact-title').textContent(), '言語');
    assert.equal(await page.locator('#close').getAttribute('aria-label'), '閉じる');
    await page.keyboard.press('ArrowUp');
    assert.equal(await page.locator('html').getAttribute('lang'), 'en');
    await page.keyboard.press('Escape');
    assert.equal(await page.locator('#language-setting .preference-value').textContent(), 'English');
    await page.reload();
    await page.locator('#language-setting').waitFor();
    assert.equal(await page.locator('html').getAttribute('lang'), 'en');
    await context.close();
  }
  const motionContext = await browser.newContext({ locale: 'ko-KR', colorScheme: 'light', reducedMotion: 'no-preference', viewport: { width: 1000, height: 850 } });
  const motionPage = await motionContext.newPage();
  motionPage.on('pageerror', error => errors.push(error.message));
  await motionPage.goto('http://127.0.0.1:1422/?screen=home');
  for (const setting of ['language', 'theme']) {
    await motionPage.locator(`#${setting}-setting`).click();
    const pageEntry = await motionPage.locator(`.t-page[data-view="${setting}"]:not([inert])`).evaluate(node => node.getAnimations().flatMap(animation => animation.effect.getKeyframes()));
    assert(pageEntry.some(frame => frame.transform === 'translateX(100%)'), `Home → ${setting} must use the existing push slide`);
    const entering = await motionPage.locator('#compact-title').evaluate(node => node.getAnimations().flatMap(animation => animation.effect.getKeyframes()).some(frame => String(frame.filter).includes('blur')));
    assert(entering, 'Window titles use the existing blurred text-reveal entry');
    assert(await motionPage.locator('#compact-title').evaluate(node => node.getAnimations().flatMap(animation => animation.effect.getKeyframes()).every(frame => !frame.transform || frame.transform === 'none')), 'Title transitions must never add vertical or horizontal travel');
    await motionPage.waitForTimeout(550);
    const anchor = await motionPage.locator('#compact-title').evaluate(node => node.getBoundingClientRect().left);
    await motionPage.locator('#navigation').click();
    const pageExit = await motionPage.locator(`.t-page[data-view="${setting}"][inert]`).evaluate(node => node.getAnimations().flatMap(animation => animation.effect.getKeyframes()));
    assert(pageExit.some(frame => frame.transform === 'translateX(100%)'), `${setting} → home must use the existing pop slide`);
    const frames = await motionPage.locator('#compact-title').evaluate(node => new Promise(resolve => {
      const trace = [], start = performance.now();
      function tick() { trace.push({ x: node.getBoundingClientRect().left, opacity: +getComputedStyle(node).opacity, text: node.textContent }); if (performance.now() - start < 240) requestAnimationFrame(tick); else resolve(trace); }
      requestAnimationFrame(tick);
    }));
    assert(frames.filter(frame => frame.text && frame.opacity > .01).every(frame => Math.abs(frame.x - anchor) < .1), 'Outgoing title must not jump left when Back morphs to Add');
    assert.equal(await motionPage.locator('#compact-title').textContent(), '', 'Outgoing title must finish clearing');
  }
  await motionPage.locator('#language-setting').click();
  await motionPage.locator('#navigation').click();
  await motionPage.locator('#theme-setting').click();
  await motionPage.waitForTimeout(550);
  assert.equal(await motionPage.locator('#compact-title').textContent(), '테마', 'Rapid navigation cannot complete a stale title transition');
  await motionContext.close();
  for (const colorScheme of ['light', 'dark']) for (const reducedMotion of ['no-preference', 'reduce']) {
    const context = await browser.newContext({ locale: 'ko-KR', colorScheme, reducedMotion, viewport: { width: 1000, height: 850 } });
    const page = await context.newPage();
    page.on('pageerror', error => errors.push(error.message));
    await page.goto('http://127.0.0.1:1422/?screen=home');
    await page.selectOption('#preview-delay', '600');
    await page.locator('#check-update').click();
    for (const text of ['확인 중', '업데이트 진행 중']) {
      await page.waitForFunction(text => {
        const node = document.querySelector('#check-update > .t-text-swap:not(.shortcut-copy-outgoing)');
        return node?.textContent === text && node.classList.contains('is-thinking');
      }, text);
      const live = page.locator('#check-update > .t-text-swap:not(.shortcut-copy-outgoing)');
      assert.equal(await live.getAttribute('data-text'), text, 'Shimmer mask always matches the displayed status');
      const effect = await live.evaluate(node => ({ animation: getComputedStyle(node, '::before').animationName, display: getComputedStyle(node, '::before').display, font: getComputedStyle(node).fontWeight, position: getComputedStyle(node, '::before').backgroundPosition }));
      assert.equal(effect.animation, 't-think-shimmer');
      if (reducedMotion === 'reduce') assert.equal(effect.display, 'none');
      assert.equal(effect.font, '500');
      assert.equal(await page.locator('#check-update').getAttribute('aria-busy'), 'true');
      if (text === '확인 중' && reducedMotion === 'no-preference') {
        await page.waitForTimeout(100);
        assert.notEqual(await live.evaluate(node => getComputedStyle(node, '::before').backgroundPosition), effect.position, 'Shimmer band must actually move');
      }
    }
    await page.waitForFunction(() => {
      const node = document.querySelector('#check-update > .t-text-swap:not(.shortcut-copy-outgoing)');
      return node?.textContent === '업데이트 완료' && !node.classList.contains('is-thinking') && !node.hasAttribute('data-text');
    });
    await page.waitForFunction(() => document.querySelectorAll('.is-thinking').length === 0);
    assert.equal(await page.locator('.is-thinking').count(), 0, 'Completion also removes any outgoing shimmer copy');
    await context.close();
  }
  assert.deepEqual(errors, []);
  // Prevent new literal colors from bypassing the token hierarchy.
  for (const name of ['src/style.css','src/motion.css','dev/preview.css']) assert(!/#[\da-f]{3,8}\b|rgba?\(/i.test(readFileSync(new URL(`../ui/${name}`, import.meta.url), 'utf8')), `${name} has a literal color`);
  console.log('Language/theme: KO/EN/JA, unsupported locale fallback, live system changes, overrides, persistence, keyboard navigation, all screen layouts, token references, exact greens and WebGL ribbon contrast passed. Screenshots: local/test-results/theme-language/.');
} finally { await browser?.close(); await server?.close(); }
