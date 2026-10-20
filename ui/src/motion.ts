const reduced = () => matchMedia('(prefers-reduced-motion: reduce)').matches;
// Production CSS minification rewrites 4000ms → 4s and 500ms → .5s.
// Custom properties retain that unit; parseFloat alone turns seconds into ms.
export function cssTimeMs(value: string, fallback: number): number {
  const time = value.trim();
  if (time === '0') return 0;
  const match = time.match(/^(\d*\.?\d+)(ms|s)$/i);
  if (!match) return fallback;
  const ms = Number(match[1]) * (match[2].toLowerCase() === 's' ? 1000 : 1);
  return Number.isFinite(ms) ? ms : fallback;
}
export const duration = (name: string, fallback: number, node: Element = document.documentElement) => reduced() ? 0 : cssTimeMs(getComputedStyle(node).getPropertyValue(name), fallback);
export const pause = (ms: number) => new Promise<void>(resolve => setTimeout(resolve, ms));
const swaps = new WeakMap<HTMLElement, symbol>();
const pendingTexts = new WeakMap<HTMLElement, string>();

export type PageMotion = 'push' | 'pop' | 'replace' | 'complete';
export function pageTransitionMode(from: string, to: string, backward: boolean): PageMotion {
  if (backward) return 'pop';
  if (from === 'connection' && to === 'home') return 'complete';
  if ((from === 'choose' || from === 'manual') && to === 'connection') return 'push';
  if ((from === 'home' && (to === 'choose' || to === 'language' || to === 'theme' || to.startsWith('edit-'))) || (from === 'choose' && to === 'manual')) return 'push';
  return 'replace';
}

const pageMotions = new WeakMap<HTMLElement, { token: symbol; animations: Animation[] }>();
// Animate the two content pages only. Window controls are outside this region.
export function transitionPages(previous: HTMLElement, next: HTMLElement, mode: PageMotion, initial: boolean) {
  const root = next.parentElement!;
  const old = pageMotions.get(root);
  const motion = { token: Symbol(), animations: [] as Animation[] };
  pageMotions.set(root, motion);
  old?.animations.forEach(animation => animation.cancel());
  for (const page of [previous, next]) page.style.zIndex = '';
  if (initial || reduced()) return;
  const animate = (node: HTMLElement, frames: Keyframe[], options: KeyframeAnimationOptions) => {
    const animation = node.animate(frames, options);
    motion.animations.push(animation);
    return animation;
  };
  const smooth = getComputedStyle(next).getPropertyValue('--ease-smooth-out').trim();
  const ms = duration('--page-navigation-dur', 350);
  if (mode === 'push' || mode === 'pop') {
    const back = mode === 'pop';
    previous.style.zIndex = back ? '2' : '1';
    next.style.zIndex = back ? '1' : '2';
    const options = { duration: ms, easing: getComputedStyle(next).getPropertyValue('--page-navigation-ease').trim() };
    animate(previous, [{ opacity: 1, transform: 'none' }, { opacity: 1, transform: `translateX(${back ? '100%' : '-30%'})` }], options);
    animate(next, [{ opacity: 1, transform: `translateX(${back ? '-30%' : '100%'})` }, { opacity: 1, transform: 'none' }], options);
    return;
  }
  const fade = duration('--page-replace-dur', 250);
  if (mode === 'replace') {
    animate(previous, [{ opacity: 1, filter: 'none' }, { opacity: 0, filter: 'blur(4px)' }], { duration: fade, easing: smooth });
    animate(next, [{ opacity: 0, filter: 'blur(4px)' }, { opacity: 1, filter: 'none' }], { duration: fade, easing: smooth });
    return;
  }
  // Keep home hidden until the completed page has fully faded out.
  next.inert = true;
  const hold = animate(next, [{ opacity: 0 }, { opacity: 0 }], { duration: fade, fill: 'forwards' });
  const exit = animate(previous, [{ opacity: 1, filter: 'none' }, { opacity: 0, filter: 'blur(6px)' }], { duration: fade, easing: smooth });
  void exit.finished.then(() => {
    if (pageMotions.get(root)?.token !== motion.token) return;
    // Home's rows have been populated by renderHome before this handoff.
    const pieces = next.querySelectorAll<HTMLElement>('.device-section > h2, .saved-device, .home-devices-empty[data-open="true"], .settings-section > h2, .settings-section > .setting-row, .settings-section > .divider, .shortcut-group > .setting-row, .shortcut-panel[data-open="true"] .shortcut-setting');
    pieces.forEach((piece, index) => animate(piece, [
      { opacity: 0, filter: 'blur(3px)', transform: 'translateY(8px)' },
      { opacity: 1, filter: 'none', transform: 'none' },
    ], { duration: duration('--duration-medium', 350), delay: Math.min(index * duration('--duration-stagger', 40), 160), easing: smooth, fill: 'backwards' }));
    hold.cancel();
    next.inert = false;
  }).catch(() => {});
}

