import { demoDevices } from '../ui/dev/fixtures.ts';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { build, preview } from 'vite';

const { chromium } = createRequire(import.meta.url)('playwright');
const evidence = new URL('../local/test-results/hdmi-menu/', import.meta.url);
mkdirSync(evidence, { recursive: true });
// This bridge is confined to a separate build directory; it never controls a TV.
const bridge = `
import { initialState } from './model';
const demoDevices = ${JSON.stringify(demoDevices)};
export const desktop = true;
let state = JSON.parse(localStorage.getItem('test.state') || 'null') || { ...initialState, configured:true, stage:'ready', devices:demoDevices.map((d,i) => ({ ...d, pc_hdmi:i===0?3:null })) };
export const appUpdateState = async () => ({ version:'0.1.4', completed:false });
export const checkAppUpdate = async () => ({ status:'latest', version:'0.1.4' });
export const windowAction = async () => {};
export const connect = async callback => callback(structuredClone(state));
export async function command(name,args) {
  if (name==='set_device_hdmi') {
    const calls=JSON.parse(localStorage.getItem('test.calls')||'[]');calls.push({name,args});localStorage.setItem('test.calls',JSON.stringify(calls));
    if (localStorage.getItem('test.slow_hdmi')) await new Promise(resolve=>setTimeout(resolve,500));
    if (localStorage.getItem('test.fail_hdmi')) throw new Error('설정을 저장하지 못했어요.');
    state.devices=state.devices.map(d=>d.id===args.id?{...d,pc_hdmi:args.pcHdmi}:d);
  }
  localStorage.setItem('test.state',JSON.stringify(state));
  return structuredClone(state);
}
`;
const config = {
  configFile: fileURLToPath(new URL('../vite.config.ts', import.meta.url)),
  preview: { port:1426, strictPort:true },
  build: { outDir:fileURLToPath(new URL('../local/hdmi-menu-check', import.meta.url)), emptyOutDir:true, minify:true, cssMinify:true },
  plugins: [{ name:'hdmi-menu-check', enforce:'pre', load(id) {
    if (id.replaceAll('\\','/').split('?')[0].endsWith('/ui/src/bridge.ts')) return bridge;
  } }],
};
let browser, server;
try {
  await build(config); server=await preview(config);
  browser=await chromium.launch({channel:'msedge',headless:true});
  for (const locale of ['ko-KR','en-US','ja-JP']) for (const colorScheme of ['dark','light']) {
    const context=await browser.newContext({locale,colorScheme,reducedMotion:'reduce',viewport:{width:375,height:500}});
    const page=await context.newPage(); const errors=[];
    page.on('pageerror',error=>errors.push(error.message));
    await page.goto('http://127.0.0.1:1426');
    const first=page.locator('.saved-device').first(), second=page.locator('.saved-device').nth(1);
    const button=first.locator('.hdmi-button'), menu=first.locator('.hdmi-menu');
    await button.waitFor();
    assert.equal(await button.innerText(),'HDMI 3');
    await page.mouse.move(1,1);
    const idle=await button.evaluate(node=>({color:getComputedStyle(node).color,background:getComputedStyle(node).backgroundColor,x:node.firstElementChild.getBoundingClientRect().x,y:node.firstElementChild.getBoundingClientRect().y,width:node.firstElementChild.getBoundingClientRect().width,height:node.firstElementChild.getBoundingClientRect().height}));
    await button.hover();
    await page.waitForTimeout(25);
    const hovered=await button.evaluate(node=>({color:getComputedStyle(node).color,background:getComputedStyle(node).backgroundColor,chevron:getComputedStyle(node.querySelector('path')).stroke}));
    assert.equal(idle.background,'rgba(0, 0, 0, 0)');assert.notEqual(hovered.background,'rgba(0, 0, 0, 0)');
    assert.notEqual(idle.color,hovered.color,'Hover retains the foreground feedback');assert.equal(hovered.color,hovered.chevron);
    const footprint=await menu.evaluate(node=>{
      const box=node.getBoundingClientRect(),trigger=node.querySelector('.hdmi-trigger-content').getBoundingClientRect(),anchor=node.parentElement.getBoundingClientRect();
      return {left:trigger.left-box.left,right:box.right-trigger.right,top:trigger.top-box.top,bottom:box.bottom-trigger.bottom,radius:getComputedStyle(node).borderRadius,background:getComputedStyle(node.querySelector(".hdmi-button")).backgroundColor,x:trigger.x,y:trigger.y,width:trigger.width,height:trigger.height,anchorHeight:anchor.height};
    });
    assert([footprint.left-4,footprint.right-2,footprint.top-2,footprint.bottom-2].every(delta=>Math.abs(delta)<.02),'Hover surface expands L4/R2/T2/B2 around the existing trigger');
    assert.equal(footprint.radius,'4px');assert.notEqual(footprint.background,'rgba(0, 0, 0, 0)');
    assert.deepEqual([footprint.x,footprint.y,footprint.width,footprint.height],[idle.x,idle.y,idle.width,idle.height],'Hover keeps the actual trigger bounds unchanged');
    assert.equal(footprint.anchorHeight,16,'Background expansion does not change layout height');
    await page.locator('#app').screenshot({path:fileURLToPath(new URL(`${locale}-${colorScheme}-hover.png`,evidence))});
    assert(!/A5:D3|192\.168/.test(await page.locator('.saved-devices').innerText()),'Home shows HDMI instead of network addresses');
    const before=await first.boundingBox();
    assert.equal(before.height,72,'Keep the existing device row height');
    await button.click();
    await page.waitForTimeout(25);
    assert.equal(await menu.getAttribute('data-open'),'true');
    assert.equal(await menu.locator('[aria-checked=true]').getAttribute('data-port'),'3');
    const geometry=await menu.evaluate(node=>{
      node=node.querySelector('.hdmi-options'); const css=getComputedStyle(node), selected=node.querySelector('[aria-checked=true]');
      return {width:node.offsetWidth,height:node.offsetHeight,radius:css.borderRadius,blur:css.backdropFilter,gap:css.gap,selection:getComputedStyle(selected).backgroundColor,selectedY:selected.offsetTop,selectedHeight:selected.offsetHeight,rows:[...node.querySelectorAll('[role=menuitemradio]')].map(n=>({height:n.offsetHeight,padding:getComputedStyle(n).padding})),icon:getComputedStyle(node.querySelector('[data-port="3"] svg path')).fill};
    });
    assert.equal(geometry.width,140); assert.equal(geometry.height,148); assert.equal(geometry.radius,'20px');assert.equal(geometry.blur,'blur(24px)');
    assert.equal(geometry.selection,colorScheme==='dark'?'rgba(255, 255, 255, 0.08)':'rgba(0, 0, 0, 0.08)');
    assert.equal(geometry.selectedY,76);assert.equal(geometry.selectedHeight,32);
    assert.equal(geometry.gap,'4px');
    assert.deepEqual(geometry.rows,Array(4).fill({height:32,padding:'8px'}));
    assert.equal(geometry.icon,colorScheme==='dark'?'rgb(255, 255, 255)':'rgb(0, 0, 0)');
    assert(Math.abs((await menu.locator('.hdmi-options').boundingBox()).x-(idle.x-10))<.1,'Panel starts 10px before the trigger text');
    const openTrigger=await button.locator('.hdmi-trigger-content').boundingBox(), panel=await menu.locator('.hdmi-options').boundingBox();
    assert.equal(openTrigger.y,idle.y); assert.equal(openTrigger.height,16);
    assert.equal(await button.evaluate(n=>getComputedStyle(n).opacity),'1','Trigger remains visible');
    assert.equal(await button.locator('svg').evaluate(n=>getComputedStyle(n).transform),'matrix(-1, 0, 0, -1, 0, 0)','Expanded chevron points up');
    assert(Math.abs(panel.y-(openTrigger.y+openTrigger.height+6))<.1,'Panel opens below the trigger with a 6px gap');
    await page.locator('#app').screenshot({path:fileURLToPath(new URL(`${locale}-${colorScheme}-open.png`,evidence))});
    await page.keyboard.press('Escape');
    assert.equal(await button.getAttribute('aria-expanded'),'false');
    await button.click();await menu.locator('[data-port="1"]').click();
    await page.waitForFunction(()=>document.querySelector('.hdmi-menu').dataset.open==='false');
    await page.waitForFunction(()=>document.querySelector('.hdmi-button').innerText==='HDMI 1');
    assert.equal(await button.innerText(),'HDMI 1');
    assert(!/^HDMI [1-4]$/.test(await second.locator('.hdmi-button').innerText()),'Other TVs retain an unknown input');
    assert.equal((await first.boundingBox()).height,before.height);
    const calls=await page.evaluate(()=>JSON.parse(localStorage.getItem('test.calls')));
    assert.deepEqual(calls,[{name:'set_device_hdmi',args:{id:'tv-0',pcHdmi:1}}]);
    assert.equal(await page.locator('#automatic').getAttribute('aria-checked'),'true');
    assert.equal(await page.locator('#keep-tv-on').getAttribute('aria-checked'),'false');
    assert.equal(await page.locator('#shortcut-enabled').getAttribute('aria-checked'),'false');
    await page.reload();await button.waitFor();assert.equal(await button.innerText(),'HDMI 1');
    await button.click();
    await page.evaluate(()=>localStorage.setItem('test.fail_hdmi','1'));
    await menu.locator('[data-port="4"]').click();
    await page.waitForFunction(()=>document.querySelector('.hdmi-menu [data-port="1"]').getAttribute('aria-checked')==='true');
    assert.equal(await button.innerText(),'HDMI 1','Failed saves restore the committed choice');
    await page.keyboard.press('Escape');
    await first.locator('.more-button').click();
    const more=first.locator('.device-menu:not(.hdmi-menu)');
    assert.equal(await menu.getAttribute('data-open'),'false','Only one device menu can be open');
    const moreGeometry=await more.evaluate(n=>({width:n.offsetWidth,height:n.offsetHeight,radius:getComputedStyle(n).borderRadius,gap:getComputedStyle(n.querySelector('.t-morph-menu')).gap}));
    assert.deepEqual(moreGeometry,{width:140,height:84,radius:'23px',gap:'4px'},'More menu has a 4px item gap and matching height');
    assert.deepEqual(errors,[]);
    await context.close();
  }

  // The panel animates independently while the trigger remains visible and fixed.
  const page=await browser.newPage({locale:'ko-KR',colorScheme:'dark',reducedMotion:'no-preference',viewport:{width:375,height:500}});
  await page.goto('http://127.0.0.1:1426');
  const trigger=page.locator('.hdmi-button').first(); await trigger.waitFor();
  await page.waitForTimeout(300);
  const rest=await trigger.boundingBox();
  await trigger.hover(); await page.mouse.down(); await page.waitForTimeout(180);
  const held=await trigger.boundingBox();
  assert(Math.abs(held.width/rest.width-.96)<.005&&Math.abs(held.height/rest.height-.96)<.005,'Entire button scales as one surface');
  assert(Math.abs(held.x+held.width/2-rest.x-rest.width/2)<.02&&Math.abs(held.y+held.height/2-rest.y-rest.height/2)<.02,'Full background center stays fixed while pressed');
  await page.mouse.move(1,1);await page.mouse.up();await page.waitForTimeout(200);
  await trigger.click();
  const frames=await page.locator('.hdmi-menu').first().evaluate(menu=>new Promise(resolve=>{
    const panel=menu.querySelector('.hdmi-options'),button=menu.querySelector('.hdmi-button'),frames=[],start=performance.now();
    const sample=()=>{const box=button.getBoundingClientRect();frames.push({x:box.x,y:box.y,opacity:getComputedStyle(button).opacity,panelOpacity:parseFloat(getComputedStyle(panel).opacity)});if(performance.now()-start<300)requestAnimationFrame(sample);else resolve(frames);};sample();
  }));
  assert(frames.every(f=>Math.abs(f.x-rest.x)<.1&&Math.abs(f.y-rest.y)<.1&&f.opacity==='1'),'Opening preserves the trigger position and visibility');
  assert(frames.some(f=>f.panelOpacity>0&&f.panelOpacity<1),'Independent panel fades in');
  const closeFrames=await page.locator('.hdmi-menu').first().evaluate(menu=>new Promise(resolve=>{
    const panel=menu.querySelector('.hdmi-options'),button=menu.querySelector('.hdmi-button'),frames=[],start=performance.now();
    document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}));
    const sample=()=>{const css=getComputedStyle(panel),box=panel.getBoundingClientRect(),buttonBox=button.getBoundingClientRect();frames.push({time:performance.now()-start,opacity:parseFloat(css.opacity),x:box.x,y:box.y,buttonX:buttonBox.x,buttonY:buttonBox.y,blur:css.backdropFilter,visible:css.visibility,animations:panel.getAnimations().length});if(performance.now()-start<320)requestAnimationFrame(sample);else resolve(frames);};sample();
  }));
  assert(closeFrames.every(f=>Math.abs(f.buttonX-rest.x)<.1&&Math.abs(f.buttonY-rest.y)<.1),'Closing keeps trigger stationary');
  assert(closeFrames.every(f=>Math.abs(f.x-closeFrames[0].x)<.1&&Math.abs(f.y-closeFrames[0].y)<.1),'Panel closes without vertical or horizontal travel');
  assert(closeFrames.every((f,i)=>!i||f.opacity<=closeFrames[i-1].opacity+.001),'Close opacity decreases without a flash');
  assert(closeFrames.filter(f=>f.time>=200).every(f=>f.opacity===0&&f.visible==='hidden'&&f.animations===0),'Close resets invisibly without a second animation');
  assert.equal(await trigger.locator('svg').evaluate(n=>getComputedStyle(n).transform),'matrix(1, 0, 0, 1, 0, 0)','Collapsed chevron returns down');
  assert.equal(await page.locator('.hdmi-options').first().evaluate(n=>getComputedStyle(n).opacity),'0');
  await trigger.click(); await page.waitForTimeout(300);
  await page.keyboard.press('ArrowDown');await page.keyboard.press('Enter');
  await page.waitForFunction(()=>document.querySelector('.hdmi-menu').dataset.open==='false');
  await trigger.locator('.t-morph-new').waitFor({state:'attached'});
  const textFrames=await trigger.locator('.hdmi-label').evaluate(label=>new Promise(resolve=>{
    const samples=[],start=performance.now();
    const sample=()=>{
      const outgoing=label.querySelector('.t-morph-old'),incoming=label.querySelector('.t-morph-new');
      if(outgoing&&incoming) samples.push({prefix:label.firstChild.textContent,old:outgoing.textContent,next:incoming.textContent,hidden:outgoing.getAttribute('aria-hidden'),opacity:parseFloat(getComputedStyle(incoming).opacity),blur:getComputedStyle(incoming).filter});
      if(performance.now()-start<300)requestAnimationFrame(sample);else resolve(samples);
    };sample();
  }));
  assert(textFrames.length>0&&textFrames.every(f=>f.prefix==='HDMI '&&f.old==='3'&&f.next==='4'&&f.hidden==='true'),'Shared HDMI prefix stays fixed and outgoing digit is hidden from accessibility');
  assert(textFrames.some(f=>f.opacity>0&&f.opacity<1&&f.blur!=='blur(0px)'),'Changed digit cross-fades and blurs');
  assert.equal(await trigger.innerText(),'HDMI 4','Keyboard chooses and saves the next port');
  await trigger.click();await page.waitForTimeout(300);
  await page.locator('.hdmi-menu').first().locator('[data-port="2"]').click();
  await page.waitForFunction(()=>document.querySelector('.hdmi-button').innerText==='HDMI 2');
  // Dismissal must not wait on an IPC round trip or paint an uncommitted input.
  await page.evaluate(()=>localStorage.setItem('test.slow_hdmi','1'));
  await trigger.click();await page.waitForTimeout(300);
  const immediate=await page.locator('.hdmi-menu').first().evaluate(menu=>{
    menu.querySelector('[data-port="4"]').click();
    return {open:menu.dataset.open,label:menu.querySelector('.hdmi-label').textContent,selected:menu.querySelector('[aria-checked=true]').dataset.port,inert:menu.querySelector('[role=menu]').inert};
  });
  assert.deepEqual(immediate,{open:'false',label:'HDMI 2',selected:'2',inert:true},'Slow saves close immediately while preserving committed UI');
  await page.waitForFunction(()=>document.querySelector('.hdmi-button').innerText==='HDMI 4');
  await page.evaluate(()=>localStorage.removeItem('test.slow_hdmi'));

  // Rapid reversals retain the same center and settle both icon and surface.
  await page.waitForTimeout(300);
  const rapid=await page.locator('.hdmi-menu').first().evaluate(async menu=>{
    const button=menu.querySelector('.hdmi-button'),panel=menu.querySelector('.hdmi-options');
    const initial=button.getBoundingClientRect(),samples=[];
    for(let i=0;i<6;i++) {
      button.click(); await new Promise(resolve=>setTimeout(resolve,35));
      const box=button.getBoundingClientRect();
      samples.push({cx:box.x+box.width/2,cy:box.y+box.height/2});
    }
    await new Promise(resolve=>setTimeout(resolve,350));
    return {samples,cx:initial.x+initial.width/2,cy:initial.y+initial.height/2,open:menu.dataset.open,closing:menu.dataset.closing,opacity:getComputedStyle(panel).opacity,rotation:getComputedStyle(button.querySelector('svg')).transform,animations:panel.getAnimations().length};
  });
  assert(rapid.samples.every(f=>Math.abs(f.cx-rapid.cx)<.02&&Math.abs(f.cy-rapid.cy)<.02),'Repeated reversal never changes the trigger center');
  assert.equal(rapid.open,'false');assert.equal(rapid.closing,undefined);assert.equal(rapid.opacity,'0');assert.equal(rapid.animations,0);
  assert.equal(rapid.rotation,'matrix(1, 0, 0, 1, 0, 0)','Chevron settles after rapid reversal');

  // A close interrupted by a fresh open should reverse smoothly and settle.
  await trigger.click();await page.waitForTimeout(300);await page.keyboard.press('Escape');await page.waitForTimeout(60);
  await trigger.evaluate(node=>node.click());await page.waitForTimeout(300);
  assert.equal(await page.locator('.hdmi-menu').first().getAttribute('data-open'),'true');
  assert.equal(await page.locator('.hdmi-menu').first().getAttribute('data-closing'),null,'An older completion cannot leave stale closing state on a reopened menu');
  assert.equal(await page.locator('.hdmi-menu').first().locator('[aria-checked=true]').getAttribute('data-port'),'4');
  await page.keyboard.press('Escape');await page.waitForTimeout(300);

  await trigger.click();await page.waitForTimeout(300);
  const layers=await page.evaluate(()=>{
    const rows=[...document.querySelectorAll('.saved-device')];rows[1].querySelector('.more-button').click();
    return {closing:rows[0].querySelector('.hdmi-menu').dataset.closing,oldLayer:parseInt(getComputedStyle(rows[0]).zIndex),activeLayer:parseInt(getComputedStyle(rows[1]).zIndex)};
  });
  assert(layers.closing==='true'&&layers.activeLayer>layers.oldLayer,'A newly opened menu stays above a previously closing surface');
  await page.keyboard.press('Escape');await page.waitForTimeout(300);

  // A device near the bottom must expose all four choices inside the viewport.
  await page.evaluate(()=>{
    const state=JSON.parse(localStorage.getItem('test.state'));
    state.devices=Array.from({length:10},(_,i)=>({...state.devices[0],id:`tv-${i}`}));localStorage.setItem('test.state',JSON.stringify(state));
  });
  await page.reload();const last=page.locator('.hdmi-button').last();await last.waitFor();await last.evaluate(node=>node.scrollIntoView({block:'end'}));await last.click();await page.waitForTimeout(350);
  const lastRest=await last.boundingBox();
  const lastMenu=page.locator('.hdmi-menu').last();assert.equal(await lastMenu.getAttribute('data-side'),'up');
  const bounds=await lastMenu.locator('.hdmi-options').boundingBox();assert(bounds.y>=56&&bounds.y+bounds.height<=500,'Bottom device menu, including the expanded background, stays inside the window');
  const choices=await lastMenu.locator('[role=menuitemradio]').evaluateAll(nodes=>nodes.map(node=>{const box=node.getBoundingClientRect();return {top:box.top,bottom:box.bottom};}));
  assert(choices.every(box=>box.top>=56&&box.bottom<=500),'All four choices remain inside the visible content area');
  await page.keyboard.press('Escape');await page.mouse.move(1,1);await page.waitForTimeout(300);
  const lastClosed=await last.boundingBox();assert(Math.abs(lastRest.x-lastClosed.x)<.1&&Math.abs(lastRest.y-lastClosed.y)<.1,'Upward close preserves the original trigger position');
  await page.close();
  console.log('HDMI dropdown: L4/R2/T2/B2 hover, 20px panel radius, 16px item radius, 8px padding, 4px gaps, stable visible trigger, independent motion, KO/EN/JA × dark/light, save/reload, rollback, keyboard, interrupted close/reopen and bottom placement passed.');
} finally {await browser?.close();await server?.httpServer.close();}
