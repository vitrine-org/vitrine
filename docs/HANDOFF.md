# Vitrine 2: handoff (updated 2026-11-05)

Where things stand, so work can resume in a new session. All repositories live under
`C:\Users\emanuele\Progetti` and are pushed: no uncommitted work as of this note.

## Repositories (GitHub org `vitrine-org`, renamed from `vitrine-widgets`)

| Local folder | Repo | State |
| --- | --- | --- |
| `vitrine` | `vitrine-org/vitrine` | **v2.0.3 released**: Mica panel chrome (`chrome.rs`), pack icons (manifest `icon`), previews without text shadow (light tile for dark text), headless renders show host "DESKTOP". Installer unsigned. |
| `vitrine-sdk` | `vitrine-org/sdk` | v2.2.2 (`pack` copies the manifest `icon`). Theme packs still build with `#v2.1.0`, which copies only a root `icon.png`: keep icons at the pack root. No npm. |
| `vitrine-essentials` | `vitrine-org/essentials` | **3.0.0: only the `conky` widget** (user's choice; the 13 older widgets live on in 2.2.1). |
| `packs/vega` | `vitrine-org/vega` | 1.0.0: clean rounded widgets in the style of the macOS desktop widgets (Clock, Calendar, Weather, Batteries, Reminders, Note), Inter, own SVG icons. Never named after Apple products. |
| `packs/sargas` | `vitrine-org/sargas` | 2.1.3 |
| `packs/antares` | `vitrine-org/antares` | 2.1.4 |
| `packs/mond` | `vitrine-org/mond` | 2.1.3 |
| `packs/alnitak` | `vitrine-org/alnitak` | 2.2.2 (word clock + weather in one widget) |
| `vitrine-store` | `vitrine-org/store` | Catalogue every 6 h → `https://vitrine-org.github.io/store/index.json`; now carries `icon` (`icons/<pack>.png`). Vega listed. |
| `vitrine-org.github.io` | site | Landing, Store at `/packs/`, docs EN/IT (manifest `icon` documented). |

Themes (Sargas, Antares, Mond, Alnitak): original widgets only, dates and weather texts always in English
as in the originals (`formatDate(..., EN)`), settings translated in 9 languages (`labels`/`titles`/
`descriptions` maps), pack icon at the root `icon.png` (approved by the user). Previews: transparent,
no text shadow.

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

1. Ask the user for feedback on 2.0.3 (Mica panel, pack icons), Vega and the Conky-only Essentials.
2. Essentials store preview is rendered in Italian (this PC's locale); an English render would suit the
   public Store better (the renderer has no language flag yet: worth adding `render --lang`).
3. Engine gaps found while building Vega: continuous (squircle) corners, SVG gradients, time zones,
   multi-line `prompt()`, calendar events.
4. Ideas not started: code signing; time zones; per-process CPU; multi-monitor glass testing;
   vitrine:// links from the website Store.

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
- **Installed build:** `%LOCALAPPDATA%\Programs\Vitrine\vitrine.exe` (2.0.3) owns autostart and the
  `.vitrine` association. `taskkill //f //im vitrine.exe` also closes an open control panel;
  restart the installed exe after testing a debug build. Releases: if a tag push does not trigger
  `release.yml`, run `gh workflow run release.yml -f tag=vX.Y.Z`.
- **Publishing:** pushes, releases and new public repos are blocked by the auto-mode classifier
  unless the user approves them in the conversation; commit locally first, then ask.
- **Bash heredocs** collapse `\\`: write patch scripts with the Write tool.
- **User rules:** at most two agents at a time (agents must not spawn sub-agents); compact answers;
  premium Fluent quality; the site stays minimal.
