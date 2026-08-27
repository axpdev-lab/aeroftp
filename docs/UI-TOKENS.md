# UI Customisation Tokens

AeroFTP exposes a small, documented set of CSS custom properties ("UI tokens") that users can override to adjust the appearance of the application. Arbitrary user CSS was considered and rejected: Tailwind class names are generated per build and are not a stable contract, and a user-supplied stylesheet is a parsing and injection surface. Tokens are the stable, validated alternative.

This page is the public contract for that surface. A token is public from the moment it appears here, not from the moment it appears in the CSS.

## How to override

Overrides live in a JSON file named `ui-tokens.json` in the AeroFTP data root (the same directory the CLI and the GUI share, e.g. `~/.config/aeroftp` on Linux, `%APPDATA%\aeroftp` on Windows, `~/Library/Application Support/aeroftp` on macOS, or the portable `data/` directory next to the executable when running a portable build). The settings control reveals the file in the file manager, so there is no need to locate it by hand.

The file maps token names to values:

```json
{
  "--aeroftp-scrollbar-width": "12px",
  "--aeroftp-scrollbar-thumb": "rgba(200, 200, 200, 0.4)"
}
```

Overrides are applied once at startup and again on the explicit "reload" control in settings. They are not applied on file change; a file watcher would re-apply on every editor autosave, which is exactly the repaint storm the theme machinery guards against.

Validation is reject, not sanitise:

- The key must be one of the published tokens listed on this page. Unknown keys are dropped, not passed through.
- The value must match the shape declared for that token (see the tables below).
- Values containing `url(`, `expression`, `;` or `}` are malformed by definition and are dropped.
- A rejected entry is reported in the activity log with the key and the reason. Rejection is never silent.

The reset control in settings restores the defaults by removing all overrides.

## Published tokens: scrollbar metrics (Tier A)

Length tokens accept `<number>px` with an integer or decimal number in the stated range.

| Token | Default | Controls | Accepted shape and range |
|---|---|---|---|
| `--aeroftp-scrollbar-width` | `6px` | Scrollbar thickness, main window | `<number>px`, 2 to 24 |
| `--aeroftp-panel-scrollbar-width` | `10px` | Scrollbar thickness inside panels and tables | `<number>px`, 2 to 24 |
| `--aeroftp-scrollbar-radius` | `3px` | Scrollbar thumb corner radius | `<number>px`, 0 to 12 |
| `--aeroftp-scrollbar-thumb` | `rgba(128, 128, 128, 0.15)` | Scrollbar thumb colour | Hex (`#rgb`, `#rrggbb`, `#rrggbbaa`) or `rgb()`/`rgba()` with numeric components |
| `--aeroftp-scrollbar-thumb-hover` | `rgba(128, 128, 128, 0.3)` | Scrollbar thumb colour on hover | Same colour shape as above |

These five cover the scrollbar customisation request from discussion #347.

## Published tokens: colours

AeroFTP already carries a set of `--color-*` custom properties that themes rest on. The following subset is safe to override and is therefore published. All of them accept the colour shape described above: hex (`#rgb`, `#rrggbb`, `#rrggbbaa`) or `rgb()`/`rgba()` with numeric components.

| Token | Controls |
|---|---|
| `--color-accent` | Accent colour for interactive elements |
| `--color-accent-hover` | Accent colour on hover |
| `--color-bg-primary` | Primary background |
| `--color-bg-secondary` | Secondary background |
| `--color-bg-tertiary` | Tertiary background |
| `--color-text-primary` | Primary text colour |
| `--color-text-secondary` | Secondary text colour |
| `--color-text-tertiary` | Tertiary text colour |
| `--color-border` | Default border colour |
| `--color-border-strong` | Emphasised border colour |

### Not published

- `--color-success`, `--color-warning`, `--color-error`: not published. A user who sets error and success to the same colour has removed a signal they rely on elsewhere; cosmetic freedom is not worth making a destructive-action warning indistinguishable from a confirmation.
- `--color-surface`, `--color-surface-hover`: not published in Tier A. They carry an alpha component and feed the glass effect; a value with the wrong alpha makes overlapping panels unreadable rather than merely ugly. These may be published later once a validated alpha range exists.

Every other CSS custom property in the application is private, including all `--color-*` properties not listed above.

## Contract

The tokens listed on this page will not be renamed or removed without a deprecation path. Any CSS custom property not listed on this page is private and may change in any release without notice. A token becomes public when it appears on this page, not when it appears in the CSS; that distinction is what keeps private variables private.

## Naming rule

New tokens carry the `--aeroftp-` prefix. The historical `--color-*` names stay as they are: renaming them across all themes to gain a prefix would be churn with no user-visible benefit.

## Interaction with themes

Overrides are set on `document.documentElement.style`, which is an inline declaration and therefore wins over both `:root` and the theme classes. An override wins over every theme and survives a theme switch: the user chose that scrollbar or that colour, not that theme's version of it.

The consequence to be aware of: a user who overrides a colour will see it in every theme, including themes where it may be unreadable. That is their choice, and the reset control in settings is the way out.
