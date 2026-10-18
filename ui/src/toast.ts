import { duration } from './motion';

export class Toast {
  private timer = 0;
  private cleanup = 0;

  constructor(private node: HTMLElement) {
    node.hidden = true;
  }

  show(message: string, kind: 'error' | 'info' = 'error') {
    clearTimeout(this.timer); clearTimeout(this.cleanup);
    this.node.classList.remove('is-open');
    this.node.hidden = false;
    this.node.inert = false;
    this.node.setAttribute('aria-hidden', 'false');
    this.node.setAttribute('role', kind === 'error' ? 'alert' : 'status');
    this.node.setAttribute('aria-live', kind === 'error' ? 'assertive' : 'polite');
    this.node.dataset.kind = kind;
    this.node.querySelector<HTMLElement>('.toast-message')!.textContent = message;
    void this.node.offsetHeight;
    this.node.classList.add('is-open');
    this.timer = window.setTimeout(() => this.hide(), 4500);
  }

  hide(immediate = false) {
    clearTimeout(this.timer); clearTimeout(this.cleanup);
    this.node.classList.remove('is-open'); this.node.inert = true;
    this.node.setAttribute('aria-hidden', 'true');
    const clear = () => { this.node.hidden = true; this.node.querySelector<HTMLElement>('.toast-message')!.textContent = ''; };
    if (immediate) clear();
    else this.cleanup = window.setTimeout(clear, duration('--toast-close', 250));
  }

}
