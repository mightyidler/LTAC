const updates = new WeakMap<HTMLElement, symbol>();

/** transitions.dev 02-number-pop-in. Duplicate snapshots do not replay it. */
export function numberPopIn(group: HTMLElement, text: string) {
  if (group.dataset.value === text) return;
  const previous = group.dataset.value;
  const token = Symbol(); updates.set(group, token);
  group.dataset.value = text;
  group.setAttribute('aria-label', text);
  group.classList.remove('is-animating');
  let same = 0;
  while (previous && same < previous.length && same < text.length && previous[same] === text[same]) same++;
  const characters = Array.from(text);
  group.replaceChildren(...characters.map((character, index) => {
    const span = document.createElement('span');
    span.className = 't-digit'; span.textContent = character;
    span.setAttribute('aria-hidden', 'true');
    if (previous === undefined || index < same || character === ' ') span.dataset.same = '';
    if (index === characters.length - 2) span.dataset.stagger = '1';
    else if (index === characters.length - 1) span.dataset.stagger = '2';
    return span;
  }));
  if (previous === undefined || matchMedia('(prefers-reduced-motion: reduce)').matches) return;
  void group.offsetHeight;
  group.classList.add('is-animating');
  // Finished/cancelled animations from a previous value cannot clear a newer
  // update. Removing the class also releases the temporary compositing hint.
  void Promise.allSettled(group.getAnimations({ subtree: true }).map(animation => animation.finished)).then(() => {
    if (updates.get(group) === token) group.classList.remove('is-animating');
  });
}
