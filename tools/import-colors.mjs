import { readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';

const sourcePath = process.argv[2];
if (!sourcePath) throw new Error('Usage: node tools/import-colors.mjs <original-design.json-or-txt>');
const raw = readFileSync(sourcePath, 'utf8').replace(/^\uFEFF/, '');
const design = JSON.parse(raw);
const sections = Object.fromEntries(['dark', 'light'].map(theme => [theme, design.nodes.find(node => node.name === `LTAC-${theme === 'dark' ? 'Dark' : 'Light'}`)]));
function from(theme, component, property) {
  let found;
  function visit(node, path = []) {
    const next = [...path, node.name ?? node.use];
    if (!found && node.use === component && property in (node.props ?? {})) found = { value: node.props[property].toLowerCase(), source: `${next.join('/')}/props/${property}` };
    for (const child of node.children ?? []) visit(child, next);
  }
  visit(sections[theme]);
  if (!found || !/^#[\da-f]{6}([\da-f]{2})?$/.test(found.value)) throw new Error(`Missing exact source color: ${theme}/${component}/${property}`);
  return found;
}
const roles = {};
function sourceRole(name, component, property) {
  roles[name] = Object.fromEntries(['dark', 'light'].map(theme => [theme, from(theme, component, property)]));
}
sourceRole('--bg-canvas', 'Component', 'x_fill');
sourceRole('--bg-raised', 'Component2', 'remove_folder_button_container_fill');
sourceRole('--bg-overlay', 'Component3', 'frame_97_fill');
sourceRole('--bg-header', 'Component', 'header_icons_fill');
sourceRole('--bg-scrim', 'Component', 'frame_9_fill');
sourceRole('--text-primary', 'Component2', 'monitor_folder_label_color');
sourceRole('--text-secondary', 'Component2', 'monitor_folder_description_color');
sourceRole('--text-label', 'Component2', 'x_color');
sourceRole('--text-field-label', 'Component7', 'ip_color');
sourceRole('--text-header', 'Component', 'header_color');
sourceRole('--text-control', 'Component2', 'display_language_label_color');
sourceRole('--text-muted', 'Component5', 'frame_10_frame_12_x_color');
sourceRole('--text-placeholder', 'Component7', '00_color');
sourceRole('--text-notice', 'Frame', 'frame_6_x_color');
sourceRole('--text-emphasis', 'Component5', 'frame_12_x_color');
sourceRole('--text-inverse', 'Component', 'frame_9_x_color');
sourceRole('--icon-secondary', 'Component', 'vector_fill');
sourceRole('--border-subtle', 'Component2', 'divider_fill');
sourceRole('--surface-subtle', 'Component2', 'frame_33_fill');
sourceRole('--surface-control', 'Component', 'icon_container_fill');
sourceRole('--interaction-disabled', 'Component', 'frame_9_frame_9_fill');
sourceRole('--interaction-primary', 'Component4', 'frame_8_fill');
sourceRole('--separator-color', 'Component7', 'ellipse_12_fill');
sourceRole('--input-bg', 'Component7', 'frame_18_fill');
sourceRole('--input-border', 'Component7', 'frame_18_stroke');
sourceRole('--input-border-focus', 'Component7', 'frame_27_stroke');
sourceRole('--shortcut-bg', 'Component2', 'frame_88_fill');

// Explicit user changes and existing motion/feedback values absent from the
// supplied frames. These are separate from imported source colors.
const overrides = {
  '--border-window': ['#383838', '#e5e5e5'], // User: opaque light window border.
  '--border-overlay': ['#ffffff1a', '#0000001a'],
  '--surface-thumb': ['#ffffff', '#ffffff'],
  '--interaction-hover': ['#ffffff1f', '#0000000f'],
  '--interaction-hover-subtle': ['#ffffff08', '#0000000a'], // User: light list hover 4%.
  '--interaction-active': ['#ffffff29', '#00000014'],
  '--interaction-primary-hover': ['#e5e5e5', '#383838'],
  '--status-success': ['#31d158', '#34c759'],
  '--status-error': ['#ff4245', '#ff383c'],
  '--status-accent': ['#ff375f', '#ff375f'],
  '--caret-color': ['#d9d9d9', '#000000'],
  '--scrollbar': ['#ffffff25', '#00000025'],
  '--scrollbar-hover': ['#ffffff40', '#00000040'],
  '--shadow-subtle': ['#00000014', '#00000014'],
  '--shadow-menu': ['#00000014', '#0000000a'],
  '--shadow-raised': ['#00000033', '#00000033'],
  '--text-on-primary-muted': ['#1d1d1da6', '#f6f6f6a6'], // Existing shimmer base.
};
for (const [name, values] of Object.entries(overrides)) roles[name] = Object.fromEntries(['dark', 'light'].map((theme, i) => [theme, { value: values[i], source: 'explicit user change / existing interaction token' }]));

const baseNames = {
  '#ffffff': 'white', '#000000': 'black', '#1d1d1d': 'ink', '#2f2f2f': 'gray-850',
  '#383838': 'gray-700', '#f6f6f6': 'canvas-light', '#eeeeee': 'gray-150',
  '#e5e5e5': 'gray-200', '#d9d9d9': 'gray-300', '#31d158': 'green-dark',
  '#34c759': 'green-light', '#ff4245': 'red-dark', '#ff383c': 'red-light',
  '#ff375f': 'pink',
  '#444444': 'preview-neutral', '#505050': 'preview-checker', '#101010': 'preview-dark',
};
const primitives = new Map();
function primitive(value) {
  const base = value.slice(0, 7), alpha = value.slice(7);
  const name = `--p-${baseNames[base] ?? base.slice(1)}${alpha ? `-alpha-${alpha}` : ''}`;
  primitives.set(name, value);
  return `var(${name})`;
}
for (const pair of Object.values(roles)) for (const theme of ['dark', 'light']) primitive(pair[theme].value);
for (const color of ['#444444', '#505050', '#eeeeee', '#101010']) primitive(color);
const aliases = {
  '--window-bg': '--bg-canvas', '--window-border': '--border-window',
  '--button-bg': '--surface-control', '--button-primary-bg': '--interaction-primary', '--button-primary-text': '--text-inverse',
  '--input-placeholder': '--text-placeholder', '--toggle-off': '--interaction-disabled', '--toggle-on': '--status-success', '--toggle-thumb': '--surface-thumb',
  '--menu-bg': '--bg-raised', '--menu-overlay': '--bg-overlay', '--indicator-color': '--text-primary', '--indicator-error': '--status-error',
  '--toast-bg': '--bg-overlay', '--toast-text': '--text-primary', '--toast-border': '--border-subtle',
  '--text-preference-value': '--text-label', '--text-version': '--text-control', '--p-preview-light': '--p-gray-150',
};
const declarations = theme => Object.entries(roles).map(([name, pair]) => `  ${name}: ${primitive(pair[theme].value)}; /* ${pair[theme].value} */`).join('\n');
const dark = declarations('dark'), light = declarations('light');
writeFileSync(new URL('../ui/src/tokens.css', import.meta.url), `/* Generated from the original JSON by tools/import-colors.mjs.
   Preserve the supplied HEX alpha bytes; do not infer light colors from dark roles. */
:root {
  /* Primitive palette: deduplicated, exact source pigments. */
${[...primitives].map(([name, value]) => `  ${name}: ${value};`).join('\n')}

  /* Semantic roles: original dark palette. */
${dark}

  /* Component aliases. */
${Object.entries(aliases).map(([name, target]) => `  ${name}: var(${target});`).join('\n')}
}
:root[data-theme="light"] {
${light}
  color-scheme: light;
}
:root[data-theme="dark"] { color-scheme: dark; }
`);
writeFileSync(new URL('../ui/src/colors.json', import.meta.url), JSON.stringify({ sourceSha256: createHash('sha256').update(raw).digest('hex'), roles, aliases }, null, 2) + '\n');
console.log('Imported exact dark/light color roles and source paths from the supplied JSON.');
