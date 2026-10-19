import { Channel, invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { demoChoices, initialState, type Snapshot } from './model';

export const desktop = isTauri();
export const previewOptions = { outcome: 'success' as 'success' | 'failure', delay: 2400, count: 2, update: 'available' as 'available' | 'latest' | 'error' };
let previewVersion = __APP_VERSION__;
export type UpdateResult = { status: 'latest' | 'installed'; version: string };
export function appUpdateState(): Promise<{ version: string; completed: boolean }> {
  return desktop ? invoke('update_status') : Promise.resolve({ version: previewVersion, completed: false });
}
export async function checkAppUpdate(progress: () => void): Promise<UpdateResult> {
  if (desktop) {
    const onProgress = new Channel<'installing'>();
    onProgress.onmessage = () => progress();
    return invoke('check_for_updates_manual', { onProgress });
  }
  const outcome = previewOptions.update;
  await pause(previewOptions.delay);
  if (outcome === 'error') throw new Error('업데이트를 확인하지 못했어요. 다시 시도해주세요.');
  if (outcome === 'latest') return { status: 'latest', version: previewVersion };
  progress();
  await pause(previewOptions.delay);
  const parts = previewVersion.split('.').map(Number); parts[2]++;
  previewVersion = parts.join('.');
  return { status: 'installed', version: previewVersion };
}
let preview: Snapshot = { ...initialState, stage: 'choose', choices: demoChoices, shortcut_enabled: true };
let operation = 0;
const pause = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));
export async function connect(onState: (state: Snapshot) => void, onOpen: (state: Snapshot) => void = onState) {
  if (desktop) {
    await listen<Snapshot>('ltac-state', event => onState(event.payload));
    await listen<Snapshot>('ltac-opened', event => onOpen(event.payload));
    onState(await invoke<Snapshot>('snapshot'));
  } else onState(preview);
}
export function setPreview(state: Partial<Snapshot>) { operation++; preview = { ...preview, ...state }; return preview; }
export async function command(name: string, args: Record<string, unknown> = {}): Promise<Snapshot> {
  if (desktop) return invoke<Snapshot>(name, args);
  const token = ++operation;
  if (name === 'discover_tvs') {
    await pause(previewOptions.delay);
    if (token !== operation) return preview;
    preview = { ...preview, stage: previewOptions.count ? 'choose' : 'not_found', choices: Array.from({ length: previewOptions.count }, (_, i) => ({ ...demoChoices[0], id: `tv-${i}` })) };
  } else if (name === 'pair_tv') {
    await pause(previewOptions.delay);
    if (token !== operation) return preview;
    if (previewOptions.outcome === 'failure') preview = { ...preview, stage: 'pair_error' };
    else {
      const choice = preview.choices.find(c => c.id === args.deviceId);
      const device = { id: String(args.deviceId ?? args.ip), name: choice?.name ?? String(args.ip), ip: String(args.ip ?? '192.168.1.21'), mac: args.mac ? String(args.mac) : 'A5:D3:31:F3:35:A8' };
      preview = { ...preview, configured: true, stage: 'ready', name: device.name, devices: [...preview.devices.filter(d => d.id !== device.id), device] };
    }
  } else if (name === 'set_automatic') preview.automatic = Boolean(args.enabled);
  else if (name === 'set_shortcut') { preview.shortcut_enabled = Boolean(args.enabled); preview.shortcut = args.shortcut ? String(args.shortcut) : null; }
  else if (name === 'edit_device') preview.devices = preview.devices.map(d => d.id === args.id ? { ...d, ip: String(args.ip), mac: args.mac ? String(args.mac) : null } : d);
  else if (name === 'remove_device') { preview.devices = preview.devices.filter(d => d.id !== args.id); preview.configured = preview.devices.length > 0; preview.stage = 'ready'; }
  else if (name === 'cancel_connection') preview.stage = preview.configured ? 'ready' : 'choose';
  return { ...preview, error: null, busy: false };
}
export async function windowAction(action: 'minimize' | 'close' | 'drag') {
  if (!desktop) return;
  const window = getCurrentWindow();
  if (action === 'minimize') await window.minimize();
  else if (action === 'close') await window.close();
  else await window.startDragging();
}
