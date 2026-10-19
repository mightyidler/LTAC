# UI themes and languages

`ui/src/tokens.css` is the single color source. App styles, shared motion styles, SVGs (`currentColor`) and the WebGL connection indicator all consume it. Do not put hex/RGB values into a component stylesheet or renderer.

## Token hierarchy

1. **Primitive** `--p-*`: pigments, including the requested dark green `#31D158` and light green `#34C759`. Literal colors belong here.
2. **Semantic**: `--bg-*`, `--text-*`, `--border-*`, `--surface-*`, `--interaction-*`, `--status-*`, caret, scrollbar and shadow roles. Theme overrides change roles without changing component rules. Identical values in different roles (for example a divider and a subtle surface) remain independent so future themes can distinguish them.
3. **Component**: `--window-*`, `--button-*`, `--input-*`, `--toggle-*`, `--menu-*`, `--indicator-*` reference semantic roles. The toggle thumb intentionally stays white in both modes.

For a new component, first reuse an existing semantic role. Add a component alias only when it represents a meaningful independent styling contract. Add a primitive only when no existing pigment fits the design. Never read a primitive from application TypeScript. The WebGL indicator reads its component tokens and refreshes on `ltac-theme-change`, including settled/reduced-motion results.

Hover, active, keyboard focus and disabled states use `--interaction-*` roles. Small enabled text is checked at 4.5:1 in the browser regression. Light secondary text uses 65% ink; the dark empty-device notice uses 46% white (the previous 44% measured 4.27:1). Input placeholders reuse the readable muted-text role, including the focused placeholder proxy. Other dark pigments and original layout remain preserved, apart from the requested green. Color mixtures use sRGB.

Typography is regenerated directly from the original attachment using `node tools/import-typography.mjs <source-json-or-txt>`. The importer resolves each original text node's font style (omitted style means Regular), then subtracts 100 **once**: Regular 400 → 300, Medium 500 → 400, SemiBold 600 → 500. It never reads existing CSS as an input. Re-importing the same JSON produces identical output.

`ui/src/typography.css` contains the resulting tokens and 26 selector rules; `ui/src/typography.json` records the source SHA-256, original node path/style/weight, and final weight for every rule. This generated stylesheet owns the weights for all text represented in the supplied design: page/header titles, lists, empty states, fields, connection states, device details, settings, shortcuts, menus, version/update, and language/theme options. Menu labels are Medium 500 → 400; the previous working-copy exception of 500 has been removed. Shortcut/update text animations read the imported final weight rather than assigning a different 500 while active. Text absent from the supplied JSON, such as toast copy, keeps its existing styling. No sizes, spacing, colors, or motion are changed by the import.

The browser regression compares computed weights to this imported manifest across all three languages and both themes, including shortcut capture and menu states. The shortcut motion regression checks that animated prompt text also stays at 400.

Browser review corrections: settings section headers have no vertical padding. General settings use a separate value/chevron button; the label and full row are not interactive. Language/theme titles live in the window header, and their list rows share discovery's 12px vertical / 8px horizontal padding with inset dividers. Light hover uses 6% black for controls and 4% for lists (dark: 12% white and 3%); pressed controls use 8% black (dark: 16%). Error red is `#FF383C` in light and `#FF4245` in dark throughout the UI and WebGL indicator.

Use the attachment's `nodes` → `LTAC-Light` → `Component3.props` as the source for the light device menu: closed surface `#EEEEEE`, open surface `#FFFFFF99`, border and hovered item `#0000000A`, text `#000000`. The app's light canvas is `#F6F6F6`, with black primary text. These replace the earlier guessed white canvas, ink text, gray menu and 85% overlay.

The updated Frame 97 specification sets both menu themes to 4px outer padding, 23px outer radius, zero row gap, and 10px item padding with 18px item radius. Icon/text gap stays 8px, rows left-align their contents per the user's review, and each row is 36px high (80px panel total). The menu has a 140px minimum in every language and expands for longer labels. Opening measures the actual text/icon/gap/padding widths, retaining the existing numeric width transition; there are no locale-specific fixed widths. The drop shadow is 0px/4px/8px, with `#0000000A` in light; dark keeps its existing shadow pigment. Blur remains 24px. Dark background, border, item hover, and text pigments are unchanged.

`WindowTitle` uses an in-place blur/fade on entry and a 200ms quiet exit. Titles have no directional travel. It keeps its own horizontal anchor until the old text has faded away; the Back → Add icon change cannot move that text. New navigation cancels the previous title animation, and reduced motion changes the title immediately. Browser regression checks the entry keyframes have no translation, samples the outgoing title each frame for both preference pages, and exercises rapid navigation.

Toast review: plain text with no leading dot or icon. The toast aligns with the page content at 16px from the window edge, uses an 8px corner radius, 10px vertical / 12px horizontal padding, and 13px/20px text at weight 400. `--toast-bg`, `--toast-text`, and `--toast-border` reuse the existing floating-menu semantic roles. The previous separate toast pigments and heavy shadow were removed. Announcement roles, timeout, and existing motion remain unchanged.

Update checking/installing labels opt into the transitions.dev `t-shimmer` recipe (2s linear, 400% band). The text swap commits the new copy and its `data-text` mask together, retaining the old copy's mask during its existing exit. Only the update button opts in; terminal states remove the effect. Its base/highlight colors derive from the existing active-button text role in each theme. Reduced motion disables the sweep. Font weight, button geometry, and other transitions are unchanged.

## Preferences

`preferences.ts` initializes before the first screen. On Windows, `system_locale` reads the user's **display** language using `GetUserDefaultUILanguage` and `LCIDToLocaleName`, independently of regional date/number formats. Korean, English and Japanese are supported; other languages, including Chinese, fall back to English. A user's explicit language overrides detection. `i18n.ts` owns UI copy and known native error-context translations; device names, addresses and technical diagnostics are preserved.

Theme defaults to **System**. `matchMedia('(prefers-color-scheme: dark)')` follows Windows/WebView2 theme changes while System is selected. Light/Dark override the system. Preferences persist under `ltac.language` and `ltac.theme` in WebView local storage. Native tray labels update on language changes; their selection also persists in the app configuration directory's `ui-language.txt` for startup before the WebView opens.

## Verification

- `npm test` — existing input, update and page/motion regressions.
- `npm run build` — TypeScript and production assets.
- `node tools/check-theme-language.mjs` — Edge browser: three locales × two themes, unsupported locale fallback, discovery/empty/manual/connection/failure/home/menu screens, selection and keyboard controls, OS theme changes, overrides and persistence, exact greens, rendered WebGL contrast, text clipping/contrast and token references. Screenshots go to ignored `docs/evidence/theme-language/`.
- `node tools/check-connection-flow.mjs` — minified production connection success/failure/cancellation, with normal and reduced motion.
- `cargo test --manifest-path src-tauri/Cargo.toml --locked --all-targets` and `cargo clippy --manifest-path src-tauri/Cargo.toml --locked --all-targets -- -D warnings` — desktop compile and regression checks.

Browser tools require Playwright (locally installed or supplied through `NODE_PATH`) and Microsoft Edge. They use simulated TV data and do not change Windows personalization or control a physical TV. Native Windows API wiring is compiled; system-change behavior is exercised through Chromium's media emulation.
