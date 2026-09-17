# Release checklist (every release — run on a networked Windows dev box)

## 1. Pins + hashes (needs network)
- `pwsh scripts/dev/measure-models.ps1` — re-pins `config/models.json`
  `bytes` + `sha256` from local files.
- Diff every hash against the HuggingFace Hub Files page (LFS OID ==
  file SHA-256) before committing. Never ship unreviewed pins.
- Quarterly: `cargo update -p gpui-pre -p gpui-component` per `PINS.md`,
  then repeat the whole list before committing the lock.

## 2. Version + feed
- Bump `version` in `app/Cargo.toml`, `pv-backend/Cargo.toml`,
  `[package.metadata.packager]`, and keep the workspace in lockstep
  (`pv-backend::update::current_version` reads its own crate version).
- Build with the releases feed baked in (drives Settings → Check for
  updates; unset builds honestly report "not configured"):
  `PV_UPDATE_FEED=https://api.github.com/repos/OWNER/REPO/releases/latest`

## 3. Tests (all green, zero warnings)
- `cargo test --workspace`
- `node tests/smoke.mjs`
- `cargo audit` (clean; CI also runs it on every push).
- Rebuild + run `tests/fixtures/test_audacity.exe` (see
  `tests/fixtures/make_aup3.py`): 26/26 native importer checks.
- Full-DLL proof: `pwsh scripts/build.ps1` (compiles the C++ core —
  the only validation the new `qarg`/SRT/Audacity code gets, since CI
  here is Rust-only).
- Pin gate: tag builds fail closed on any empty `sha256` in
  `config/models.json`; `build.ps1 -Package` refuses the
  feed-plus-empty-pins combo (`PV_UPDATE_FEED` set). Populate pins first.

## 4. Package + install
- `pwsh scripts/build.ps1 -Package` → `dist/` (NSIS exe + WiX msi +
  `SHA256SUMS`; SBOM under `target/cyclonedx/` when `cargo-cyclonedx`
  is installed — CI always generates + uploads it).
- Sign when a cert is available: set `PV_SIGN_PFX` (plus
  `PV_SIGN_PASSWORD` if needed) and the script Authenticode-signs every
  exe/msi via `signtool`; otherwise it warns and ships unsigned with
  `SHA256SUMS` as the integrity story.
- Install both on a clean machine; confirm: no models/, no scripts/
  inside; wizard downloads tier; standard-user launch works.
- **Offline proof**: with models present, disconnect network, run a full
  batch end-to-end (transcribe → summary → output → edit → diff-save)
  plus one video job (frames + captions + `.srt`). Zero network errors.

## 5. Never ship
- A `models.json` sidecar beside the exe (dev override = unverified
  community models).
- Empty `sha256` pins combined with the feed configured (size-only
  enforcement with an update channel is a false promise).
