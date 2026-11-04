<p align="center">
  <img src="assets/icon/vitrine.svg" width="96" alt="Vitrine icon">
</p>

<h1 align="center">Vitrine</h1>

<p align="center">
  Lightweight, interactive desktop widgets for Windows — drawn <b>beneath</b> your desktop icons.
</p>

<p align="center">
  <a href="https://vitrine-org.github.io/">Website</a> ·
  <a href="https://vitrine-org.github.io/docs/introduction/">Docs</a> ·
  <a href="https://vitrine-org.github.io/it/">Italiano</a>
</p>

> Vitrine 2 is a ground-up rewrite of Vitrine 1 (C#/WebView2, on branch [`v1`](../../tree/v1)).

## What it is

Vitrine turns your desktop into a **grid**. Place, resize and arrange **widgets** — clocks, system
gauges, weather, anything you can write in JSX — and they live under your icons, just like the
wallpaper, while staying clickable.

- **Native and frugal** — a Rust engine with its own renderer (QuickJS + Taffy + Direct2D/DirectWrite).
  No browser engine, no Chromium processes. It redraws only what changes and pauses when the desktop
  is hidden.
- **Under the icons, still interactive** — widgets sit between the wallpaper and the icons; clicks and
  scrolls that miss an icon are forwarded to the widget.
- **Widgets, not themes** — each widget is a small package (`.vitrine`) with its own settings,
  permissions and size on the grid. Run as many instances as you like, on every monitor.
- **Write widgets in JSX** — powered by SolidJS fine-grained reactivity through
  [`@vitrine/sdk`](https://github.com/vitrine-org/sdk).

## Use

Install with `VitrineSetup-<version>.exe` from [Releases](../../releases) (per-user, no admin rights).
Vitrine starts in the tray: **Add widget**, **Edit layout** (or <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>V</kbd>)
to drag and resize widgets on the grid, right-click an interactive widget for its menu.
Double-click a `.vitrine` file to install a widget package.

```
vitrine                         run (tray icon, widgets on the desktop)
vitrine install <file.vitrine>  install a widget package
vitrine --edit | --quit         toggle edit mode / quit the running instance
vitrine render <package> <widget> --cells 4x2 --out shot.png   headless preview
```

Data lives in `%APPDATA%\Vitrine` (`layout.json`, `settings.json`, `packages\`, `logs\`).

## Build

Rust (stable, MSVC) on Windows 10/11:

```
cargo build --release -p vitrine-app      # target/release/vitrine.exe
cargo test
```

## Repository layout

| Path | Contents |
|---|---|
| `crates/vitrine-app` | The `vitrine.exe` application: runtime, tray, edit mode, CLI |
| `crates/vitrine-desktop` | Desktop embedding beneath the icons, input routing, monitors |
| `crates/vitrine-render` | Scene, Taffy layout, Direct2D/DirectWrite painting, DirectComposition |
| `crates/vitrine-script` | QuickJS runtime and the host API used by the SDK |
| `crates/vitrine-system` | System metrics collector and power state |
| `crates/vitrine-widgets` | Package format, safe install, layout and settings schema |
| `installer/` | NSIS installer |
| — | SDK: [vitrine-org/sdk](https://github.com/vitrine-org/sdk) · Widget packs: [vitrine-org](https://github.com/vitrine-org) |
| — | Website: [vitrine-org.github.io](https://github.com/vitrine-org/vitrine-org.github.io) |
| `assets/` | Icon and artwork |

## License

[MIT](LICENSE) © Emanuele Frascella
