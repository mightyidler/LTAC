import type { Choice, Device } from '../src/model';

export const demoChoices: Choice[] = [0, 1].map(i => ({ id: `tv-${i}`, name: 'OLED42C4ENA', model: 'OLED42C4ENA' }));
export const demoDevices: Device[] = [0, 1].map(i => ({ id: `tv-${i}`, name: 'OLED42C4ENA', ip: `192.168.1.${21 + i}`, mac: 'A5:D3:31:F3:35:A8', pc_hdmi: null }));
