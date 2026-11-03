# Vitrine app icon: sources

The icon ("Acrylic") is hand-built vector art in the Windows 11 Fluent app-icon style: a frosted
widget pane floating over a lagoon "desktop" pane, with an ember orb glowing between them.
Everything in `assets/icon/` is generated from the two Python files here.

| File | What it is |
| --- | --- |
| `acrylic.py` | The icon. One function, `acrylic(size)`, draws the master (size ≥ 48) or a pixel-fitted small frame (32, 24, 20, 16). |
| `kit.py` | Small SVG helpers: 120° Fluent gradients, the two-layer drop shadow, inner highlights, rounded shapes. |
| `build.js` | Rasterises the SVGs with resvg, re-encodes the PNGs (zlib 9, adaptive filters) and packs `vitrine.ico`. |

## Regenerate

Requires Python 3 (standard library only) and Node 18+.

```sh
cd assets/icon/src
npm ci          # first time only: installs @resvg/resvg-js
npm run build   # = python acrylic.py && node build.js
```

To update the website, copy the master afterwards:

```sh
cp ../vitrine.svg ../../../site/src/assets/vitrine.svg
cp ../vitrine.svg ../../../site/public/favicon.svg
```

## Output (`assets/icon/`)

| Path | Use |
| --- | --- |
| `vitrine.svg` | Master artwork (48-unit grid, rendered at 256). Also the website logo and favicon. All ids are prefixed `vit-`, so the SVG can be inlined safely. |
| `vitrine-1024.png` | Master raster for stores and marketing. |
| `vitrine.ico` | Windows icon: 16, 20, 24, 32, 40, 48 and 64 px as 32-bit BMP frames, 256 px as PNG. |
| `png/vitrine-<size>.png` | Individual frames: 16, 20, 24, 32, 40, 48, 64, 96, 128, 256. |
| `svg/vitrine-<size>.svg` | The pixel-fitted small drawings (32, 24, 20, 16). |

## Design rules

- **Grid.** The master uses `viewBox="0 0 48 48"` (Microsoft's 48-unit icon grid), so 1 unit is 1 px
  at 48 px. The panes sit on the 40 × 40 square keyline, offset on the diagonal (back pane 4–34, front pane 15–44).
- **Small frames are redrawn, not scaled.** At 32 px and below, `Fit.q()` snaps the main edges to that
  frame's pixel grid and `Fit.w()` gives strokes a 1 px floor. Detail drops by size: the acrylic blur
  goes below 24 px, the second text line below 32 px, the masked pane shadow below 32 px, and all
  blurred shadows below 32 px.
- **Light.** Ambient light comes from the top left. Every ramp runs light top-left to deep bottom-right
  at 120°, as the Fluent guidelines recommend.
- **Shadows.** A two-layer drop shadow (key: 1 u down, 1 u blur, 22 %; contact: 0.25 u down, 0.25 u blur, 18 %).
  The front pane's shadow is masked into the shapes below it, the Fluent "separate metaphor" shadow.
- **Palette: Lagoon & Ember.**
  - Lagoon `#6BF2E2 → #1EE0D2 → #00B8F5 → #1A7CFF → #2B4DE0 → #1C2A8F` is used for structure.
  - Ember `#FFE05A → #FFB23E → #FF7A45 → #FF4D73` is used for the widget glow.
  - Ink is `#0E1B4D`.
  - The deep lagoon reaches 3:1 on the light taskbar. Aqua, frost and ember reach it on the dark one.
