import { initialState, type Snapshot } from '../src/model';
import { demoChoices } from './fixtures';
import type { UpdateResult } from '../src/bridge';

export const previewOptions = { outcome: 'success' as 'success' | 'failure', delay: 2400, count: 2, update: 'available' as 'available' | 'latest' | 'error' };
let previewVersion = __APP_VERSION__;
export function appUpdateState(): Promise<{ version: string; completed: boolean }> {
  return Promise.resolve({ version: previewVersion, completed: false });
}
export async function checkAppUpdate(progress: () => void): Promise<UpdateResult> {
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
export async function connect(onState: (state: Snapshot) => void) {
  onState(preview);
}
export function setPreview(state: Partial<Snapshot>) { operation++; preview = { ...preview, ...state }; return preview; }
export async function command(name: string, args: Record<string, unknown> = {}): Promise<Snapshot> {
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
      const device = { id: String(args.deviceId ?? args.ip), name: choice?.name ?? String(args.ip), ip: String(args.ip ?? '192.168.1.21'), mac: args.mac ? String(args.mac) : 'A5:D3:31:F3:35:A8', pc_hdmi: null };
      preview = { ...preview, configured: true, stage: 'ready', name: device.name, devices: [...preview.devices.filter(d => d.id !== device.id), device] };
    }
  } else if (name === 'set_automatic') preview.automatic = Boolean(args.enabled);
  else if (name === 'set_keep_tv_on') preview.keep_tv_on = Boolean(args.enabled);
  else if (name === 'set_device_hdmi') preview.devices = preview.devices.map(d => d.id === args.id ? { ...d, pc_hdmi: Number(args.pcHdmi) } : d);
  else if (name === 'set_shortcut') { preview.shortcut_enabled = Boolean(args.enabled); preview.shortcut = args.shortcut ? String(args.shortcut) : null; }
  else if (name === 'set_screen_protection') { preview.screen_enabled = Boolean(args.enabled); preview.screen_timeout_seconds = Number(args.timeoutSeconds); preview.screen_shortcut_enabled = Boolean(args.shortcutEnabled); preview.screen_shortcut = args.shortcut ? String(args.shortcut) : null; }
  else if (name === 'edit_device') preview.devices = preview.devices.map(d => d.id === args.id ? { ...d, ip: String(args.ip), mac: args.mac ? String(args.mac) : null } : d);
  else if (name === 'remove_device') { preview.devices = preview.devices.filter(d => d.id !== args.id); preview.configured = preview.devices.length > 0; preview.stage = 'ready'; }
  else if (name === 'cancel_connection') preview.stage = preview.configured ? 'ready' : 'choose';
  return { ...preview, error: null, busy: false };
}
