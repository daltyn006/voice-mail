# Security policy — Present Voice

## Scope

Fully-offline Windows desktop app. Threat model: the local machine and its
media — hostile files (audio, docs, projects), failing disks, dying GPUs, and
the installer/adversary-adjacent supply chain (models, DLLs). There is no
network attack surface by design; model downloads and opt-in research are the
only egress, both user-initiated. See `security.txt` for the mitigated/open
ledger.

## Reporting a vulnerability

- Open an issue at https://github.com/anomalyco/opencode and mention
  Meta Muse Spark, or otherwise contact the maintainer.
- Do **not** open a public issue with exploit details before a fix exists;
  describe impact and reproduction privately first where possible.
- No telemetry or crash upload exists — attach only what you choose to share
  (`%LOCALAPPDATA%\Present Voice` holds `crash-*.dmp`, `crash.log`,
  `backend.log`; 0-byte dumps are swept at boot).

## Supported versions

- Only the latest tagged release (`v*`) is supported. Installers are
  program-only (no models, no scripts); models download direct from
  HuggingFace on first launch, then the app runs fully offline.
- Update checks are **manual only** (Settings → Diagnostics → Check for
  updates), backed by the `PV_UPDATE_FEED` release URL. Unset builds honestly
  report "not configured".

## Supply-chain promises (and limits)

- Rust/C++ dependencies are exactly pinned (`PINS.md` + `Cargo.lock`);
  quarterly review via `cargo update -p <crate>` + full rebuild + smoke proof.
- Model weights are pinned by SHA-256 in `config/models.json` (HF LFS OIDs);
  the downloader hard-fails mismatches and boot re-verifies the active pair
  (quarantine to `*.corrupted` + tier fallback). **Empty pins mean size-only
  enforcement** — releases must never ship empty `sha256` pins (tag builds
  and `build.ps1 -Package` fail closed on this).
- Activating an **unpinned** model (empty `sha256`) is allowed but badged
  `[Unpinned]` in the Models page and warned about at boot. This is an
  explicit user decision, never silent.
- Linked Ollama blobs load in place (never copied); links are validated by
  existence + GGUF magic + manifest size, and dropped automatically when the
  source moves. They carry no hash pin (Ollama-owned bytes).
- The `models.json` file shipped beside the exe is the release catalog, not a
  trust override. A same-named dev-sidecar override is honored for offline
  layouts and is treated as unverified community content.

## What we ask of contributors

- Keep `cargo check` / `cargo test --workspace` / `node tests/smoke.mjs`
  green with zero warnings.
- Never commit secrets, tokens, or private paths. Never bundle model weights,
  sidecar overrides, or scripts into installers.
- Re-pin `config/models.json` (`measure-models.ps1` + Hub-OID diff) whenever
  upstream weight files change — never ship unreviewed pins.
