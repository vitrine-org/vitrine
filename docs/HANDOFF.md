# Vitrine 2: handoff (updated 2026-11-04 evening)

Where things stand, so work can resume in a new session. All repositories live under
`C:\Users\emanuele\Progetti` and are pushed: no uncommitted work as of this note.

## Repositories (GitHub org `vitrine-org`, renamed from `vitrine-widgets`)

| Local folder | Repo | State |
| --- | --- | --- |
| `vitrine` | `vitrine-org/vitrine` | **v2.0.2 released** (2.0.0 → 2.0.1 fixes → 2.0.2 library + macOS-style sheet). Installer unsigned (SmartScreen warns). |
| `vitrine-sdk` | `vitrine-org/sdk` | v2.2.1 (own JSX namespace, template typechecks). No npm: `github:vitrine-org/sdk#v2.2.1`. |
| `vitrine-essentials` | `vitrine-org/essentials` | 2.1.2: 13 widgets, flat opaque acrylic tint (no gradient), Theme System/Light/Dark. |
| `packs/sargas` | `vitrine-org/sargas` | 2.1.2 (original widgets only; "Serbas" was a typo, renamed). |
| `packs/antares` | `vitrine-org/antares` | 2.1.3 (original widgets; date sized from the longest weekday). |
| `packs/mond` | `vitrine-org/mond` | 2.1.2 (original widgets only). |
| `packs/alnitak` | `vitrine-org/alnitak` | 2.2.1: word clock + weather in one widget (`wordclock`), as in v1. |
| `vitrine-store` | `vitrine-org/store` | Catalogue built from pack releases every 6 h → `https://vitrine-org.github.io/store/index.json`. Submissions by PR. |
| `vitrine-org.github.io` | site | Landing, Store at `/packs/` (neutral #1f1f1f preview tiles), 50 docs pages EN/IT for SDK v2.2.1. |

Every pack has `scripts/previews.py`: previews are transparent (themes) or on a flat #2a2a2a card
(Essentials), shown on one neutral tile everywhere. Fonts were audited against the v1 themes: all
resolve, none falls back to Segoe UI.

The user did not like the extra widgets added to the four themes in 2.1.0: they were removed and
the 2.1.0 theme releases deleted. Don't add widgets to the themes without asking.

## Done in this stretch

- Grid fills the work area (cells stretch, rows centred).
- Windows 11 menus (`crates/vitrine-app/src/flyout.rs`): acrylic, Fluent icons, toggles, submenus, keyboard.
- `prompt()` text input: `vitrine --prompt` Slint dialog (`prompt.rs`, `PromptWindow` in `ui/panel.slint`).
- Frosted glass `backdropFilter` (`vitrine-render/src/backdrop.rs`, `vitrine-desktop/src/wallpaper.rs`, `vitrine-app/src/glass.rs`); `render --backdrop`.
- `textDecoration` (underline / line-through).
- Localisation in 10 languages (en, it, fr, de, es, pt-BR, zh, ru, ja, ko): `i18n.rs`, `locales/*.json`, Slint `@tr()` via generated .po, installer languages, manifest `locales` and settings label maps.
- Panel: "Add widgets" search + package/size/interactive/network filters; edit-mode gallery search + package tabs; Store page (install/update with SHA-256 check, update badge); tray "Get more widgets" opens the Store page.
- Tray icon re-added after Explorer restarts; dependencies optimised in dev builds (debug QuickJS tripped the 250 ms script slice).
- New app icon "Lastre" (by the user, `assets/icon/src/lastre.py`); org avatars in `Downloads\vitrine-org-avatar-{dark,light}.png`; site favicons regenerated.
- Gallery redesign: panel "Add widgets" in collapsible pack sections (favourites first, reorder/hide
  from the header menu, collapse/expand all, grid or list view; `gallery.json` via
  `vitrine_widgets::gallery::Prefs`); every preview on one neutral dark tile. Edit mode's "Add widget"
  is a centred sheet (`edit.rs` + `sheet.rs`): sidebar (search, All, Favourites, packs), each widget in
  its min/default/max sizes rendered off-screen at true proportions (worker in `runtime.rs`, cached
  per session), drag or "+" to add, Tab/arrows/Enter/Esc.
- 145+ tests (app 94+, store builder, locales).

## Next steps

1. **Ask the user for feedback on 2.0.2** (installed on their PC from the release installer):
   the edit-mode Add widget sheet (sidebar, size variants, drag / +) and the panel library
   (collapsible pack sections, favourites, order/hide, grid/list).
2. Their desktop now shows only Mond date + weather (since about 19:15 on 2026-11-04; Essentials
   widgets were removed, probably by the user). Ask before restoring anything.
3. Ideas not started: code signing for the installer; outer soft shadow for cards; time zones
   (world clock); per-process CPU; multi-monitor and slideshow testing of the glass; vitrine://
   links from the website's Store to the app; the edit-mode hint line above the sheet may be
   redundant now.

## Working notes

- **Git:** SSH to GitHub fails ("Permission denied (publickey)") and GitHub had intermittent 500s.
  All local repos use HTTPS remotes with a per-repo `credential.helper = !gh auth git-credential`.
  Retry pushes on 500.
- **Build:** run `export PATH="$HOME/.cargo/bin:$PATH"` first. A dev instance runs from
  `target/debug/vitrine.exe` and locks it: `taskkill //f //im vitrine.exe` before building, then
  restart it in the background.
- **Desktop testing:** the scratchpad scripts `desk.ps1` (shot/click/rclick/drag/move/key/showdesktop)
  and `winshot.ps1` / PrintWindow. Edit mode minimises the user's windows (Peek) and restores them
  on close; never close or move the user's windows yourself.
- **Installed build:** `%LOCALAPPDATA%\Programs\Vitrine\vitrine.exe` (2.0.2) owns autostart and the
  `.vitrine` association. `taskkill //f //im vitrine.exe` also closes an open control panel;
  restart the installed exe after testing a debug build. Releases: if a tag push does not trigger
  `release.yml`, run `gh workflow run release.yml -f tag=vX.Y.Z`.
- **User rules:** at most two agents at a time (agents must not spawn sub-agents); compact answers;
  premium Fluent quality; the site stays minimal.
