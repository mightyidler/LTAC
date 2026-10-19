import { duration } from './motion';

// Title transitions stay in place: blur/fade on entry, quiet fade on exit.
// The outgoing title owns its horizontal anchor until it has disappeared.
export class WindowTitle {
  private animation: Animation | null = null;
  private desired: string | null = null;
  private inset = false;
  private revision = 0;

  constructor(private node: HTMLElement) { node.textContent = ''; }

  set(text: string | null, inset = false) {
    if (text === this.desired && (!text || inset === this.inset)) return;
    const presentation = getComputedStyle(this.node);
    const opacity = presentation.opacity, transform = presentation.transform, filter = presentation.filter;
    this.animation?.cancel();
    this.animation = null;
    const revision = ++this.revision;
    this.desired = text;
    const style = getComputedStyle(this.node);
    if (text) {
      this.inset = inset;
      this.node.style.left = inset ? '48px' : '11px';
      this.node.textContent = text;
      this.node.classList.add('visible');
      const ms = duration('--stagger-dur', 500, this.node);
      if (!ms) return;
      this.animation = this.node.animate([
        { opacity: 0, transform: 'none', filter: `blur(${style.getPropertyValue('--stagger-blur').trim()})` },
        { opacity: 1, transform: 'none', filter: 'none' },
      ], { duration: ms, easing: style.getPropertyValue('--stagger-ease').trim(), fill: 'both' });
    } else {
      const ms = duration('--header-title-out-dur', 200, this.node);
      if (!ms) { this.node.classList.remove('visible'); this.node.textContent = ''; return; }
      this.animation = this.node.animate([{ opacity, transform, filter }, { opacity: 0, transform, filter }], { duration: ms, easing: style.getPropertyValue('--ease-in-out').trim(), fill: 'both' });
    }
    const animation = this.animation;
    void animation.finished.then(() => {
      if (revision !== this.revision) return;
      if (!text) { this.node.classList.remove('visible'); this.node.textContent = ''; }
      animation.cancel(); this.animation = null;
    }).catch(() => {});
  }
}
