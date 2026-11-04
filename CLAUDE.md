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

## Conventions

- Release line is 2.x (first release 2.0.0); Vitrine 1 history lives on branch `v1`.
- `cargo fmt`, `cargo clippy --all-targets` and `cargo test` must pass before committing.
