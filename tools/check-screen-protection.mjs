import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { build, preview } from 'vite';
const { chromium }=createRequire(import.meta.url)('playwright');
const evidence=new URL('../local/test-results/screen-protection/',import.meta.url);
mkdirSync(evidence,{recursive:true});
// No Tauri or real TV connection is available to this bundle.
const bridge=`
import { initialState } from './model';
export const desktop=true;
let state=JSON.parse(localStorage.getItem('screen.state')||'null')||{...initialState,configured:true,stage:'ready',devices:[{id:'test-tv',name:'Test TV',ip:'192.0.2.1',mac:null,pc_hdmi:2}]};
export const appUpdateState=async()=>({version:'0.1.6',completed:false});
export const checkAppUpdate=async()=>({status:'latest',version:'0.1.6'});
export const windowAction=async()=>{};
export const connect=async callback=>callback(structuredClone(state));
export async function command(name,args){
 if(name==='set_screen_protection'){
  if(localStorage.getItem('screen.fail')==='true')throw new Error('Test save failure');
  if(args.shortcut&&args.shortcut===state.shortcut)throw new Error('이미 다른 LTAC 단축키에 지정된 조합입니다.');
  Object.assign(state,{screen_enabled:args.enabled,screen_timeout_seconds:args.timeoutSeconds,screen_shortcut_enabled:args.shortcutEnabled,screen_shortcut:args.shortcut});
 }
 if(name==='set_shortcut')Object.assign(state,{shortcut_enabled:args.enabled,shortcut:args.shortcut});
 localStorage.setItem('screen.state',JSON.stringify(state));return structuredClone(state);
}`;
const config={configFile:fileURLToPath(new URL('../vite.config.ts',import.meta.url)),preview:{port:0,strictPort:false},build:{outDir:fileURLToPath(new URL('../local/screen-protection-check',import.meta.url)),emptyOutDir:true},plugins:[{name:'screen-test-bridge',enforce:'pre',load(id){if(id.replaceAll('\\','/').split('?')[0].endsWith('/ui/src/bridge.ts'))return bridge;}}]};
const headings=page=>page.locator('.device-section > h2, .settings-section > h2, .screen-section > h2').evaluateAll(nodes=>nodes.map(node=>{
 const s=getComputedStyle(node);return{size:s.fontSize,line:s.lineHeight,weight:s.fontWeight,family:s.fontFamily,spacing:s.letterSpacing,color:s.color};
}));
let browser,server;
try{
 await build(config);server=await preview(config);const base='http://127.0.0.1:'+server.httpServer.address().port;browser=await chromium.launch({channel:'msedge',headless:true});
 // A closing surface must meet its resting control before the DOM handoff.
 // Measure the material itself so fading content cannot conceal a color jump.
 for(const colorScheme of ['light','dark']){
  const context=await browser.newContext({viewport:{width:375,height:800},locale:'ko-KR',colorScheme}),page=await context.newPage();await page.goto(base);await page.locator('#screen-enabled').click();await page.waitForTimeout(550);
  for(const kind of ['time','more'])for(const action of kind==='time'?['escape','select']:['escape','outside']){
   const trigger=kind==='time'?page.locator('#screen-time'):page.locator('.more-button').first();await trigger.click();await page.waitForTimeout(450);
   const frames=await page.evaluate(({kind,action})=>new Promise(resolve=>{
    const trigger=kind==='time'?document.querySelector('#screen-time'):document.querySelector('.more-button'),surface=kind==='time'?document.querySelector('.screen-time-menu'):trigger.closest('.device-menu'),frames=[],start=performance.now();
    const backdrop=getComputedStyle(document.querySelector('.app-window')).backgroundColor.match(/[\d.]+/g).map(Number);
    const sample=()=>{
     const present=surface.isConnected,node=present?surface:trigger,style=getComputedStyle(node),rgba=style.backgroundColor.match(/[\d.]+/g).map(Number),alpha=rgba[3]??1;
     frames.push({phase:present&&surface.dataset.open==='true'?'open':present&&surface.dataset.closing==='true'?'closing':'closed',background:style.backgroundColor,paint:rgba.slice(0,3).map((value,i)=>value*alpha+backdrop[i]*(1-alpha)),blur:style.backdropFilter,opacity:style.opacity,transitions:node.getAnimations().filter(a=>a.transitionProperty).map(a=>({property:a.transitionProperty,duration:a.effect.getTiming().duration}))});
     if(performance.now()-start<550)requestAnimationFrame(sample);else resolve(frames);
    };sample();
    if(action==='escape')document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}));
    else if(action==='select')surface.querySelector('[data-seconds="1800"]').click();
    else document.body.click();
   }),{kind,action});
   const closing=frames.filter(f=>f.phase==='closing'),closed=frames.find(f=>f.phase==='closed');assert(closing.length>2&&closed,`${kind}: closing motion and handoff remain visible`);
   for(const property of ['background-color','box-shadow','backdrop-filter'])assert(closing.some(f=>f.transitions.some(t=>t.property===property&&t.duration===(kind==='time'?300:150))),`${kind}: ${property} follows the existing close duration`);
   assert(new Set(closing.map(f=>f.background)).size>2,`${kind}: material interpolates through intermediate colors`);assert(frames.every(f=>f.opacity==='1'),`${kind}: the surface never disappears to hide the handoff`);
   assert(Math.max(...closed.paint.map((channel,i)=>Math.abs(channel-closing.at(-1).paint[i])))<1,`${colorScheme} ${kind}: no color jump when the resting control takes over`);
   assert.equal(frames.at(-1).phase,'closed');
  }
  await context.close();
 }
 for(const reducedMotion of ['no-preference','reduce']){
  const context=await browser.newContext({viewport:{width:375,height:800},locale:'ko-KR',colorScheme:'dark',reducedMotion});
  const page=await context.newPage();const errors=[];page.on('pageerror',error=>errors.push(error.message));await page.goto(base);await page.locator('#screen-enabled').waitFor();await page.waitForTimeout(550);
  const enabled=page.locator('#screen-enabled'),panel=page.locator('#screen-settings'),shortcut=page.locator('#screen-shortcut-enabled');
  const copy=()=>page.locator('#screen-time-value').textContent();
  const choose=async seconds=>{await page.locator('#screen-time').click();await page.waitForTimeout(450);await page.locator(`.screen-time-menu [data-seconds="${seconds}"]`).click();};
  assert.equal(await page.locator('#screen-slider, .screen-slider-wrap, .screen-time-bounds').count(),0,'Slider and end captions remain absent');
  assert.equal(await page.locator('#screen-time-value.t-digit-group').count(),1);
  assert.equal(await enabled.getAttribute('aria-checked'),'false');assert.equal(await panel.getAttribute('aria-hidden'),'true');assert.equal(await copy(),'1분 30초');
  const titles=await headings(page);assert.equal(titles.length,3);assert.deepEqual(titles[0],titles[1]);assert.deepEqual(titles[1],titles[2]);assert.equal(titles[0].size,'13px');assert.equal(titles[0].line,'18px');assert.equal(titles[0].weight,'300');
  const order=await page.locator('.home-scroll > section').evaluateAll(nodes=>nodes.map(n=>n.className));assert(order.indexOf('settings-section')<order.indexOf('screen-section'));assert(order.indexOf('screen-section')<order.indexOf('general-section'));
  const b=await enabled.boundingBox();await page.mouse.move(b.x+b.width/2,b.y+b.height/2);
  await page.evaluate(()=>{
   window.switchTrace=[];window.traceSwitch=true;
   const tick=()=>{if(!window.traceSwitch)return;const n=document.querySelector('#screen-enabled'),r=n.getBoundingClientRect(),thumb=n.querySelector('.t-toggle-thumb'),t=thumb.getBoundingClientRect();window.switchTrace.push({width:r.width,height:r.height,dy:t.y+t.height/2-r.y-r.height/2,scale:getComputedStyle(n).scale,properties:n.getAnimations().map(a=>a.transitionProperty)});requestAnimationFrame(tick);};tick();
  });
  await page.mouse.down();await page.waitForTimeout(420);assert.equal((await enabled.boundingBox()).width,b.width);assert.equal((await enabled.boundingBox()).height,b.height);
  const pressed=await enabled.locator('.t-toggle-thumb').boundingBox();assert(Math.abs(pressed.width-32.4)<.03&&Math.abs(pressed.height-24)<.03,'Bald press compresses horizontally by 10%');assert(Math.abs(pressed.y+pressed.height/2-b.y-b.height/2)<.03);assert(Math.abs(pressed.x-b.x-2)<.03,'OFF press stays anchored to the left end');
  await page.mouse.up();await page.waitForTimeout(25);
  if(reducedMotion==='no-preference'){
   const travel=await enabled.locator('.t-toggle-thumb').evaluate(n=>{const a=n.getAnimations().find(a=>a.transitionProperty==='transform');return a?{duration:a.effect.getTiming().duration,easing:a.effect.getTiming().easing}:null;});
   assert(travel,'Bald transform transition handles travel and restoration');assert.equal(travel.duration,380);assert.equal(travel.easing,'cubic-bezier(0.34, 1.56, 0.64, 1)');
   const fill=await enabled.evaluate(n=>{const a=n.getAnimations().find(a=>a.transitionProperty==='background-color');return a?{duration:a.effect.getTiming().duration,easing:a.effect.getTiming().easing}:null;});assert.deepEqual(fill,{duration:260,easing:'ease'},'Bald background color transition is preserved');
  }
  await page.waitForTimeout(450);const trace=await page.evaluate(()=>{window.traceSwitch=false;return window.switchTrace;});assert(trace.length>10);assert(trace.every(f=>Math.abs(f.width-64)<.03&&Math.abs(f.height-28)<.03&&Math.abs(f.dy)<.03));assert(trace.every(f=>f.scale==='none'||f.scale==='1'));assert(trace.every(f=>f.properties.every(p=>p==='background-color')),'Switch shell never scales or translates');
  const restored=await enabled.locator('.t-toggle-thumb').boundingBox();assert(Math.abs(restored.width-36)<.03&&Math.abs(restored.height-24)<.03,'Knob returns to its original shape');
  await page.mouse.down();await page.waitForTimeout(420);const onPress=await enabled.locator('.t-toggle-thumb').boundingBox();assert(Math.abs(onPress.width-32.4)<.03);assert(Math.abs(onPress.x+onPress.width-b.x-b.width+2)<.03,'ON press stays anchored to the right end');await page.mouse.move(5,5);await page.mouse.up();await page.waitForTimeout(420);assert.equal(await enabled.getAttribute('aria-checked'),'true','Releasing outside cancels the click');assert(Math.abs((await enabled.locator('.t-toggle-thumb').boundingBox()).width-36)<.03,'Cancelled press restores the knob');
  await enabled.focus();await page.keyboard.down('Space');await page.waitForTimeout(420);assert(Math.abs((await enabled.locator('.t-toggle-thumb').boundingBox()).width-32.4)<.03,'Keyboard press uses the same Bald motion');await page.keyboard.up('Space');await page.waitForTimeout(450);assert.equal(await enabled.getAttribute('aria-checked'),'false');await enabled.click();await page.waitForTimeout(450);
  const reversals=await enabled.evaluate(async button=>{const thumb=button.querySelector('.t-toggle-thumb'),x=()=>new DOMMatrixReadOnly(getComputedStyle(thumb).transform).m41,result=[];for(let i=0;i<10;i++){const before=x(),old=button.dataset.on;button.click();for(let j=0;j<8;j++)await Promise.resolve();result.push({before,after:x(),changed:button.dataset.on!==old});await new Promise(resolve=>setTimeout(resolve,40));}return result;});
  assert(reversals.every(s=>s.changed),'Rapid clicks still commit each state change');if(reducedMotion==='no-preference')assert(reversals.every(s=>Math.abs(s.after-s.before)<.1),'CSS reverses from the current painted position');await page.waitForTimeout(420);assert.equal(await enabled.getAttribute('aria-checked'),'true');assert(Math.abs(await enabled.locator('.t-toggle-thumb').evaluate(n=>new DOMMatrixReadOnly(getComputedStyle(n).transform).m41)-24)<.03,'Rapid activation settles at the current state');
  assert.equal(await panel.getAttribute('aria-hidden'),'false');assert(await page.locator('.screen-time-setting').evaluate(n=>Math.abs(n.getBoundingClientRect().right-n.querySelector('#screen-time').getBoundingClientRect().right)<1));assert.equal(await page.locator('.screen-time-controls').evaluate(n=>n.getBoundingClientRect().height),44);
  await shortcut.click();await page.waitForTimeout(450);await page.locator('#screen-shortcut').click();await page.keyboard.press('Control+F9');await page.waitForTimeout(600);assert.equal(await page.locator('#screen-shortcut > .t-text-swap:not(.shortcut-copy-outgoing)').innerText(),'Ctrl + F9');
  await page.locator('#shortcut-enabled').click();await page.waitForTimeout(450);await page.locator('#shortcut').click();await page.keyboard.press('Control+F8');await page.waitForTimeout(600);await page.locator('#screen-shortcut').click();await page.keyboard.press('Control+F8');await page.waitForTimeout(450);assert.equal(await page.locator('#screen-shortcut').getAttribute('aria-pressed'),'true');await page.keyboard.press('Escape');await page.waitForTimeout(450);
  assert.equal(await page.locator('.screen-section').evaluate(n=>n.getBoundingClientRect().height),251);
  await page.locator('#screen-time').click();await page.waitForTimeout(450);
  const previousWidth=await page.locator('#screen-time').evaluate(n=>n.getBoundingClientRect().width);
  await page.evaluate(()=>{window.resizeTrace=[];window.traceTime=true;const tick=()=>{if(!window.traceTime)return;const button=document.querySelector('#screen-time'),r=button.getBoundingClientRect(),menu=document.querySelector('.screen-time-menu[data-closing=true]');window.resizeTrace.push({width:r.width,right:r.right,visible:getComputedStyle(button).visibility,menu:menu?{width:menu.getBoundingClientRect().width,right:menu.getBoundingClientRect().right}:null});requestAnimationFrame(tick);};tick();});
  await page.locator('.screen-time-menu [data-seconds="1800"]').click();
  if(reducedMotion==='no-preference'){
   const resize=await page.locator('#screen-time').evaluate(n=>{const a=n.getAnimations().find(a=>a.transitionProperty==='width');return a?{duration:a.effect.getTiming().duration,easing:a.effect.getTiming().easing,frames:a.effect.getKeyframes()}:null;});
   assert(resize,'Time button width interpolates instead of jumping');assert.equal(resize.duration,300);assert.equal(resize.easing,'cubic-bezier(0.22, 1, 0.36, 1)');assert(Math.abs(parseFloat(resize.frames[0].width)-previousWidth)<.1);
  }
  await page.waitForTimeout(100);
  const pop=await page.locator('#screen-time-value').evaluate(n=>({label:n.getAttribute('aria-label'),animations:n.getAnimations({subtree:true}).map(a=>({name:a.animationName,duration:a.effect.getTiming().duration,delay:a.effect.getTiming().delay,frames:a.effect.getKeyframes()})),parent:n.getAnimations().length}));
  if(reducedMotion==='no-preference'){
   const copies=await page.evaluate(()=>['#screen-time-value','.screen-time-menu .t-morph-plus .t-digit-group'].map(selector=>{const n=document.querySelector(selector),s=getComputedStyle(n),r=n.getBoundingClientRect();return{font:s.font,spacing:s.letterSpacing,width:r.width,height:r.height,characters:[...n.children].map(c=>{const r=c.getBoundingClientRect();return{width:r.width,height:Math.round(r.height*100)/100};})};}));
   assert.deepEqual(copies[0],copies[1],'Morph digits and button digits share the exact font, line height and character metrics');
  }
  assert.equal(pop.label,'30분');assert.equal(pop.parent,0,'The complete label never slides or fades');
  assert.equal(await page.locator('#screen-time .shortcut-copy-outgoing').count(),0);
  if(reducedMotion==='no-preference'){
   assert.equal(pop.animations.length,3);assert(pop.animations.every(a=>a.name==='t-digit-pop-in'&&a.duration===500));assert.deepEqual(pop.animations.map(a=>a.delay),[0,70,140]);
   assert(pop.animations.every(a=>a.frames[0].opacity==='0'&&a.frames[0].filter==='blur(2px)'&&a.frames[0].transform.includes('8px')),'Reference motion: 8px rise, 2px blur, opacity 0→1');
  }else assert.equal(pop.animations.length,0);
  await page.waitForTimeout(700);assert.equal(await copy(),'30분');assert.equal(await page.locator('#screen-time-value').getAttribute('class'),'t-digit-group');
  const resizeTrace=await page.evaluate(()=>{window.traceTime=false;return window.resizeTrace;});
  const finalWidth=await page.locator('#screen-time').evaluate(n=>n.getBoundingClientRect().width);
  assert(resizeTrace.every(frame=>Math.abs(frame.right-resizeTrace[0].right)<.1),'Time button remains right aligned while resizing');
  if(reducedMotion==='no-preference'){
   assert(resizeTrace.filter(frame=>frame.width<previousWidth-.5&&frame.width>finalWidth+.5).length>=2,'Resize has intermediate widths');
   const closing=resizeTrace.filter(frame=>frame.menu);assert(closing.length>2);assert(Math.abs(closing.at(-1).menu.width-finalWidth)<.5,'Morph closes directly to the new time width');
   assert(resizeTrace.filter(frame=>frame.menu).every(frame=>frame.visible==='hidden'),'Button stays hidden until the morph hands off');
  }
  assert.equal(await page.locator('#screen-time').evaluate(n=>getComputedStyle(n).visibility),'visible');assert.equal(await page.locator('#screen-time').evaluate(n=>n===document.activeElement),true);
  const settledGeometry=()=>page.locator('#screen-time-value .t-digit').evaluateAll(nodes=>nodes.map(n=>{const r=n.getBoundingClientRect();return{x:r.x,y:r.y,width:r.width,height:r.height};}));
  const settled=await settledGeometry();await page.waitForTimeout(180);assert.deepEqual(await settledGeometry(),settled,'Digits remain fixed after motion cleanup');
  await page.locator('#screen-time').click();await page.waitForTimeout(450);assert.equal(await page.locator('.screen-time-menu [aria-checked=true]').getAttribute('data-seconds'),'1800');assert(await page.locator('.screen-time-menu [aria-checked=true]').evaluate(n=>{const r=n.getBoundingClientRect(),s=n.closest('[role=menu]'),b=s.getBoundingClientRect(),style=getComputedStyle(s);return r.top>=b.top+parseFloat(style.paddingTop)-.1&&r.bottom<=b.bottom-parseFloat(style.paddingBottom)+.1;}),'A lower selection opens fully visible inside the menu padding');await page.locator('.screen-time-menu [data-seconds="30"]').click();
  assert.equal(await page.locator('#screen-time-value .t-digit[data-same]').allTextContents().then(parts=>parts.join('')),'30','Shared leading digits stay still');
  await page.waitForTimeout(750);assert.equal(await copy(),'30초');await page.locator('.screen-section').screenshot({path:fileURLToPath(new URL(`section-${reducedMotion}.png`,evidence))});
  await page.locator('#screen-time').click();await page.waitForTimeout(450);await page.keyboard.press('Home');await page.keyboard.press('Enter');await page.waitForTimeout(700);assert.equal(await copy(),'10초','10 seconds is the first selectable preset');await page.reload();await enabled.waitFor();assert.equal(await enabled.locator('.t-toggle-thumb').evaluate(n=>n.getAnimations().length),0,'Restored ON state has no entrance motion');await page.waitForTimeout(600);assert.equal(await copy(),'10초','10-second selection persists after reload');
  await enabled.click();await page.waitForTimeout(450);assert.equal(await panel.evaluate(n=>n.getBoundingClientRect().height),0);assert.equal(await panel.evaluate(n=>n.inert),true);await enabled.click();await page.waitForTimeout(450);assert.equal(await page.locator('#screen-shortcut-settings').getAttribute('aria-hidden'),'false');await shortcut.click();await page.waitForTimeout(450);assert.equal(await page.locator('#screen-shortcut-settings').evaluate(n=>n.getBoundingClientRect().height),0);
  await page.evaluate(()=>{const s=JSON.parse(localStorage.getItem('screen.state'));s.screen_timeout_seconds=159;localStorage.setItem('screen.state',JSON.stringify(s));});await page.reload();await enabled.waitFor();await page.waitForTimeout(650);assert.equal(await copy(),'2분 39초');await page.locator('#screen-time').click();await page.waitForTimeout(450);assert.equal(await page.locator('.screen-time-menu [aria-checked=true]').getAttribute('data-seconds'),'159');await page.keyboard.press('Escape');await page.waitForTimeout(350);
  await page.evaluate(()=>localStorage.setItem('screen.fail','true'));await choose(60);await page.waitForTimeout(1100);assert.equal(await copy(),'2분 39초','Failure restores the saved legacy value');await page.evaluate(()=>localStorage.removeItem('screen.fail'));await choose(60);await page.waitForTimeout(700);assert.equal(await copy(),'1분');await page.reload();await enabled.waitFor();await page.waitForTimeout(600);assert.equal(await copy(),'1분');assert.equal(await enabled.getAttribute('aria-checked'),'true');assert.deepEqual(errors,[]);await context.close();
 }
 for(const locale of ['ko-KR','en-US','ja-JP'])for(const colorScheme of ['dark','light']){
  const context=await browser.newContext({viewport:{width:375,height:500},locale,colorScheme,reducedMotion:'reduce'}),page=await context.newPage();await page.goto(base);await page.locator('#screen-enabled').click();await page.waitForTimeout(80);
  const styles=await headings(page);assert.deepEqual(styles[0],styles[1]);assert.deepEqual(styles[1],styles[2]);assert.equal(styles[0].size,'13px');assert.equal(styles[0].weight,colorScheme==='dark'?'300':'400');await page.locator('#screen-time').click();await page.waitForTimeout(80);
  const menu=page.locator('.screen-time-menu'),scroll=menu.locator('.app-scrollbar'),bounds=await menu.boundingBox();assert(bounds.x>=0&&bounds.y>=0&&bounds.x+bounds.width<=375&&bounds.y+bounds.height<=500&&bounds.height<=256);assert.equal(await menu.getAttribute('data-scroll-ready'),'true');assert(await scroll.evaluate(n=>n.scrollHeight>n.clientHeight));
  assert.equal(await scroll.evaluate(n=>n.scrollTop),0,'Opening with an already-visible selection starts at the top');assert(await scroll.evaluate(n=>Math.abs(n.querySelector('[role=menuitemradio]').getBoundingClientRect().top-n.getBoundingClientRect().top-8)<.1),'The first row retains the full 8px opening inset');
  const track=menu.locator('.screen-time-scrollbar'),radius=await menu.evaluate(n=>getComputedStyle(n).borderTopRightRadius);
  const trackStyle=await track.evaluate(n=>{const s=getComputedStyle(n);return{top:s.top,bottom:s.bottom,width:s.width,right:s.right,position:s.position,opacity:s.opacity};});assert.equal(radius,'24px');assert.equal(trackStyle.top,'22px');assert.equal(trackStyle.bottom,'22px');assert.equal(trackStyle.width,'3px');assert.equal(trackStyle.right,'2px');assert.equal(trackStyle.position,'absolute');assert.equal(trackStyle.opacity,'1');assert.equal(await scroll.evaluate(n=>getComputedStyle(n).padding),'8px');
  assert.equal(await scroll.evaluate(n=>getComputedStyle(n).scrollbarWidth),'none');assert.equal(await scroll.evaluate(n=>n.offsetWidth-n.clientWidth),0,'Native scrollbar reserves no space');
  assert.equal(await menu.evaluate(n=>n.getBoundingClientRect().right-n.querySelector('.screen-time-scrollbar').getBoundingClientRect().right),2,'Overlay scrollbar sits 2px inside the existing menu');
  assert.equal(await menu.evaluate(n=>n.getBoundingClientRect().width-n.querySelector('.app-scrollbar').getBoundingClientRect().width),0,'Menu content retains the full width');
  assert(await menu.evaluate(n=>Math.abs(n.getBoundingClientRect().width-parseFloat(n.style.getPropertyValue('--menu-open-width')))<.03),'Scrollbar does not add width to the menu');
  const color=await page.locator('.hdmi-menu [role=menuitemradio][aria-checked=false]').first().evaluate(n=>getComputedStyle(n).color);assert.equal(await menu.locator('[role=menuitemradio][aria-checked=false]').last().evaluate(n=>getComputedStyle(n).color),color);
  const selected=menu.locator('[role=menuitemradio][aria-checked=true]'),selection=colorScheme==='dark'?'rgba(255, 255, 255, 0.08)':'rgba(0, 0, 0, 0.08)';
  const weights=await menu.locator('[role=menuitemradio]').evaluateAll(nodes=>nodes.map(n=>({selected:n.getAttribute('aria-checked')==='true',weight:Number(getComputedStyle(n).fontWeight)})));assert.equal(weights.find(n=>n.selected).weight,weights.find(n=>!n.selected).weight+100,'Selected time text is exactly 100 heavier');
  assert.equal(weights.find(n=>!n.selected).weight,await page.locator('.hdmi-menu [role=menuitemradio]').first().evaluate(n=>Number(getComputedStyle(n).fontWeight)),'Time and HDMI menu items share the same base weight');
  await page.keyboard.press('ArrowDown');await page.mouse.move(1,1);assert.equal(await selected.evaluate(n=>getComputedStyle(n).backgroundColor),selection,'Selected background matches HDMI and persists after focus moves');await selected.hover();assert.equal(await selected.evaluate(n=>getComputedStyle(n).backgroundColor),selection,'Hover preserves the selected background');await selected.focus();assert.equal(await selected.evaluate(n=>getComputedStyle(n).backgroundColor),selection,'Focus preserves the selected background');await page.mouse.move(1,1);
  await page.screenshot({path:fileURLToPath(new URL(`menu-${locale}-${colorScheme}.png`,evidence))});await context.close();
 }
 const context=await browser.newContext({viewport:{width:375,height:650},colorScheme:'dark'}),page=await context.newPage();await page.goto(base);await page.locator('#screen-enabled').click();await page.waitForTimeout(550);
 await page.evaluate(()=>{window.scrollbarFade=[];window.traceScrollbar=true;const tick=()=>{if(!window.traceScrollbar)return;const menu=document.querySelector('.screen-time-menu'),bar=menu?.querySelector('.screen-time-scrollbar');if(bar)window.scrollbarFade.push({time:performance.now(),ready:menu.dataset.scrollReady==='true',opacity:Number(getComputedStyle(bar).opacity)});requestAnimationFrame(tick);};tick();});await page.locator('#screen-time').click();
 const scroller=page.locator('.screen-time-menu .app-scrollbar'),bar=page.locator('.screen-time-scrollbar');assert.equal(await scroller.evaluate(n=>getComputedStyle(n).overflowY),'hidden');const contentWidth=await scroller.evaluate(n=>n.clientWidth),rowWidth=await scroller.locator('button').first().evaluate(n=>n.offsetWidth);await page.waitForTimeout(600);assert.equal(await scroller.evaluate(n=>getComputedStyle(n).overflowY),'auto');assert.equal(await scroller.evaluate(n=>n.clientWidth),contentWidth);assert.equal(await scroller.locator('button').first().evaluate(n=>n.offsetWidth),rowWidth,'Showing the scrollbar cannot reflow menu rows');
 const fade=await page.evaluate(()=>{window.traceScrollbar=false;return window.scrollbarFade;});assert(fade.filter(f=>!f.ready).every(f=>f.opacity===0),'Scrollbar stays hidden until expansion finishes');const ready=fade.filter(f=>f.ready),visible=ready.filter(f=>f.opacity>0&&f.opacity<.999);assert(visible.length>=4,'Scrollbar appears through multiple fade frames');assert(visible[0].opacity<.15,'Appearance starts softly instead of jumping to a high opacity');assert(ready.find(f=>f.opacity>=.999).time-ready[0].time>=200,'Scrollbar fades in gradually after the menu settles');assert(ready.every((f,i)=>!i||f.opacity>=ready[i-1].opacity),'Appearance never flickers or reverses');
 await page.waitForTimeout(1100);assert.equal(await bar.evaluate(n=>getComputedStyle(n).opacity),'0','Scrollbar gently hides after inactivity');assert.equal(await bar.evaluate(n=>getComputedStyle(n).pointerEvents),'none');assert.equal(await scroller.evaluate(n=>n.clientWidth),contentWidth,'Auto-hide does not change content width');await scroller.hover();await page.mouse.wheel(0,40);await page.waitForTimeout(300);assert.equal(await bar.evaluate(n=>getComputedStyle(n).opacity),'1','Scrolling reveals the scrollbar again');
 await scroller.evaluate(n=>n.scrollTop=0);await page.waitForTimeout(60);const thumb=await bar.locator('.screen-time-scrollbar-thumb').boundingBox();await page.mouse.move(thumb.x+thumb.width/2,thumb.y+thumb.height/2);await page.mouse.down();await page.waitForTimeout(1100);assert.equal(await bar.evaluate(n=>getComputedStyle(n).opacity),'1','Scrollbar remains visible during an active drag');await page.mouse.move(thumb.x+thumb.width/2,thumb.y+thumb.height/2+30,{steps:6});await page.mouse.up();await page.waitForTimeout(60);assert(await scroller.evaluate(n=>n.scrollTop>0),'Overlay thumb supports dragging');await scroller.evaluate(n=>n.scrollTop=0);await scroller.hover();await page.mouse.wheel(0,80);await page.waitForTimeout(200);assert(await scroller.evaluate(n=>n.scrollTop>0),'Wheel scrolling still uses the native scroller');await page.waitForTimeout(1400);assert.equal(await bar.evaluate(n=>getComputedStyle(n).opacity),'0','Scrollbar hides again after scrolling stops');
 await page.keyboard.press('Escape');if(await page.locator('.screen-time-menu').count())assert.equal(await scroller.evaluate(n=>getComputedStyle(n).overflowY),'hidden');await page.waitForTimeout(450);assert.equal(await page.locator('#screen-time').evaluate(n=>n===document.activeElement),true);await context.close();
 for(const reducedMotion of ['no-preference','reduce']){
  const ctx=await browser.newContext({viewport:{width:375,height:500},locale:'ko-KR',colorScheme:'dark',reducedMotion}),p=await ctx.newPage();await p.goto(base);await p.locator('#screen-enabled').click();await p.waitForTimeout(550);
  const trigger=p.locator('#screen-time'),home=p.locator('.home-scroll');
  const open=async(wait=450)=>{await trigger.click();if(wait)await p.waitForTimeout(wait);};
  const scrollPage=async()=>{
   const hadMenu=await p.locator('.screen-time-menu').count();
   const moved=await home.evaluate(n=>{const previous=n.scrollTop;n.scrollTop+=n.scrollTop<n.scrollHeight-n.clientHeight-30?30:-30;return{previous,next:n.scrollTop};});assert.notEqual(moved.previous,moved.next);await p.waitForTimeout(35);
   if(hadMenu&&reducedMotion==='no-preference'){
    assert.equal(await p.locator('.screen-time-menu').getAttribute('data-closing'),'true','Page scroll preserves the closing morph');
    assert.equal(await trigger.evaluate(n=>getComputedStyle(n).visibility),'hidden','Surface hands off only after reaching the trigger');await p.waitForTimeout(400);
   }
   assert.equal(await p.locator('.screen-time-menu').count(),0);assert.equal(await trigger.evaluate(n=>getComputedStyle(n).visibility),'visible');assert.equal(await trigger.getAttribute('aria-expanded'),'false');assert.equal(await home.evaluate(n=>n.scrollTop),moved.next,'Focus restoration never scrolls the page back');
  };
  await open();
  const trace=await p.evaluate(async()=>{
   const scroll=document.querySelector('.home-scroll'),button=document.querySelector('#screen-time');
   const initial=scroll.scrollTop,dir=initial>120?-1:1,frames=[];
   for(let i=0;i<23;i++){
    scroll.scrollTop=initial+dir*(30+(i<10?i*5:(20-i)*5));
    await new Promise(resolve=>requestAnimationFrame(()=>setTimeout(resolve,0)));
    const menu=document.querySelector('.screen-time-menu'),b=button.getBoundingClientRect(),m=menu?.getBoundingClientRect(),animation=menu?.getAnimations().find(a=>a.transitionProperty==='width');
    frames.push({time:performance.now(),menu:!!menu,dy:m?m.top-b.top:null,dx:m?m.right-b.right:null,height:m?.height,visible:getComputedStyle(button).visibility,start:animation?.startTime,duration:animation?.effect.getTiming().duration});
    await new Promise(resolve=>setTimeout(resolve,16));
   }
   return frames;
  });
  if(reducedMotion==='no-preference'){
   const moving=trace.filter(f=>f.menu);assert(moving.length>4,'Closing animation remains visible during page scrolling');assert(moving.every(f=>f.visible==='hidden'));
   const starts=moving.map(f=>f.start).filter(v=>v!=null);assert.equal(new Set(starts).size,1,'Continuous scroll never restarts the original morph timeline');assert(moving.filter(f=>f.duration!=null).every(f=>f.duration===300),'Existing close duration is preserved');
   assert(Math.abs(moving.at(-1).dy)<.5&&Math.abs(moving.at(-1).dx)<.5,'Morph meets the live trigger after scrolling reverses');
   for(let i=1;i<moving.length;i++)assert(Math.abs(moving[i].dy)<=Math.abs(moving[i-1].dy)+.1,'Remaining distance eases to zero while the anchor moves');
  }
  assert.equal(trace.at(-1).menu,false);assert.equal(trace.at(-1).visible,'visible');
  for(let i=0;i<6;i++)await scrollPage();
  await open(120);await scrollPage();await p.waitForTimeout(450);assert.equal(await p.locator('.screen-time-menu').count(),0,'Interrupted opening completes its reverse morph and releases the portal');
  await open();await p.locator('.screen-time-menu [data-seconds="600"]').click();await scrollPage();
  await open();assert.equal(await p.locator('.screen-time-menu').getAttribute('data-open'),'true');assert.equal(await p.locator('.screen-time-menu').getAttribute('data-scroll-ready'),'true');assert.equal(await trigger.evaluate(n=>getComputedStyle(n).visibility),'hidden','Old closing completion cannot reveal a newly opened trigger');assert.equal(await p.locator('.screen-time-menu [aria-checked=true]').getAttribute('data-seconds'),'600');
  await p.setViewportSize({width:375,height:520});await p.waitForTimeout(35);
  if(reducedMotion==='no-preference')assert.equal(await p.locator('.screen-time-menu').getAttribute('data-closing'),'true','Viewport movement preserves the morph');
  await p.setViewportSize({width:375,height:540});await p.waitForTimeout(400);assert.equal(await p.locator('.screen-time-menu').count(),0);assert.equal(await trigger.evaluate(n=>getComputedStyle(n).visibility),'visible');
  await p.evaluate(()=>{document.body.style.height='1800px';document.body.style.overflow='auto';document.querySelector('#app').style.height='500px';});await open();
  for(let i=0;i<4;i++){await p.evaluate(()=>window.scrollBy(0,20));await p.waitForTimeout(25);}
  if(reducedMotion==='no-preference')assert.equal(await p.locator('.screen-time-menu').getAttribute('data-closing'),'true','Outer document scrolling also keeps the original animation');
  await p.waitForTimeout(400);assert.equal(await p.locator('.screen-time-menu').count(),0);assert.equal(await trigger.evaluate(n=>getComputedStyle(n).visibility),'visible');
  await ctx.close();
 }
 console.log('Screen protection: selected backgrounds persist across focus/hover in all locales/themes; time and More menus close with continuous material and precise handoff; Bald switch, scroll-follow morph, menu input, width, overlay scrollbar, Number pop-in, reduced motion, rollback and persistence passed.');
}finally{await browser?.close();await server?.httpServer.close();}
