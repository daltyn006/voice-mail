# UI version pins (pinned + tracked, revisited quarterly)

| Crate | Pin | Why |
|---|---|---|
| `gpui-kit` (facade) | `=0.6.1` | Blessed boot (`application()` + `init()` + `Root`); mirrors kit hello_world |
| `gpui-component` | `=0.6.1` | Comes via the facade; never depended on directly |
| `gpui-pre` (package) | `=0.3.1` | Comes via the facade; never depended on directly |
| `embed-resource` (build-dep) | `=3.0.11` | Embeds `assets/voice.ico` into the exe; established, tiny |
| `rfd` | `0.15` | Native file dialogs; semver range is fine (leaf UI dep) |
| `async-channel` | `2` | Backend→UI event bridge; semver range is fine (leaf dep) |
| `zip` (backend) | `=2.4.2` | OOXML/ODF/EPUB containers for the offline document extractor |
| `quick-xml` (backend) | `=0.36.2` | Streaming `w:t`/`a:t`/`text:p` text nodes for the extractor |
| `calamine` (backend) | `=0.24.0` | `xlsx`/`xls`/`ods` sheets → markdown tables; pure Rust |
| `encoding_rs` (backend) | `=0.8.41` | UTF-16/Windows-1252 fallback decoding for plain-text docs |
| `pdf-extract` (backend) | `=0.7.12` | Text-layer PDF extraction; scanned PDFs warn instead of OCR |
| `cpal` (backend) | `=0.18.2` | Cross-platform mic capture (WASAPI/CoreAudio/ALSA) for the Record page |
| `rf64` (backend, in-tree) | — | Streaming RF64/ds64 24-bit take writer + reader (`pv-backend/src/rf64.rs`); `hound` removed 2026-09 (RIFF-only, wraps past 4 GiB) |

## Facade doctrine (burned twice, recorded forever)

`app/` depends on **`gpui-kit` only** for UI. The kit re-exports the matched
set (`use gpui_kit::*` is GPUI itself), so versions cannot drift apart.
Direct `gpui` / `gpui-component` dependencies are banned — depending on both
the facade and the raw crates risks duplicate-type graphs.

## The critical decision

`gpui-component 0.6.1` does **not** depend on Zed's `gpui` crate at all. It builds on
`gpui-pre ^0.3.1` (+ `gpui-base`, `gpui-pre-macros`). Depending on both `gpui 0.2.2`
and `gpui-component 0.6.1` would compile **two different UI frameworks** into one
binary with mutually incompatible types. So: `gpui-pre` only, never `gpui`.

Naming trap (burned us once): the package is `gpui-pre` but its **lib target is
named `gpui`** — exactly like `gpui-component` itself declares it
(`gpui = { version, package = "gpui-pre" }`). Our `app/Cargo.toml` does the
same rename, so code reads `use gpui::...`. If `use gpui_pre::` ever reappears,
the build fails with E0432/E0433 — that is the tripwire working.

## Track policy (pin and track)

- `=` pins in `app/Cargo.toml` + committed `Cargo.lock`. Never float.
- Quarterly: `cargo update -p gpui-pre -p gpui-component`, then full rebuild +
  `node tests/smoke.mjs` + build-machine proof before committing the new lock.
- Never upgrade mid-milestone.

## Known upstream issues

- `gpui-pre-macros 0.3.1` fails **release** builds with E0433
  (`derive_inspector_reflection` referenced but cfg'd out when
  `debug_assertions` are off). Worked around via
  `[profile.release] debug-assertions = true` in the workspace root.
  Re-evaluate each quarterly review; drop the override once a fixed
  `gpui-pre` is pinned.

## M0 proof (run on a networked Windows machine)

```powershell
cargo tree -i gpui-pre          # exactly ONE gpui-pre version; no `gpui` (Zed) in graph
cargo build --workspace         # app window opens, shows M0 placeholder
cargo test -p pv-backend        # dirs + health unit tests
```

## Build-time note

First-time compilation of the GPU stack (`gpui-pre` + `gpui-component`:
Blade graphics, cosmic-text, font-kit) is slow — tens of minutes in release,
faster in dev profile. This is inherent, not a hang: `gpui-component` ships
with an empty default feature set (no tree-sitter weight), so there is nothing
to trim. Watch Task Manager: `rustc` pinned + RAM headroom = just wait. Use
`build.ps1 -Dev` for iteration; reserve `--release` for shipping.
