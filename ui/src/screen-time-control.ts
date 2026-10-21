import { numberPopIn } from './number-pop-in';

/** Give CSS two numeric widths, including when an earlier resize is interrupted. */
export function updateScreenTimeLabel(button: HTMLButtonElement, text: string) {
  const label = button.querySelector<HTMLElement>('.t-digit-group')!;
  if (label.dataset.value === text) return;
  const previousWidth = getComputedStyle(button).width;
  const initial = label.dataset.value === undefined;
  const transition = button.style.transition;
  button.style.transition = 'none';
  button.style.width = previousWidth;
  numberPopIn(label, text);
  const style = getComputedStyle(button);
  const width = label.getBoundingClientRect().width + parseFloat(style.paddingLeft) + parseFloat(style.paddingRight)
    + parseFloat(style.borderLeftWidth) + parseFloat(style.borderRightWidth);
  if (initial) button.style.width = `${width}px`;
  void button.offsetWidth;
  button.style.transition = transition;
  button.style.width = `${width}px`;
}

/** Close the morph at the new button bounds, rather than the old label's width. */
export function fitScreenTimeClose(menu: HTMLElement, button: HTMLButtonElement) {
  followScreenTimeClose(menu);
  const box = button.getBoundingClientRect();
  const width = parseFloat(button.style.width);
  menu.style.setProperty('--screen-trigger-width', `${width}px`);
  menu.style.setProperty('--screen-trigger-height', getComputedStyle(button).height);
  menu.style.setProperty('--screen-close-x', `${box.right - width - parseFloat(menu.style.left)}px`);
  menu.style.setProperty('--screen-close-y', `${box.top - parseFloat(menu.style.top)}px`);
  copyScreenTimeLabel(menu, button);
}

const closingAnchors = new WeakMap<HTMLElement, { frame: number; update: () => void }>();

/** Keep the existing CSS morph's timeline while its coordinate origin follows
 * the live trigger. Retargeting translate on every scroll would restart easing. */
export function trackScreenTimeClose(menu: HTMLElement, button: HTMLButtonElement) {
  stopScreenTimeClose(menu);
  const origin = { left: parseFloat(menu.style.left), top: parseFloat(menu.style.top) };
  const anchor = button.getBoundingClientRect();
  const tracker = { frame: 0, update: () => {
    const current = button.getBoundingClientRect();
    const left = `${origin.left + current.right - anchor.right}px`;
    const top = `${origin.top + current.top - anchor.top}px`;
    if (menu.style.left !== left) menu.style.left = left;
    if (menu.style.top !== top) menu.style.top = top;
  } };
  closingAnchors.set(menu, tracker);
  const tick = () => {
    if (!menu.isConnected || !button.isConnected) { stopScreenTimeClose(menu); return; }
    tracker.update();
    tracker.frame = requestAnimationFrame(tick);
  };
  tracker.frame = requestAnimationFrame(tick);
}

export function followScreenTimeClose(menu: HTMLElement) { closingAnchors.get(menu)?.update(); }

export function stopScreenTimeClose(menu: HTMLElement) {
  const tracker = closingAnchors.get(menu);
  if (tracker) cancelAnimationFrame(tracker.frame);
  closingAnchors.delete(menu);
}

export function copyScreenTimeLabel(menu: HTMLElement, button: HTMLButtonElement) {
  // Both copies start in the same frame, so the handoff preserves digit motion.
  const label = button.querySelector<HTMLElement>('.t-digit-group')!;
  const plus = menu.querySelector<HTMLElement>('.t-morph-plus')!;
  // A portal does not inherit the button's typography. Match its exact metrics
  // so handing the animated digits back never changes their width or baseline.
  const style = getComputedStyle(button);
  plus.style.font = style.font;
  plus.style.letterSpacing = style.letterSpacing;
  plus.style.fontVariantNumeric = style.fontVariantNumeric;
  if (plus.querySelector<HTMLElement>('.t-digit-group')?.dataset.value !== label.dataset.value) {
    const copy = label.cloneNode(true) as HTMLElement;
    copy.removeAttribute('id'); plus.replaceChildren(copy);
  }
}

const scrollbars = new WeakMap<HTMLElement, () => void>();

/** The native scroller keeps wheel/keyboard behavior; the painted bar overlays it. */
export function mountScreenTimeScrollbar(menu: HTMLElement) {
  const scroller = menu.querySelector<HTMLElement>('[role=menu]')!;
  const track = document.createElement('div');
  track.className = 'screen-time-scrollbar'; track.setAttribute('aria-hidden', 'true');
  const thumb = document.createElement('div'); thumb.className = 'screen-time-scrollbar-thumb';
  track.append(thumb); menu.append(track);
  const events = new AbortController();
  let thumbHeight = 0;
  let drag: { pointer: number; y: number; scroll: number } | undefined;
  const metrics = () => ({ height: track.clientHeight, range: scroller.scrollHeight - scroller.clientHeight });
  const update = () => {
    const { height, range } = metrics();
    thumbHeight = Math.min(height, Math.max(24, height * scroller.clientHeight / scroller.scrollHeight));
    track.dataset.scrollable = String(range > 0);
    thumb.style.height = `${thumbHeight}px`;
    thumb.style.transform = `translateY(${range > 0 ? scroller.scrollTop / range * (height - thumbHeight) : 0}px)`;
  };
  const observer = new ResizeObserver(update); observer.observe(scroller); observer.observe(track);
  scroller.addEventListener('scroll', update, { passive: true, signal: events.signal });
  track.addEventListener('pointerdown', event => {
    if (event.button !== 0 || menu.dataset.scrollReady !== 'true') return;
    event.preventDefault();
    const { height, range } = metrics();
    if (range <= 0 || height <= thumbHeight) return;
    if (event.target !== thumb) {
      scroller.scrollTop = (event.clientY - track.getBoundingClientRect().top - thumbHeight / 2) / (height - thumbHeight) * range;
      update();
    }
    drag = { pointer: event.pointerId, y: event.clientY, scroll: scroller.scrollTop };
    track.setPointerCapture(event.pointerId); track.dataset.dragging = 'true';
  }, { signal: events.signal });
  track.addEventListener('pointermove', event => {
    if (!drag || event.pointerId !== drag.pointer) return;
    const { height, range } = metrics();
    if (height > thumbHeight) scroller.scrollTop = drag.scroll + (event.clientY - drag.y) * range / (height - thumbHeight);
    update();
  }, { signal: events.signal });
  const release = () => { drag = undefined; delete track.dataset.dragging; };
  track.addEventListener('lostpointercapture', release, { signal: events.signal });
  track.addEventListener('pointerup', release, { signal: events.signal });
  track.addEventListener('pointercancel', release, { signal: events.signal });
  scrollbars.set(menu, () => { observer.disconnect(); events.abort(); });
  update();
}

export function unmountScreenTimeScrollbar(menu: HTMLElement) {
  scrollbars.get(menu)?.(); scrollbars.delete(menu);
}
