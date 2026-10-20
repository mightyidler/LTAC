import {createRequire} from 'node:module';
import assert from 'node:assert/strict';
import {createServer} from 'vite';
const server=await createServer({server:{port:1427,strictPort:true}});
await server.listen();
const {chromium}=createRequire(import.meta.url)('playwright');
const browser=await chromium.launch({channel:'msedge',headless:true});
try {
 for(const colorScheme of ['dark','light']) {
  const context=await browser.newContext({colorScheme,locale:'ko-KR',viewport:{width:1000,height:850}});
  const page=await context.newPage();
  await page.goto('http://127.0.0.1:1427/?screen=home');
  await page.locator('#shortcut').click();
  await page.waitForFunction(()=>document.querySelector('#shortcut.status-active'));
  await page.waitForTimeout(600);
  assert.equal(await page.locator('#shortcut').evaluate(n=>getComputedStyle(n).backgroundColor),colorScheme==='dark'?'rgba(255, 146, 48, 0.12)':'rgba(255, 141, 40, 0.12)','Hover must preserve the orange status surface');
  async function sample(selector,action,duration=1300) {
   return page.evaluate(({selector,action,duration})=>new Promise(resolve=>{
    const button=document.querySelector(selector),frames=[],start=performance.now();
    if(action==='escape') document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true})); else button.click();
    function frame(){const css=getComputedStyle(button);frames.push({time:performance.now()-start,bg:css.backgroundColor,shadow:css.boxShadow,weights:[...button.querySelectorAll('.is-thinking')].map(n=>getComputedStyle(n).fontWeight),outgoing:[...button.querySelectorAll('.shortcut-copy-outgoing.is-thinking')].map(n=>getComputedStyle(n).color)});if(performance.now()-start<duration)requestAnimationFrame(frame);else resolve(frames);}frame();
   }),{selector,action,duration});
  }
  function verify(frames,name){
   assert(frames.every(f=>f.weights.every(w=>w==='500')),`${name}: status weights stay at 500 throughout the transition`);
   const alpha=bg=>bg.startsWith('rgba(')?Number(bg.slice(bg.lastIndexOf(',')+1,-1)):1;
   assert(frames.every(f=>alpha(f.bg)<=.125),`${colorScheme} ${name}: never flashes an opaque black or white surface`);
   assert(frames.every((f,i)=>!i||Math.abs(alpha(f.bg)-alpha(frames[i-1].bg))<.025),`${name}: background alpha stays continuous`);
   assert(frames.every(f=>f.outgoing.every(c=>c===(colorScheme==='dark'?'rgb(255, 146, 48)':'rgb(255, 141, 40)'))),`${name}: outgoing orange copy retains its color`);
  }
  verify(await sample('#shortcut','escape'),'shortcut cancellation');
  await page.selectOption('#preview-delay','600');
  verify(await sample('#check-update','click',2200),'update checking → installing → completion');
  assert.equal(await page.locator('#check-update.status-active').count(),0);
  await context.close();
 }
 console.log('Dark/light animation frames: no black/white surface flash, continuous opacity, stable outgoing text, and correct final status.');
} finally {await browser.close();await server.close();}
