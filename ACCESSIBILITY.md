# Accessibility Statement

> _Last updated: 2026-11-23_

AeroFTP is meant to be usable by everyone who moves files for a living or for themselves, including people who rely on a keyboard, a screen reader, magnification, larger text or reduced motion. This statement says what we aim for, what works today, what does not work yet, and how to tell us when something gets in your way.

It covers the AeroFTP desktop application (Linux, Windows, macOS) and the `aeroftp-cli` command-line interface. The documentation site ([docs.aeroftp.app](https://docs.aeroftp.app)) and the marketing site ([aeroftp.app](https://aeroftp.app)) are separate projects and are not assessed here.

## Our Commitment

- **Target:** [WCAG 2.2](https://www.w3.org/TR/WCAG22/) Level AA, applied to a desktop application as described in [WCAG2ICT](https://www.w3.org/TR/wcag2ict-22/).
- **Status:** **partially conformant.** Parts of the interface meet the target, others do not yet. We have not commissioned a formal third-party conformance audit, and we do not claim full conformance.
- **Principle:** an accessibility barrier is a bug, not a feature request. It goes through the Issues tab and the same triage as any other defect.

## What Works Today

### Keyboard

- Every core file operation has a keyboard path: Enter to open, Backspace to go up, F2 to rename, Delete, Ctrl+A / Ctrl+C / Ctrl+X / Ctrl+V, Ctrl+F to search, Space for Quick Look, Alt+Enter for properties, arrows / Shift+arrows / Home / End to move and extend the selection.
- Drag and drop always has a keyboard alternative: cut/copy/paste between panels, and F5 (copy to the other panel), F6 (move to the other panel), F7 (new folder) in the dual-panel file manager. Tab switches between panels.
- Dialogs are real modals: focus moves into the dialog when it opens, Tab and Shift+Tab stay inside it, Escape closes it, and focus returns to where it was when it closes.
- Settings uses a `tablist` with Arrow / Home / End navigation, for the sidebar and for the Appearance sub-tabs.
- Resizable panel dividers are keyboard-operable separators (Arrow Left / Right to resize, Home / End to jump to the extremes, Enter / Space to reset) and expose their position to assistive technology.
- Ctrl+= / Ctrl+- / Ctrl+0 change and reset the interface font size; the integrated terminal and the code editor keep their own zoom when they have focus.

### Screen readers and semantics

- Controls carry accessible names, including icon-only buttons. Interactive custom widgets expose roles and states (`dialog`, `alertdialog`, `tab`, `grid`, `checkbox`, `menuitem`, expanded, selected, pressed, checked).
- Errors and progress are announced through `alert`, `status` and `aria-live` regions instead of relying on colour or position alone.
- The document language follows the selected interface language, so a screen reader switches voice and pronunciation with it.
- Accessible names, status messages and announcements are translated along with the rest of the interface, in all 47 supported languages, with plural forms handled per locale.

### Vision and display

- Interface font size is adjustable from 10 to 22 px (Settings, or the shortcuts above), with a choice of font families including a monospace option.
- Eight colour palettes, light and dark, plus an automatic mode that follows the operating system or switches on a schedule you set.
- A compact or detailed layout, optional file extensions, hidden files, status bar and transfer progress, so the screen carries only what you need.

### Motion

- Selected animations (update highlights, loading overlays, transfer progress effects, animated text) stop when the operating system requests reduced motion (`prefers-reduced-motion`). The loading overlay also waits about 250 ms before appearing, so fast operations never flash.

### Command line

`aeroftp-cli` offers a complete non-graphical path to the same operations, which also suits screen-reader and braille users:

- Plain-text output by default; `--json` for structured output that other tools can read reliably.
- Data goes to stdout, status and progress to stderr, so output can be piped or redirected without noise.
- Colour is disabled when `NO_COLOR` is set ([no-color.org](https://no-color.org/)) or when the output is not a terminal; the interactive TUI also switches to a monochrome palette under `NO_COLOR`.
- `--quiet` and `--no-banner` remove decoration; documented, stable exit codes report the outcome without parsing text.

See the [CLI Guide](docs/CLI-GUIDE.md) for the full command set.

## Known Limitations

We would rather tell you about these than have you discover them. Each one is tracked for improvement.

| Area | Limitation | Workaround |
| ---- | ---------- | ---------- |
| Keyboard | Tab traversal through the main window is not yet complete: some regions can be reached only with the mouse or with shortcuts. Tracked on the [roadmap](ROADMAP.md). | Use the documented shortcuts, or `aeroftp-cli` for the same operation. |
| Integrated terminal | The terminal panel does not yet enable its screen-reader mode, so its output is not reliably announced. | Use your system terminal with `aeroftp-cli`. |
| Contrast | The colour palettes have not been audited against WCAG contrast ratios, and there is no dedicated high-contrast theme. Windows High Contrast / `forced-colors` is not specifically supported. | Try the Light or True Dark palettes and a larger font size. |
| Motion | `prefers-reduced-motion` is honoured for selected animations, not yet for every transition. | Hide transfer progress in Settings → General, and enable reduced motion in your operating system. |
| Right-to-left | No right-to-left language ships today, so right-to-left layout has not been verified. | None yet. |
| Assistive technology testing | Screen-reader behaviour has not been tested systematically with each platform reader (Orca, NVDA, JAWS, Narrator, VoiceOver). Results may differ between the Linux (WebKitGTK), Windows (WebView2) and macOS (WKWebView) builds. | Please report what you find: it directly shapes our priorities. |
| Testing | Accessibility is not yet checked automatically in CI; regressions are caught by review and by reports. | None. |

Third-party content shown inside AeroFTP, such as web pages opened for OAuth sign-in by a cloud provider, is outside our control. If a provider's sign-in page blocks you, tell us anyway and we will raise it with them where we can.

## Reporting a Barrier

If something in AeroFTP is hard or impossible for you to use, we want to know, whether or not it is listed above.

- **Public report:** open an issue at [github.com/axpdev-lab/aeroftp/issues](https://github.com/axpdev-lab/aeroftp/issues/new/choose). Maintainers apply the `accessibility` label during triage.
- **Private report:** email **dev@aeroftp.app** with "Accessibility" in the subject, if you prefer not to post publicly.

Helpful details, if you can share them (none is required):

- What you were trying to do and where it stopped you.
- Your operating system and AeroFTP version (Help → About, or `aeroftp-cli --version`).
- The assistive technology and version you use, if any (for example NVDA 2025.3, Orca, VoiceOver, a screen magnifier, voice control).
- Whether a keyboard-only or CLI path worked instead.

What you can expect:

- We acknowledge accessibility reports within **5 business days**.
- A barrier that blocks a core task (connecting, browsing, transferring, unlocking the vault) with no workaround is triaged with the same priority as a functional defect of the same impact.
- We keep the issue updated as it moves, and credit you in the [CHANGELOG](CHANGELOG.md) unless you ask us not to.

## For Contributors

Accessibility is part of "done". Before opening a pull request that touches the interface:

- **Keyboard:** every new control is reachable and operable without a mouse, with a visible focus indicator. New dialogs use the shared modal focus handling (`useModalFocusTrap` or `ConfirmOverlay`) rather than a custom trap.
- **Names and roles:** icon-only buttons get an `aria-label`; custom widgets get the matching ARIA role and state. Prefer native elements (`button`, `input`, `select`) over clickable `div`s.
- **Translations:** accessible names, announcements and `title` text go through i18n like any visible string, never hard-coded English.
- **Colour and motion:** do not carry meaning by colour alone; new animations respect `prefers-reduced-motion`.
- **CLI:** new output works without colour and without a terminal, and anything a script needs is available in `--json`.

General development and pre-push gates live in [CONTRIBUTING.md](CONTRIBUTING.md).

## Related Documents

- [README](README.md)
- [CONTRIBUTING](CONTRIBUTING.md)
- [CODE_OF_CONDUCT](CODE_OF_CONDUCT.md)
- [SECURITY](SECURITY.md)
- [PRIVACY](PRIVACY.md)
- [ROADMAP](ROADMAP.md)