// transitions.dev text-states-swap: keep the node and animate only its changed copy.
export async function swapText(node: HTMLElement, next: string, resize?: HTMLElement, force = false) {
  if (pendingTexts.get(node) === next || (!force && node.textContent === next && !swaps.has(node))) return;
  const token = Symbol(); swaps.set(node, token);
  pendingTexts.set(node, next);
  node.classList.add('t-text-swap', 'is-exit');
  await pause(duration('--text-swap-dur', 150, node));
  if (!node.isConnected || swaps.get(node) !== token) return;
  node.textContent = next;
  // Resize the surrounding slot while the replacement text fades back in.
  if (resize) resize.style.height = `${node.offsetHeight}px`;
  node.classList.remove('is-exit'); node.classList.add('is-enter-start');
  void node.offsetHeight;
  node.classList.remove('is-enter-start'); swaps.delete(node); pendingTexts.delete(node);
}

type ShortcutCopy = { text: string; active: boolean; emphasized: boolean; shimmer: boolean };
function syncLabelShimmer(label: HTMLElement, enabled: boolean) {
  label.classList.add('t-think-text');
  label.classList.toggle('is-thinking', enabled);
  if (enabled) label.setAttribute('data-text', label.textContent ?? '');
  else label.removeAttribute('data-text');
}
const shortcutSwaps = new WeakMap<HTMLButtonElement, { next: ShortcutCopy; running: boolean; version: number; cleanup?: () => void }>();
// CSS layout sizes exclude the button's press scale and any page transform.
const layoutSize = (node: HTMLElement) => {
  const style = getComputedStyle(node);
  return { width: parseFloat(style.width), height: parseFloat(style.height) };
};
function fitShortcutCopy(button: HTMLButtonElement, label: HTMLElement) {
  button.style.width = 'auto'; button.style.height = 'auto';
  label.style.width = 'max-content'; label.style.maxWidth = 'none'; label.style.whiteSpace = 'pre';
  const style = getComputedStyle(button);
  const width = Math.ceil(parseFloat(style.width));
  const inset = parseFloat(style.paddingLeft) + parseFloat(style.paddingRight) + parseFloat(style.borderLeftWidth) + parseFloat(style.borderRightWidth);
  const natural = layoutSize(label).width;
  const available = width - inset;
  // Only a combination too long for its final button may wrap. Short status
  // labels stay unbreakable at every intermediate surface width.
  label.style.width = `${Math.min(Math.ceil(natural), available)}px`;
  label.style.whiteSpace = natural <= available ? 'pre' : 'normal';
  return { width, height: Math.ceil(layoutSize(button).height) };
}
// Text states swap with independently measured copies: resizing the button
// never reflows either label, and the two text phases overlap without a gap.
export function swapButtonLabel(button: HTMLButtonElement, text: string, active: boolean, emphasized = false, shimmer = false) {
  // Status labels may opt into a separate weight without changing normal copy.
  const buttonStyle = getComputedStyle(button);
  const emphasisWeight = buttonStyle.getPropertyValue('--font-weight-status-emphasis').trim() || buttonStyle.getPropertyValue('--font-weight-copy').trim() || buttonStyle.fontWeight;
  const mountedId = button.id;
  const label = button.querySelector<HTMLElement>('.t-text-swap')!;
  let state = shortcutSwaps.get(button);
  if (!state) {
    state = { next: { text, active, emphasized, shimmer }, running: false, version: 0 }; shortcutSwaps.set(button, state);
    fitShortcutCopy(button, label);
  }
  state.next = { text, active, emphasized, shimmer };
  if (reduced()) {
    state.version++; state.running = false;
    state.cleanup?.(); state.cleanup = undefined;
    label.textContent = text; label.classList.remove('is-exit', 'is-enter-start');
    syncLabelShimmer(label, shimmer);
    label.style.fontWeight = emphasized ? emphasisWeight : '';
    label.style.transitionDelay = '';
    button.classList.toggle('capturing', active);
    button.classList.toggle('status-active', active && shimmer); button.style.width = ''; button.style.height = '';
    fitShortcutCopy(button, label);
    return;
  }
  if (state.running) return;
  const changed = () => label.textContent !== state.next.text || button.classList.contains('capturing') !== state.next.active || label.style.fontWeight !== (state.next.emphasized ? emphasisWeight : '') || label.classList.contains('is-thinking') !== state.next.shimmer;
  if (!changed()) return;
  state.running = true;
  const version = ++state.version;
  const current = () => state.version === version && button.isConnected && mountedId !== '' && button.id === mountedId;
  void (async () => {
    try {
      while (current() && changed()) {
        // Coalesce instantaneous saves before committing to the active surface.
        await pause(duration('--duration-micro', 80, button));
        if (!current()) return;
        if (!changed()) break;
        const target = state.next;
        const previous = layoutSize(button);
        const painted = getComputedStyle(button);
        const material = { backgroundColor: painted.backgroundColor, boxShadow: painted.boxShadow, color: painted.color };
        const inlineMaterial = { backgroundColor: button.style.backgroundColor, boxShadow: button.style.boxShadow, color: button.style.color };
        const previousLabel = layoutSize(label);
        const outgoing = label.cloneNode(true) as HTMLElement;
        outgoing.removeAttribute('id'); outgoing.setAttribute('aria-hidden', 'true');
        outgoing.classList.add('shortcut-copy-outgoing', 't-think-text');
        outgoing.style.fontWeight = getComputedStyle(label).fontWeight;
        outgoing.style.color = getComputedStyle(label).color;
        outgoing.style.width = `${previousLabel.width}px`; outgoing.style.maxWidth = 'none';
        outgoing.style.transitionDelay = '0ms';
        const outDuration = duration('--think-swap', 150, button);
        outgoing.style.setProperty('--think-swap', `${outDuration}ms`);
        const transition = button.style.transition;
        button.style.transition = 'none';
        button.classList.toggle('capturing', target.active);
        button.classList.toggle('status-active', target.active && target.shimmer);
        label.textContent = target.text;
        syncLabelShimmer(label, target.shimmer);
        label.style.fontWeight = target.emphasized ? emphasisWeight : '';
        const next = fitShortcutCopy(button, label);
        label.classList.add('is-enter-start');
        label.style.transitionDelay = `${duration('--think-gap', 50, button)}ms`;
        button.append(outgoing);
        button.style.width = `${previous.width}px`; button.style.height = `${previous.height}px`;
        const shrinking = next.width < previous.width || next.height < previous.height;
        button.style.setProperty('--shortcut-resize-delay', `${shrinking ? outDuration : 0}ms`);
        // Measurement may flush layout, but must never commit an intermediate
        // material. Restore the painted surface before starting its transition.
        for (const key of ['backgroundColor', 'boxShadow', 'color'] as const) {
          if (material[key]) button.style[key] = material[key];
        }
        void button.offsetWidth;
        button.style.transition = transition;
        for (const key of ['backgroundColor', 'boxShadow', 'color'] as const) {
          button.style[key] = inlineMaterial[key] || '';
        }
        button.style.width = `${next.width}px`; button.style.height = `${next.height}px`;
        const cleanup = () => {
          outgoing.remove(); label.classList.remove('is-exit', 'is-enter-start');
          label.style.transitionDelay = ''; button.style.removeProperty('--shortcut-resize-delay');
        };
        state.cleanup = cleanup;
        outgoing.classList.add('is-exit');
        label.classList.remove('is-enter-start');
        const surface = button.getAnimations().filter(animation => {
          const property = (animation as CSSTransition).transitionProperty;
          return property === 'width' || property === 'height' || property === 'background-color' || property === 'box-shadow' || property === 'color' || property?.startsWith('border-');
        });
        await Promise.all([
          pause(Math.max(outDuration, duration('--think-swap', 150, label) + duration('--think-gap', 50, button))),
          surface.length ? Promise.allSettled(surface.map(animation => animation.finished)) : pause(duration('--shortcut-surface-dur', 350, button) + (shrinking ? outDuration : 0)),
        ]);
        if (!current()) return;
        cleanup();
        if (state.cleanup === cleanup) state.cleanup = undefined;
      }
    } finally {
      if (state.version === version) { state.cleanup?.(); state.cleanup = undefined; state.running = false; label.classList.remove('is-exit', 'is-enter-start'); }
    }
  })();
}
export const swapShortcutLabel = swapButtonLabel;

