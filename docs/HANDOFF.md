# Vitrine 2: handoff (2026-11-04)

Where things stand, so work can resume in a new session. All repositories live under
`C:\Users\emanuele\Progetti` and are pushed: no uncommitted work as of this note.

## Repositories (GitHub org `vitrine-org`, renamed from `vitrine-widgets`)

| Local folder | Repo | State |
| --- | --- | --- |
| `vitrine` | `vitrine-org/vitrine` | main 4e3f8ae, CI green. **v2.0.0 not tagged yet.** |
| `vitrine-sdk` | `vitrine-org/sdk` | v2.2.0 released (localisation). v2.1.0 added `prompt()`, `backdropFilter`. No npm: installed from GitHub (`github:vitrine-org/sdk#v2.2.0`). |
| `vitrine-essentials` | `vitrine-org/essentials` | 2.1.0 released, 13 widgets (clock, analog, calendar, system, network, battery, weather, todo, notes, links, drives, countdown, progress). Glass card, Theme System/Light/Dark. |
| `packs/sargas` | `vitrine-org/sargas` | 2.1.1 (original widgets only; "Serbas" was a typo, renamed). |
| `packs/antares` | `vitrine-org/antares` | 2.1.1 (original widgets only). |
| `packs/mond` | `vitrine-org/mond` | 2.1.1 (original widgets only). |
| `packs/alnitak` | `vitrine-org/alnitak` | 2.2.0: word clock + weather merged into one widget (`wordclock`), as in the v1 theme. |
| `vitrine-store` | `vitrine-org/store` | Catalogue built from pack releases every 6 h → `https://vitrine-org.github.io/store/index.json`. Submissions by PR (`packs/<id>.json`). |
| `vitrine-org.github.io` | site | `https://vitrine-org.github.io/`: landing, Store page, full docs EN/IT (45cd6b0, deployed). |

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

## Next steps (in order)

1. **Review the site** (deployed, 65 pages, no broken links): landing with real renders, Store at
   `/packs/` and `/it/packs/` (`/store/` is the catalogue's own Pages site), 50 docs pages for SDK
   v2.2.0. Check that Alnitak's merged widget and the removed theme widgets are reflected. Point the
   tray's "Get more widgets" fallback / site links at `/packs/` where relevant.
   Issues the site agent found while documenting (verify, then fix):
   - `net:*.host` wildcard is rejected by the manifest parser;
   - the `system` metric has no permission that grants it;
   - `minEngine` is never enforced;
   - `transform` transitions don't animate, though Essentials' to-do and links use them;
   - the SDK template fails `tsc`: Solid's JSX types clash with `view`/`text`/`svg`;
   - the Antares date widget clips "Wednesday" at every width.
2. **Release Vitrine 2.0.0**: review `docs/release-notes/v2.0.0.md` (add menus, store, languages,
   search), then `git tag -a v2.0.0 && git push origin v2.0.0`. `release.yml` builds the NSIS installer
   (with the latest Essentials release) and a portable zip. The installer was never compiled locally
   (no makensis here): watch the workflow.
3. **Test the installer** on this PC, but **ask the user first**: it takes over autostart and the
   `.vitrine` file association from the dev build.
4. Possible later work: outer soft shadow for cards (window would need a margin), time zones (world
   clock), per-process CPU, multi-monitor and wallpaper-slideshow testing of the glass, vitrine://
   links from the website's Store to the app.

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
- **Test leftover:** a calendar instance (`wtestcal-0`) was added to the user's `layout.json` for
  click testing; ask whether to keep it.
- **User rules:** at most two agents at a time (agents must not spawn sub-agents); compact answers;
  premium Fluent quality; the site stays minimal.
