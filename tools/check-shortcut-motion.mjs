import assert from 'node:assert/strict';
import { swapShortcutLabel } from '../ui/src/motion.ts';
const originalTimeout = globalThis.setTimeout;
let now = 0, reduced = false;
const timers = [];
globalThis.setTimeout = (callback, ms) => { timers.push({ callback, at: now + ms }); return timers.length; };
globalThis.document = { documentElement: {} };
globalThis.matchMedia = () => ({ matches: reduced });
const tokens = { '--duration-micro': '80ms', '--text-swap-dur': '250ms', '--shortcut-copy-out-dur': '350ms', '--shortcut-overlap-delay': '120ms', '--shortcut-surface-dur': '350ms' };
globalThis.getComputedStyle = node => {
  const box=node.layoutBox?.() ?? {};
  return { width:`${box.width}px`, height:`${box.height}px`, fontWeight:node.style?.fontWeight || '400', paddingLeft:'8px', paddingRight:'8px', borderLeftWidth:'1px', borderRightWidth:'1px', getPropertyValue: key => tokens[key] ?? '' };
};
async function flush() { for (let i=0;i<6;i++) await Promise.resolve(); }
async function advance(ms) {
  const end = now + ms;
  while (true) {
    timers.sort((a,b) => a.at-b.at);
    if (!timers.length || timers[0].at > end) break;
    const timer=timers.shift(); now=timer.at; timer.callback(); await flush();
  }
  now=end;
}
class Style {
  fontWeight='';
  setProperty(key,value) { this[key]=value; }
  removeProperty(key) { delete this[key]; }
}
class Classes {
  values=new Set(); everAdded=new Set();
  add(...values) { values.forEach(value => {this.values.add(value);this.everAdded.add(value);}); }
  remove(...values) { values.forEach(value => this.values.delete(value)); }
  contains(value) { return this.values.has(value); }
  toggle(value,on) { if(on)this.add(value);else this.remove(value); }
}
function makeButton() {
  const natural=copy=>(copy.textContent.length*7+.375)*(copy.style.fontWeight==='500'?1.05:1);
  const button={id:'shortcut',isConnected:true,scale:1,classList:new Classes(),style:new Style(),copies:[],querySelector:()=>label,getAnimations:()=>[],append(node){this.copies.push(node);},layoutBox(){const width=parseFloat(this.style.width)||Math.min(220,(parseFloat(label.style.width)||natural(label))+18);return {width,height:parseFloat(this.style.height)||Math.max(28,label.layoutBox().height+10)};},getBoundingClientRect(){const box=this.layoutBox();return {width:box.width*this.scale,height:box.height*this.scale};},get offsetWidth(){return this.layoutBox().width;}};
  const makeLabel=()=>({textContent:'단축키 지정',classList:new Classes(),style:new Style(),offsetHeight:18,attributes:{},removeAttribute(key){delete this.attributes[key];},setAttribute(key,value){this.attributes[key]=value;},remove(){button.copies=button.copies.filter(node=>node!==this);},layoutBox(){const width=this.style.width==='max-content'?natural(this):parseFloat(this.style.width)||natural(this);return {width,height:this.style.whiteSpace==='pre'?18:Math.ceil(natural(this)/width)*18};},getBoundingClientRect(){const box=this.layoutBox();return {width:box.width*button.scale,height:box.height*button.scale};},cloneNode(){const copy=makeLabel();copy.textContent=this.textContent;Object.assign(copy.style,this.style);return copy;}});
  const label=makeLabel();
  return {button,label};
}
try {
  const quick=makeButton();
  quick.button.scale=.98; // A held press must not reduce the measured line width.
  swapShortcutLabel(quick.button,'키를 눌러주세요',true);
  await advance(40);
  swapShortcutLabel(quick.button,'저장 중',true);
  swapShortcutLabel(quick.button,'Ctrl + Alt + Shift + D',false);
  await advance(40);
  assert.equal(quick.button.classList.everAdded.has('capturing'),false);
  assert.equal(quick.button.copies[0].textContent,'단축키 지정');
  assert.equal(quick.button.copies[0].attributes['aria-hidden'],'true');
  assert.equal(quick.label.textContent,'Ctrl + Alt + Shift + D');
  assert.equal(quick.label.style.maxWidth,'none');
  assert.equal(quick.label.style.whiteSpace,'pre');
  assert.ok(parseFloat(quick.label.style.width)>=quick.label.textContent.length*7+.375);
  assert.equal(quick.label.classList.contains('is-enter-start'),false,'New copy enters during resizing, with no hidden hold');
  const targetWidth=quick.label.style.width;
  quick.button.style.width='72px'; // An intermediate surface width must not reflow the copy.
  assert.equal(quick.label.style.width,targetWidth);
  assert.equal(quick.label.layoutBox().height,18);
  await advance(370);
  assert.equal(quick.button.copies.length,0);

  const update=makeButton();
  update.button.id='check-update';
  swapShortcutLabel(update.button,'업데이트 진행 중',true);
  await advance(80);
  assert.equal(update.label.textContent,'업데이트 진행 중','Update buttons share the same motion controller');
  assert.equal(update.label.layoutBox().height,18);
  await advance(370);
  assert.equal(update.button.copies.length,0);

  const normal=makeButton();
  swapShortcutLabel(normal.button,'키를 눌러주세요',true,true);
  await advance(80);
  const outgoing=normal.button.copies[0];
  assert.equal(normal.label.style.fontWeight,'400','Prompt uses the imported JSON Medium weight after one correction');
  assert.equal(outgoing.style.fontWeight,'400','Outgoing copy keeps its previous weight');
  assert.equal(outgoing.classList.contains('is-exit'),true);
  swapShortcutLabel(normal.button,'저장 중',true);
  swapShortcutLabel(normal.button,'Alt + G',false);
  await advance(369);
  assert.equal(normal.label.textContent,'키를 눌러주세요','Do not interrupt an incoming copy');
  await advance(81);
  assert.equal(normal.label.textContent,'Alt + G');
  assert.equal(normal.button.style['--shortcut-resize-delay'],'350ms','Shrink after the wider outgoing copy has faded');
  await advance(700);
  assert.equal(normal.button.copies.length,0);

  const long=makeButton();
  swapShortcutLabel(long.button,'Ctrl + Alt + Shift + Win + Print Screen',false);
  await advance(80);
  assert.ok(long.label.layoutBox().height>18,'A combination longer than the final available width may still wrap');
  const finalHeight=long.label.layoutBox().height;
  long.button.style.width='72px';
  assert.equal(long.label.layoutBox().height,finalHeight,'Final multiline layout stays stable during resize');
  await advance(370);

  const cancelled=makeButton();
  swapShortcutLabel(cancelled.button,'키를 눌러주세요',true);
  await advance(80); cancelled.button.id=''; await advance(370);
  assert.equal(cancelled.button.copies.length,0);

  const accessible=makeButton();
  swapShortcutLabel(accessible.button,'키를 눌러주세요',true);
  await advance(80); reduced=true;
  swapShortcutLabel(accessible.button,'단축키 지정',false);
  assert.equal(accessible.button.copies.length,0);
  assert.equal(accessible.label.textContent,'단축키 지정');
  await advance(370);
  assert.equal(accessible.label.style.whiteSpace,'pre');
  assert.equal(accessible.label.layoutBox().height,18);
  console.log('Shortcut copies: unscaled fractional widths, no transient wrapping, overlapping reveal, long combinations, cancellation, and reduced motion passed.');
} finally { globalThis.setTimeout=originalTimeout; }