// Stack each complete row down into its resting position, with a short beat.
// Transform only: normal-flow geometry stays fixed throughout the entrance.
export function revealChoices(container: HTMLElement) {
  for (const [index, row] of Array.from(container.children).entries()) {
    const icon = row.querySelector<SVGElement>('.device-choice > svg');
    const name = row.querySelector<HTMLElement>('.choice-name');
    for (const node of [row, icon, name]) node?.getAnimations().forEach(animation => animation.cancel());
    if (reduced()) continue;
    const ease = getComputedStyle(row).getPropertyValue('--ease-smooth-out').trim();
    const beat = duration('--duration-stagger', 40);
    const delay = Math.min(index * beat, 240);
    const distance = getComputedStyle(row).getPropertyValue('--distance-base').trim();
    row.animate([
      { opacity: 0, transform: `translateY(calc(${distance} * -1))` },
      { opacity: 1, transform: 'none' },
    ], { duration: duration('--duration-fast', 250), delay, easing: ease, fill: 'backwards' });
  }
}

const exiting = new WeakMap<HTMLElement, Animation>();
export function syncRows<T>(container: HTMLElement, items: T[], key: (item: T) => string, create: (item: T) => HTMLElement, update: (row: HTMLElement, item: T) => void, animate = true) {
  const existing = new Map(Array.from(container.children, node => [(node as HTMLElement).dataset.key!, node as HTMLElement]));
  const desired = new Set(items.map(key));
  for (const [id, row] of existing) {
    if (desired.has(id) || exiting.has(row)) continue;
    row.inert = true;
    if (!animate || reduced()) { row.remove(); continue; }
    // A collapsing row moves its neighbors continuously, without replacing the page.
    const style = getComputedStyle(row);
    row.style.overflow = 'hidden';
    const animation = row.animate([
      { height: `${row.getBoundingClientRect().height}px`, paddingTop: style.paddingTop, paddingBottom: style.paddingBottom, marginTop: style.marginTop, marginBottom: style.marginBottom, opacity: 1, filter: 'blur(0px)' },
      { height: '0px', paddingTop: '0px', paddingBottom: '0px', marginTop: '0px', marginBottom: '0px', opacity: 0, filter: 'blur(2px)' },
    ], { duration: duration('--acc-collapse', 250), easing: style.getPropertyValue('--ease-smooth-out').trim(), fill: 'forwards' });
    exiting.set(row, animation);
    void animation.finished.then(() => { if (exiting.get(row) === animation) { exiting.delete(row); row.remove(); } }).catch(() => {});
  }
  items.forEach((item, index) => {
    const id = key(item);
    let row = existing.get(id);
    if (row) { exiting.get(row)?.cancel(); exiting.delete(row); row.style.overflow = ''; row.inert = false; }
    else {
      row = create(item); row.dataset.key = id; container.append(row);
      if (animate && !reduced()) row.animate([
        { opacity: 0, transform: 'translateY(8px)', filter: 'blur(2px)' },
        { opacity: 1, transform: 'translateY(0)', filter: 'blur(0px)' },
      ], { duration: duration('--duration-fast', 250), delay: Math.min(index * duration('--duration-stagger', 40), 240), easing: getComputedStyle(row).getPropertyValue('--ease-smooth-out').trim(), fill: 'backwards' });
    }
    update(row, item);
  });
}
