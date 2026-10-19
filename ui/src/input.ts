import { t, messages } from './i18n.ts';
export function cleanSegment(kind: 'ip' | 'mac', text: string) {
  return text.replace(kind === 'ip' ? /\D/g : /[^a-z0-9]/gi, '').toUpperCase();
}

// Keep English letters visible while editing; validate hexadecimal only at submit.
export function validMacSegments(parts: string[]) {
  return parts.every(part => !part) || (parts.every(part => /^[\dA-F]{2}$/.test(part)) && parts.some(part => part !== '00'));
}

export function shortcutFromEvent(event: Pick<KeyboardEvent, 'code' | 'key' | 'ctrlKey' | 'altKey' | 'shiftKey' | 'metaKey'>) {
  if (['Control', 'Shift', 'Alt', 'Meta'].includes(event.key)) return null;
  const key = event.code.replace(/^Key|^Digit/, '');
  if (!/^[A-Z0-9]$|^F([1-9]|1\d|2[0-4])$|^Numpad(\d|Add|Subtract|Multiply|Divide|Decimal|Enter)$/.test(key) && !shortcutKeys.has(key)) throw new Error(t("다른 키 사용"));
  return [event.ctrlKey ? 'Ctrl' : '', event.altKey ? 'Alt' : '', event.shiftKey ? 'Shift' : '', event.metaKey ? 'Win' : '', key].filter(Boolean).join('+');
}

const shortcutKeys = new Set(['Space', 'Enter', 'Tab', 'Escape', 'Backspace', 'Delete', 'Insert', 'Home', 'End', 'PageUp', 'PageDown', 'ArrowLeft', 'ArrowUp', 'ArrowRight', 'ArrowDown', 'CapsLock', 'NumLock', 'ScrollLock', 'Pause', 'PrintScreen', 'Backquote', 'Minus', 'Equal', 'BracketLeft', 'BracketRight', 'Backslash', 'IntlBackslash', 'Semicolon', 'Quote', 'Comma', 'Period', 'Slash', 'AudioVolumeMute', 'AudioVolumeDown', 'AudioVolumeUp', 'MediaTrackNext', 'MediaTrackPrevious', 'MediaStop', 'MediaPlayPause']);
const keyLabels: Record<string, string> = { ArrowLeft: '←', ArrowRight: '→', ArrowUp: '↑', ArrowDown: '↓', Backquote: '`', Minus: '−', Equal: '=', BracketLeft: '[', BracketRight: ']', Backslash: '\\', IntlBackslash: '\\', Semicolon: ';', Quote: "'", Comma: ',', Period: '.', Slash: '/', Escape: 'Esc', NumpadAdd: 'Num +', NumpadSubtract: 'Num −', NumpadMultiply: 'Num ×', NumpadDivide: 'Num ÷', NumpadDecimal: 'Num .', NumpadEnter: 'Num Enter', AudioVolumeMute: "음소거", AudioVolumeDown: "음량 −", AudioVolumeUp: "음량 +", MediaTrackNext: "다음 곡", MediaTrackPrevious: "이전 곡", MediaStop: "정지", MediaPlayPause: "재생/일시정지" };
export function shortcutLabel(shortcut: string) {
  return shortcut.split('+').map(key => localizeKey(keyLabels[key] ?? key).replace(/^Numpad(?=\d$)/, 'Num ')).join(' + ');
}

function localizeKey(key: string) { return key in messages ? t(key as keyof typeof messages) : key; }
