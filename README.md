# Present Voice — batch transcribe + summarize (.exe)

Windows-only. C++ core DLL + native GPUI GUI (Rust, Zed's UI framework).
One STT + one LLM resident at a time; FIFO batch; per-chunk VRAM guard.

## For end users (no code, no scripts)

Installers (`build.ps1 -Package` → `dist/`; MSI needs the VBScript optional
Windows feature, default-on), both program-only (no AI models, no scripts):

- `present-voice_*_x64-setup.exe` (NSIS, **per-machine** — needs admin) → `C:\Program Files\Present Voice`, everything in that one folder (installer grants Users write access for good measure).
- `present-voice_*_x64_en-US.msi` (WiX, **per-machine** as well) → same folder, same contents.

Writable data (models, outputs, `app.db`) always lives under `%LOCALAPPDATA%\Present Voice`, so standard users never hit "access denied" regardless of installer.

1. First launch opens the setup wizard: it auto-picks a model tier from your hardware, shows the download size, and lets you override. Models download **direct from HuggingFace** into your per-user data folder (resumable, size-verified) — download once while online, then the app runs **fully offline**.
2. Add audio files (or drag-and-drop files/folders onto Input) → Run. Outputs land in your per-user data folder as `Class - Day N - Title.md`.

### Recording voice in the app

Open the **Record** page → pick an input → **● Record**. Takes save as 24-bit WAV under your data folder (never auto-deleted, never auto-transcribed). **Play** to review, **Send to queue** to stage for transcription — or just leave the page and unsent takes stage themselves. Closing the app mid-take keeps the file.

### Watching films (Documentary)

**+ Add video** stages a film (read in place — never copied, never deleted). The pipeline transcribes timestamped dialogue, samples frames (cuts + attention-driven rate), watches them with a Vision model, and writes occurrences → themes → fenced AI opinion → summary. Films also get a `<Title>.srt` subtitle sidecar from the timestamped transcript. **Settings → Documentary** holds the Attention slider (Overview / Balanced / Academic) and the active Vision model; **Settings → Research** holds the opt-in web toggle (default OFF — when on, up to 10 cited sources supplement the film, never replace it).

| Tier | Auto-pick | Models | ~Download |
|---|---|---|---|
| Lite | VRAM <6GB or RAM <12GB | small.en + Qwen2.5-7B | 4.6 GB |
| Standard | VRAM 6–12GB | medium.en + Llama-3.1-8B | 5.4 GB |
| Full | VRAM >12GB | large-v3-turbo + Gemma-2-9B | 6.2 GB |

### Using your Ollama models (no duplicate downloads)

Already have models in Ollama? Don't download them twice. Open the **Models** page → **Scan Ollama library** → **Link** the one you want (or set it active). What happens:

- The app records the model's location and loads the GGUF blob **straight from Ollama's store** into its own summarizer (`llama.cpp`) — the file is never copied or moved.
- Ollama keeps working exactly as before; both programs read the same bytes.
- If the file is later moved or deleted, the link is dropped automatically with a status note — just rescan and link again.

| Tier | Auto-pick | Models | ~Download | Vision add-on |
|---|---|---|---|---|
| Lite | VRAM <6GB or RAM <12GB | small.en + Qwen2.5-7B | 4.6 GB | Qwen2-VL-2B, ~2.3 GB |
| Standard | VRAM 6–12GB | medium.en + Llama-3.1-8B | 5.4 GB | Qwen2-VL-7B Q4, ~6 GB |
| Full | VRAM >12GB | large-v3-turbo + Gemma-2-9B | 6.2 GB | Qwen2-VL-7B Q8, ~9.5 GB |

If disk space is short for the picked tier, the wizard steps down automatically and says so.
Fully offline after install: download models once while online, then disconnect — transcription, summarization, and editing are 100% local.

### Settings tour

- **Storage** — output/models/drafts folders, converted-cache cleanup, and source retention (keep originals, delete on success, or archive a `.src` copy beside each output; failures, aborts, and videos are always exempt).
- **Processing** — compute mode plus the adaptive denoise gate (recommended/off/aggressive; STT copy only, archival takes untouched).
- **Documentary + Research** — attention slider and vision model (above), plus the plainly-visible web toggle (default OFF).
- Closing the window mid-run is safe: drafts flush, downloads cancel, the backend worker drains and joins (bounded), then the app exits — no teardown crashes.

## For developers

```powershell
pwsh scripts/build.ps1                     # one-go: smoke -> DLL -> release binary
pwsh scripts/build.ps1 -Dev                # fast loop while developing
pwsh scripts/build.ps1 -Setup -FetchModels # full first-time run (toolchains + models)
pwsh scripts/build.ps1 -Package            # release: NSIS + MSI into dist/
pwsh scripts/build.ps1 -SkipTests          # skip the Node smoke test
pwsh scripts/dev/smoke-test.ps1            # static checks only, no toolchain needed
pwsh scripts/dev/measure-models.ps1        # re-pin model byte sizes after fetch
```

## Release checklist (every release)

1. `node tests/smoke.mjs` — all green.
2. `cargo test --workspace` — all green, zero warnings (`cargo check` too).
3. `pwsh scripts/build.ps1 -Package` on a clean Windows machine (first run
   downloads the NSIS + WiX toolchains, ~50 MB from GitHub, into
   `%LOCALAPPDATA%/.cargo-packager`; pre-seed that folder if firewalled).
4. Install both artifacts; confirm: no models/, no scripts/ inside; wizard downloads tier; standard-user launch works.
5. **Offline proof**: with models present, disconnect network, run a full batch end-to-end (transcribe → summary → output → edit → diff-save). Zero network errors in the log.
6. Quarterly: `cargo update -p gpui-kit` per `PINS.md`, then repeat 1–5 before committing the lock.

## Layout

- `core/` — C++ DLL: `audio_ffmpeg` (all formats via 1 ffmpeg DLL set) → `stt_whisper`
  (Vulkan, 30s windows, temp 0) → `llm_llama` (map-reduce + final pass, stats-forcing
  prompts) → `diff` (line LCS JSON) → `Class - Day N - Title.md` + sidecar `.json` + sqlite Day/Class memory.
- `app/` — GPUI binary: Input / Processing / Output pages, merge editor, theme, keymap.
- `pv-backend/` — framework-free Rust lib: models manager, resumable HuggingFace downloader, FIFO queue orchestration over `present_core.dll`, output store.
- `config/models.json` — single source of truth: download manifest, tier pairs, catalog order (bytes ascending).
- `PINS.md` — UI version pins (`gpui-pre`, `gpui-component`) + quarterly track policy.
## Answers

- **.exe? .msi?** Yes to both (`build.ps1 -Package` via cargo-packager into `dist/`): NSIS setup exe **and** WiX `.msi`, both per-machine, both program-only. Neither bundles models or scripts — models install separately via the first-run wizard/downloader, then the app runs fully offline. `ffmpeg.exe` + DLLs sit beside
  the exe (required); installs download models direct from HuggingFace to the per-user data folder, never bundled.
- Without submodules/models the DLL builds with deterministic **mock** STT/LLM so GUI + packaging
  are testable end-to-end (`build.ps1` warns loudly in that case).
- **Day?** Parsed from filename (`Day 4`, `Lec 12`, `2024-..`) with sqlite override memory, else file creation date.
- **Class?** LLM-inferred constrained to known list, fallback parent folder.
- **VRAM?** DXGI budget check before each chunk; over 80% → shrink batch / wait. STT and LLM never co-resident.
