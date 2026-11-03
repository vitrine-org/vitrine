<p align="center">
  <img src="assets/icon/vitrine.svg" width="96" alt="Vitrine icon">
</p>

<h1 align="center">Vitrine</h1>

<p align="center">
  Lightweight, interactive desktop widgets for Windows — drawn <b>beneath</b> your desktop icons.
</p>

<p align="center">
  <a href="https://vitrine-widgets.github.io/">Website</a> ·
  <a href="https://vitrine-widgets.github.io/docs/introduction/">Docs</a> ·
  <a href="https://vitrine-widgets.github.io/it/">Italiano</a>
</p>

> **Status:** Vitrine 2 is a ground-up rewrite and is under active development. Nothing is released yet.

## What it is

Vitrine turns your desktop into a **grid**. Place, resize and arrange **widgets** — clocks, system
gauges, weather, anything you can write in JSX — and they live under your icons, just like the
wallpaper, while staying clickable.

- **Native and frugal** — a Rust engine with its own renderer (QuickJS + Taffy + Direct2D/DirectWrite).
  No browser engine, no Chromium processes. It redraws only what changes and pauses when the desktop
  is hidden.
- **Under the icons, still interactive** — widgets sit between the wallpaper and the icons; clicks and
  scrolls that miss an icon are forwarded to the widget.
- **Widgets, not themes** — each widget is a small package (`.vwidget`) with its own settings,
  permissions and size on the grid. Run as many instances as you like, on every monitor.
- **Write widgets in JSX** — powered by SolidJS fine-grained reactivity through `@vitrine/sdk`.

## Repository layout

| Path | Contents |
|---|---|
| `crates/` | Rust engine (desktop embedding, renderer, script runtime, system metrics, widgets, panel) |
| `sdk/` | `@vitrine/sdk` and tooling for widget authors |
| `widgets/` | Built-in widgets |
| — | Website: [vitrine-widgets.github.io](https://github.com/vitrine-widgets/vitrine-widgets.github.io) |
| `assets/` | Icon and artwork |

## License

[MIT](LICENSE) © Emanuele Frascella
