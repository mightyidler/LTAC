export type Device = { id: string; name: string; ip: string; mac: string | null };
export type Choice = { id: string; name: string; model: string };
export type Snapshot = {
  configured: boolean; name: string; stage: string; choices: Choice[];
  devices: Device[]; automatic: boolean; shortcut_enabled: boolean; shortcut: string | null;
  busy: boolean; error: string | null;
};
export const initialState: Snapshot = { configured: false, name: '', stage: 'searching', choices: [], devices: [], automatic: true, shortcut_enabled: false, shortcut: null, busy: false, error: null };
export const demoChoices: Choice[] = [0, 1].map(i => ({ id: `tv-${i}`, name: 'OLED42C4ENA', model: 'OLED42C4ENA' }));
export const demoDevices: Device[] = [0, 1].map(i => ({ id: `tv-${i}`, name: 'OLED42C4ENA', ip: `192.168.1.${21 + i}`, mac: 'A5:D3:31:F3:35:A8' }));
