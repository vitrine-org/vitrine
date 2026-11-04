# Vitrine — project instructions

Vitrine 2 is a Rust desktop widget engine for Windows (see README.md).

## Design work

Use the **frontend-design** skill (plugin `frontend-design@claude-plugins-official`, enabled in
`.claude/settings.json`) for every piece of visual design in this project:

- the rendering engine's defaults and primitives (`crates/vitrine-render`),
- the edit-mode overlay and any on-desktop chrome (`crates/vitrine-desktop`, `crates/vitrine-app`),
- the control panel (`crates/vitrine-panel`),
- built-in widgets and widget packs (`widgets/`, and the `vitrine-org/*` pack repos),
- the website (separate repo `vitrine-org/vitrine-org.github.io`).

Follow Windows 11 Fluent conventions for anything that lives on the desktop or in the panel, and
keep the website minimal (icon, a few concepts, links).

## App icon

The icon is "Lastre": an abstract V of two panes (lit lagoon behind, deep lagoon in front, an ember
orb between them) on the 48-unit Fluent grid. It replaced the earlier "Acrylic" icon. Sources, build
and design rules (keyline, 1:2 construction, radii, pixel-fitted small frames, contrast) are in
`assets/icon/src/README.md`; never edit the generated files in `assets/icon/` by hand, change
`assets/icon/src/lastre.py` and run `npm run build` there. Wherever the app or the website shows the
icon, use `assets/icon/vitrine.svg` (or the generated PNG/ICO), never a redrawn copy.

## Conventions

- Release line is 2.x (first release 2.0.0); Vitrine 1 history lives on branch `v1`.
- `cargo fmt`, `cargo clippy --all-targets` and `cargo test` must pass before committing.
