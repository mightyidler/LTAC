import { t } from './i18n';
import type { Device } from './model';
import { icons } from './icons';

export class HdmiMenu {
  readonly menu: HTMLElement;
  readonly trigger: HTMLButtonElement;
  private readonly choices: HTMLButtonElement[];
  private port: number | null = null;
  private pending = false;

  constructor(
    private readonly anchor: HTMLElement,
    toggle: (menu: HTMLElement, trigger: HTMLButtonElement) => void,
    choose: (port: number) => Promise<boolean>,
    dismiss: (menu: HTMLElement) => void,
  ) {
    anchor.innerHTML = `<div class="device-menu hdmi-menu" data-open="false"><div class="t-dropdown hdmi-options" data-origin="top-left" role="menu" inert>${[1, 2, 3, 4].map(port => `<button role="menuitemradio" data-port="${port}" aria-checked="false" tabindex="-1">${icons.portHdmi}<span>HDMI ${port}</span><span class="hdmi-check">${icons.checkSmall}</span></button>`).join('')}</div><button class="hdmi-button" aria-haspopup="menu" aria-expanded="false" tabindex="-1"><span class="hdmi-trigger-content"><span class="hdmi-label"></span>${icons.chevronDown}</span></button></div>`;
    this.menu = anchor.querySelector('.hdmi-menu')!;
    this.trigger = anchor.querySelector('.hdmi-button')!;
    this.choices = [...anchor.querySelectorAll<HTMLButtonElement>('[role=menuitemradio]')];
    this.trigger.onclick = event => {
      event.stopPropagation();
      if (this.pending) return;
      this.markSelected();
      toggle(this.menu, this.trigger);
    };
    this.trigger.onkeydown = event => {
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        event.preventDefault();
        if (this.menu.dataset.open !== 'true') this.trigger.click();
      }
    };
    for (const item of this.choices) item.onclick = async event => {
      event.stopPropagation();
      if (this.pending) return;
      const port = Number(item.dataset.port);
      dismiss(this.menu);
      if (port === this.port) return;
      this.pending = true;
      try { await choose(port); }
      finally { this.pending = false; }
    };
  }

  update(device: Device) {
    this.port = device.pc_hdmi ?? null;
    const label = this.port === null ? t('HDMI 선택') : `HDMI ${this.port}`;
    this.trigger.querySelector('.hdmi-label')!.textContent = label;
    this.trigger.setAttribute('aria-label', `${device.name}: ${label}`);
    this.menu.querySelector('[role=menu]')!.setAttribute('aria-label', `${device.name}: ${t('연결 HDMI')}`);
    // Keep the trigger's intrinsic width independent of the floating menu.
    this.anchor.style.setProperty('--hdmi-trigger-width', getComputedStyle(this.trigger.querySelector('.hdmi-trigger-content')!).width);
    // Keep the outgoing selection still while closing. The next opening reads
    // the committed value, including a failed save's unchanged selection.
    if (!this.pending && this.menu.dataset.open === 'true') this.markSelected();
  }

  private markSelected() {
    this.choices.forEach(item => {
      const selected = Number(item.dataset.port) === this.port;
      item.setAttribute('aria-checked', String(selected));
    });
  }
}
