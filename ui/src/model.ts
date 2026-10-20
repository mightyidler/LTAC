export type Device = { id: string; name: string; ip: string; mac: string | null; pc_hdmi: number | null };
export type Choice = { id: string; name: string; model: string };
export type Snapshot = {
  configured: boolean; name: string; stage: string; choices: Choice[];
  devices: Device[]; automatic: boolean; keep_tv_on: boolean; shortcut_enabled: boolean; shortcut: string | null;
  busy: boolean; error: string | null;
};
export const initialState: Snapshot = { configured: false, name: '', stage: 'searching', choices: [], devices: [], automatic: true, keep_tv_on: false, shortcut_enabled: false, shortcut: null, busy: false, error: null };
