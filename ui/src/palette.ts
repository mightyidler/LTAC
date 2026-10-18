// Use the reference's exact 0–255 RGB numbers directly in sRGB.
// Its P3 origin is reference information, not an output-space conversion.
export const palette = {
  red: [255, 66, 69],
  orange: [255, 146, 48],
  yellow: [255, 214, 0],
  green: [48, 209, 88],
  mint: [0, 218, 195],
  teal: [0, 210, 224],
  cyan: [60, 211, 254],
  blue: [0, 145, 255],
  indigo: [109, 124, 255],
  purple: [219, 52, 242],
  pink: [255, 55, 95],
  brown: [183, 138, 102],
} as const;

export function applyPalette(root: HTMLElement) {
  for (const [name, rgb] of Object.entries(palette)) {
    root.style.setProperty(`--color-${name}`, `rgb(${rgb.join(' ')})`);
  }
}
