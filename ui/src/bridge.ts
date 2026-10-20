import { Channel, invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import type { Snapshot } from './model';

export const desktop = isTauri();
// Vite removes this import and all simulated responses from release builds.
const preview = !desktop && import.meta.env.DEV ? await import('../dev/bridge') : null;

export type UpdateResult = { status: 'latest' | 'installed'; version: string };
export function appUpdateState(): Promise<{ version: string; completed: boolean }> {
  return preview ? preview.appUpdateState() : invoke('update_status');
}
export async function checkAppUpdate(progress: () => void): Promise<UpdateResult> {
  if (preview) return preview.checkAppUpdate(progress);
  const onProgress = new Channel<'installing'>();
  onProgress.onmessage = () => progress();
  return invoke('check_for_updates_manual', { onProgress });
}
export async function connect(onState: (state: Snapshot) => void, onOpen: (state: Snapshot) => void = onState) {
  if (preview) return preview.connect(onState);
  await listen<Snapshot>('ltac-state', event => onState(event.payload));
  await listen<Snapshot>('ltac-opened', event => onOpen(event.payload));
  onState(await invoke<Snapshot>('snapshot'));
}
export async function command(name: string, args: Record<string, unknown> = {}): Promise<Snapshot> {
  return preview ? preview.command(name, args) : invoke<Snapshot>(name, args);
}
export async function windowAction(action: 'minimize' | 'close' | 'drag') {
  if (!desktop) return;
  const window = getCurrentWindow();
  if (action === 'minimize') await window.minimize();
  else if (action === 'close') await window.close();
  else await window.startDragging();
}
