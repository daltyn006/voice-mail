// Smoke test (no toolchain needed): validates configs, Day-parse parity with
// core/src/meta_day_class.cpp, and static contracts across C++/Rust/PS files.
// Run: node tests/smoke.mjs   (or pwsh scripts/dev/smoke-test.ps1)
import { readFileSync, existsSync } from 'node:fs';

let pass = 0, fail = 0;
const ok = (cond, name) => { cond ? pass++ : (fail++, console.error('FAIL:', name)); };
const src = (p) => readFileSync(p, 'utf8');

// --- 1. Configs + workspace parse ---
let models;
try { models = JSON.parse(src('config/models.json')); ok(true, 'models.json parses'); }
catch (e) { ok(false, 'models.json parses: ' + e.message); }
const rootToml = src('Cargo.toml');
ok(rootToml.includes('[workspace]'), 'workspace root declared');
ok(rootToml.includes('"app"') && rootToml.includes('"pv-backend"'), 'workspace members: app + pv-backend');
ok(models?.defaults?.stt_model === 'large-v3-turbo-Q5_0', 'default STT id');
ok(models?.defaults?.llm_model === 'gemma-2-9b-it-Q4_K_M', 'default LLM id');
const stt = models?.stt_models ?? [], llm = models?.llm_models ?? [];
const byId = Object.fromEntries([...stt, ...llm].map((m) => [m.id, m]));
for (const m of [...stt, ...llm]) {
  ok(typeof m.url === 'string' && m.url.startsWith('https://huggingface.co/'), `HF url: ${m.id}`);
  ok(Number.isInteger(m.bytes) && m.bytes > 0, `bytes pinned: ${m.id}`);
  ok(typeof m.tier === 'string', `tier tagged: ${m.id}`);
}
const sorted = (a) => a.every((m, i) => i === 0 || a[i - 1].bytes <= m.bytes);
ok(sorted(stt) && sorted(llm), 'catalog arrays sorted bytes-ascending');
for (const [tier, t] of Object.entries(models?.tiers ?? {})) {
  ok(byId[t.stt] && byId[t.llm], `tier pair valid: ${tier}`);
  ok(Number.isFinite(t.min_vram_gb), `tier threshold: ${tier}`);
}
ok(byId[models?.defaults?.stt_model] && byId[models?.defaults?.llm_model], 'defaults reference catalog ids');
for (const t of ['lite', 'standard', 'full']) ok(t in (models?.tiers ?? {}), `tier present: ${t}`);

// --- 2. Day-parse parity (mirrors meta_day_class.cpp pats[0..3]) ---
const pats = [
  /day[\s_\-]*(\d{1,3})/i,
  /lec(?:ture)?[\s_\-]*(\d{1,3})/i,
  /(\d{4}-\d{2}-\d{2})/,
  /(?:^|[\s_\-\[\(])(\d{1,3})(?:[\s_\-\]\)]|$)/,
];
const parseDay = (stem) => {
  let m;
  if ((m = stem.match(pats[0])) || (m = stem.match(pats[1]))) return 'Day ' + m[1];
  if ((m = stem.match(pats[2]))) return m[1];
  if ((m = stem.match(pats[3]))) return 'Day ' + m[1];
  return null;
};
const dayCases = [
  ['Bio - Day 4 - mitosis', 'Day 4'],
  ['lec12_intro', 'Day 12'],
  ['Lecture 4 - atman', 'Day 4'],
  ['2024-03-01 lecture', '2024-03-01'],
  ['04 - sacred sites', 'Day 04'],
  ['no tokens here xyz', null],
];
for (const [stem, want] of dayCases) ok(parseDay(stem) === want, `day-parse: "${stem}" -> ${want}`);

// --- 3. Rust workspace contracts (native GPUI, no Tauri) ---
const appToml = src('app/Cargo.toml');
ok(appToml.includes('gpui-kit = "=0.6.1"'), 'gpui-kit facade pinned exact');
ok(src('app/src/main.rs').includes('gpui_kit::'), 'boot uses kit facade');
ok(!existsSync('app/src/views_processing.rs'), 'M3 processing view merged into input page');
  ok(src('app/src/views_input.rs').includes('Progress::new'), 'progress bars on input page');
  ok(src('app/src/views_input.rs').includes('active_file_card'), 'active file cards with controls');
  ok(src('app/src/views_input.rs').includes('global_toolbar'), 'global toolbar for pause/abort/continue');
  ok(src('app/src/views_input.rs').includes('warn_button'), 'warning badges for bad files');
ok(src('app/src/store.rs').includes('abort_selected') && src('app/src/store.rs').includes('heuristic_name'), 'M3 store ops present');
ok(existsSync('app/src/views_output.rs'), 'M4 output view exists');
ok(src('app/src/views_output.rs').includes('EditorState') && src('app/src/views_output.rs').includes('open_alert_dialog'), 'editor + guarded delete');
ok(src('app/src/views_output.rs').includes('rev-submit') && src('app/src/views_output.rs').includes('rev-revert') && src('app/src/views_output.rs').includes('Keep summary'), 'unified Review screen (merge/choose/submit/revert)');
ok(src('app/src/store.rs').includes('dismiss_output') && src('app/src/store.rs').includes('dismissed'), 'row-only dismiss distinct from file delete');
ok(src('app/src/views_output.rs').includes('🗑'), 'trash button deletes row + files');
ok(existsSync('app/src/views_models.rs'), 'M4 models view exists');
ok(src('app/src/views_models.rs').includes('cancel_download') &&
src('app/src/views_models.rs').includes('scan_ollama'), 'cancel + scan paths');
ok(existsSync('app/src/views_wizard.rs'), 'M4 wizard exists');
ok(src('app/src/store.rs').includes('tier_info') && src('app/src/store.rs').includes('spawn_pump'), 'M4 store ops present');
ok(!appToml.match(/^\s*gpui(-component)?\s*=/m), 'no direct gpui deps (facade only)');
ok(!src('app/src/main.rs').match(/[^_]gpui::/) && !src('app/src/views.rs').match(/[^_]gpui::/) && !src('app/src/views_input.rs').match(/[^_]gpui::/), 'no bare gpui:: paths (facade only)');
ok(appToml.includes('async-channel'), 'async event bridge dependency');
ok(appToml.includes('pv-backend'), 'app depends on pv-backend');
ok(existsSync('app/src/main.rs') && existsSync('app/src/views.rs') && existsSync('app/src/theme.rs'), 'app shell files exist');
ok(src('app/src/main.rs').includes('open_window'), 'app opens a window');
ok(src('app/src/main.rs').includes('bind_keys'), 'keymap registered');
ok(existsSync('app/src/store.rs') && existsSync('app/src/views.rs') && existsSync('app/src/views_input.rs'), 'M2 shell + input views exist');
ok(src('app/src/store.rs').includes('apply_event'), 'store folds backend events path-keyed');
ok(src('app/src/views_input.rs').includes('Button::new') && src('app/src/views_input.rs').includes('Progress::new'), 'input page uses component widgets');
ok(src('app/src/views_settings.rs').includes('InputState'), 'settings page owns text inputs (Store is data owner)');
ok(src('app/src/views_input.rs').includes('rfd::FileDialog'), 'native file picker (no dialog plugin)');
ok(!src('app/src/views.rs').includes('🔒'), 'no nav lock — processing merged into input');
  ok(src('app/src/views.rs').includes('nav_button("Input"'), 'nav has Input');
  ok(src('app/src/store.rs').includes('page: Page::Models'), 'Models is the startup page');
  ok(src('app/src/views_input.rs').includes('file_warnings'), 'file_warnings for bad formats');
  ok(src('app/src/views_input.rs').includes('global_toolbar'), 'global toolbar on input page');
  ok(src('app/src/views_input.rs').includes('active_file_card'), 'active file cards with progress');
  ok(src('app/src/views_input.rs').includes('warn_button'), 'warning badge for bad files');
  ok(src('app/src/views_input.rs').includes('staged_tile') && src('app/src/views_input.rs').includes('staged_list_row') && src('app/src/views_input.rs').includes('staged_details_row'), 'Icons/List/Details layouts all render');
  ok(src('app/src/store.rs').includes('.aup3') || src('app/src/store.rs').includes('BAD_EXTS'), 'aup3 format handled');
  ok(src('app/src/store.rs').includes('file_warnings'), 'file_warnings HashMap in Store');
  ok(src('app/src/store.rs').includes('pub const AUDIO_EXTS'), 'single shared codec gate in store');
  ok(!src('app/src/views_input.rs').includes('const AUDIO_EXTS'), 'picker reuses the shared gate (no drift)');
  const badList = (src('app/src/store.rs').match(/pub const BAD_EXTS[^;]+;/) || [''])[0];
  ok(!badList.includes('okt'), 'okt is a playable tracker module, not a bad file');
ok(src('pv-backend/src/lib.rs').includes('pub mod dirs'), 'backend exposes dirs module');
ok(src('pv-backend/src/dirs.rs').includes('LOCALAPPDATA'), 'writable data dir is per-user');
ok(existsSync('PINS.md'), 'PINS.md records UI pins + track policy');
ok(!existsSync('gui'), 'Tauri tree removed (clean cut)');
ok(!src('app/Cargo.toml').includes('tauri') && !src('pv-backend/Cargo.toml').includes('tauri'), 'no Tauri dependency');
ok(src('Cargo.toml').includes('debug-assertions = true'), 'release assertions workaround for gpui-pre-macros bug');
// --- 3c. M1 backend modules ---
for (const m of ['catalog', 'core_bridge', 'dirs', 'download', 'manifest', 'models', 'progress', 'queue']) {
  ok(existsSync(`pv-backend/src/${m}.rs`), `backend module present: ${m}`);
}
const be = src('pv-backend/Cargo.toml');
ok(be.includes('reqwest') && be.includes('libloading') && be.includes('sha2') && be.includes('tokio'), 'backend downloader + FFI deps pinned');
ok(src('pv-backend/src/core_bridge.rs').includes('catch_unwind'), 'FFI trampoline panic-guarded');
ok(src('pv-backend/src/core_bridge.rs').includes('epoch'), 'run epoch tags stale pre-abort events');
ok(src('pv-backend/src/queue.rs').includes('refusing to overwrite'), 'rename refuses overwrite');
ok(src('pv-backend/src/queue.rs').includes('ParentDir'), 'output confined against traversal');
ok(src('pv-backend/src/download.rs').includes('cancel: Arc<Mutex<HashMap') && src('pv-backend/src/download.rs').includes('drop_flag'), 'per-download cancel flags with terminal cleanup');
// --- 3b. Dead files stay removed ---
ok(!existsSync('scripts/core'), 'dead scripts/core sqlite duplicate removed');
ok(!existsSync('example.kra'), 'stray example.kra removed');
ok(!existsSync('scripts/.git'), 'stray scripts/.git removed');
ok(!existsSync('scripts/ffmpeg-dlls'), 'stray scripts/ffmpeg-dlls staging removed');
ok(!existsSync('scripts/package-offline.ps1'), 'offline SFX packager removed (no offline packaging)');

// --- 4. C++ contracts ---
const pipe = src('core/src/pipeline.cpp');
ok(pipe.includes('.diff.json'), 'pipeline writes diff sidecar');
ok(pipe.includes('json_escape'), 'pipeline escapes sidecar JSON');
ok(pipe.includes('clean_label'), 'pipeline cleans LLM labels');
ok(pipe.includes('g_worker.join()'), 'worker thread joined (no detach leak)');
ok(src('core/src/internal.h').includes('db_lookup'), 'db_lookup declared');
ok(src('core/src/db.cpp').includes('INSERT OR REPLACE'), 'db_remember implemented');
ok(src('core/src/db.cpp').includes('SELECT day, class'), 'db_lookup implemented');
ok(src('core/src/vram_guard.cpp').includes('EnumAdapters(i'), 'VRAM iterates adapters');
ok(src('core/src/hw_detect.cpp').includes('pick_tier'), 'tier picker implemented');
ok(src('core/include/present_core.h').includes('pv_detect_tier'), 'pv_detect_tier in C ABI');
ok(src('core/include/present_core.h').includes('pv_queue_pause') && src('core/include/present_core.h').includes('pv_abort_current') && src('core/include/present_core.h').includes('pv_queue_remove'), 'phase-2 controls in C ABI');
ok(src('core/include/present_core.h').includes('skip_summary'), 'skip_summary in job options');
ok(pipe.includes('checkpoint()') && pipe.includes('pv_queue_remove'), 'pipeline honors pause/abort/remove');
ok(pipe.includes('never silently overwrite'), 'output filenames uniquified');
ok(pipe.includes('day_cap <= 0'), 'day/class resolver guards buffers');
ok(src('pv-backend/src/queue.rs').includes('c.pause(false)'), 'abort releases held pause');
ok(pipe.includes('}  // namespace\n\n// ---- Worker control'), 'pv control block is top-level ::pv (not nested in anon namespace)');
ok(src('core/src/stt_whisper.cpp').includes('progress_hook'), 'STT reports per-window progress');
ok(src('core/src/llm_llama.cpp').includes('__ABORTED__'), 'LLM propagates abort');
  ok(src('core/src/llm_llama.cpp').includes('session_load(s, true') && src('core/src/llm_llama.cpp').includes('reopen CPU'), 'LLM Vulkan->CPU retry');
  ok(src('core/src/llm_llama.cpp').includes('prompt exceeds batch'), 'oversized prompts degrade instead of aborting');
  ok(src('core/src/pipeline.cpp').includes('vram after stt unload') && src('core/src/pipeline.cpp').includes('vram after llm unload'), 'unload-per-file lifecycle logged');
  ok(src('core/src/llm_llama.cpp').includes('llama_model_free(s->model)'), 'LLM freed after every use');
ok(src('core/src/stt_whisper.cpp').includes('whisper_free(ctx)'), 'STT frees model per file');
ok(src('core/src/audio_ffmpeg.cpp').includes('ffmpeg.exe'), 'decode uses bundled ffmpeg');
ok(src('core/src/stt_whisper.cpp').includes('PV_CPU_ONLY'), 'STT honors CPU-only hatch');
ok(src('core/src/llm_llama.cpp').includes('PV_CPU_ONLY'), 'LLM honors CPU-only hatch');
ok(src('core/src/internal.h').includes('backend.log'), 'backend file logging exists');
ok(src('core/src/internal.h').includes('stderr.log'), 'backend stderr captured to file');
ok(src('core/src/dllmain.cpp').includes('_set_invalid_parameter_handler'), 'silent-class handlers installed');
for (const f of ['pipeline', 'audio_ffmpeg', 'stt_whisper', 'llm_llama', 'diff', 'meta_day_class', 'db', 'vram_guard', 'hw_detect']) {
  const raw = src(`core/src/${f}.cpp`);
  const code = raw
    .replace(/\/\/.*/g, '')
    .replace(/R"\(([\s\S]*?)\)"/g, 'RS')
    .replace(/"(?:[^"\\\n]|\\.)*"/g, 'QQ');
  const bal = (ch1, ch2) => [...code].filter((x) => x === ch1).length === [...code].filter((x) => x === ch2).length;
  ok(bal('{', '}') && bal('(', ')'), `${f}.cpp braces balanced`);
}

// --- 5. Native shell contracts (cargo-packager NSIS + MSI) ---
ok(src('scripts/build.ps1').includes('dev/smoke-test.ps1'), 'one-go build runs the smoke test');
ok(src('scripts/build.ps1').includes('-Dev'), 'fast dev-profile build option');
ok(src('scripts/build.ps1').includes('cargo build --workspace --release'), 'one-go build compiles the workspace');
ok(src('scripts/build.ps1').includes('cargo packager --release'), 'one-go build packages installers');
ok(!src('scripts/build.ps1').includes('tauri'), 'build has no Tauri step');
ok(src('app/Cargo.toml').includes('[package.metadata.packager]'), 'packager metadata present');
ok(src('app/Cargo.toml').includes('"nsis"') && src('app/Cargo.toml').includes('"wix"'), 'NSIS + MSI formats');
ok(src('app/Cargo.toml').includes('installMode = "perMachine"'), 'NSIS is per-machine');
ok(src('app/Cargo.toml').includes('icacls'), 'NSIS grants Users write access (single-folder installs)');
ok(src('app/Cargo.toml').includes('ffmpeg-dlls/*') && src('app/Cargo.toml').includes('config/models.json'), 'bundled resources: ffmpeg + catalog');
ok(src('app/Cargo.toml').includes('assets/voice.ico'), 'installer icon wired');
// Installers ship program only: no scripts, no model weights.
ok(!src('app/Cargo.toml').match(/models\/\*|\.gguf|\.bin"/), 'no model weights in package resources');
ok(!src('app/Cargo.toml').includes('.ps1'), 'no scripts in package config');
ok(src('scripts/build.ps1').includes('ffmpeg-dlls/present_core.dll'), 'build stages core DLL for bundling');
// --- 5d. Link-only model onboarding (no copies; Ollama blobs load in place) ---
ok(existsSync('pv-backend/src/ollama.rs'), 'ollama scanner module exists');
ok(src('pv-backend/src/ollama.rs').includes('pub fn scan') && src('pv-backend/src/ollama.rs').includes('gguf_magic_ok'), 'scanner + GGUF gate');
ok(src('pv-backend/src/manifest.rs').includes('path: Option<String>'), 'manifest records link paths');
ok(src('app/src/store.rs').includes('link_ollama') && src('app/src/store.rs').includes('unlink_model'), 'store links/unlinks');
ok(!src('pv-backend/src/models.rs').includes('import_local_file'), 'copy-import deleted from backend');
ok(!src('app/src/store.rs').includes('import_model'), 'copy-import deleted from store');
// --- 5b. Assets + icons (reused by cargo-packager in M5) ---
ok(existsSync('assets/voice.ico'), 'source voice.ico in assets/');
ok(!existsSync('voice.ico'), 'no stray ico at root');
ok(existsSync('app/build.rs') && src('app/build.rs').includes('embed_resource'), 'exe icon embedded via build.rs');
ok(existsSync('assets/app.rc') && src('assets/app.rc').includes('voice.ico'), 'icon resource file points at voice.ico');
ok(src('app/Cargo.toml').includes('embed-resource = "=3.0.11"'), 'embed-resource pinned');
// --- 5c. Tidy: no duplicate WIN32_LEAN_AND_MEAN defines (CMake sets it globally) ---
for (const f of ['dllmain', 'audio_ffmpeg', 'meta_day_class', 'vram_guard', 'hw_detect', 'agent_guide']) {
  ok(!src(`core/src/${f}.cpp`).includes('#define WIN32_LEAN_AND_MEAN'), `${f}.cpp has no duplicate lean-and-mean define`);
}
// --- 5d. Hardware + agent guide ---
ok(src('core/src/hw_detect.cpp').includes('DedicatedVideoMemory'), 'VRAM detection falls back past zero Budget');
ok(src('core/src/vram_guard.cpp').includes('DedicatedVideoMemory'), 'VRAM guard falls back past zero Budget');
ok(src('core/src/pipeline.cpp').includes('day_cap <= 0'), 'day/class resolver guards buffers');
ok(existsSync('AGENTS.md'), 'AGENTS.md behavior contract exists');
for (const h of ['## 1.', '## 2.', '## 3.', '## 4.', '## 5.', 'CLASS:', 'TITLE:', 'Day N']) {
  ok(src('AGENTS.md').includes(h), `AGENTS.md covers ${h}`);
}
ok(src('core/src/agent_guide.cpp').includes('2000'), 'guide excerpt bounded');
ok(src('core/src/llm_llama.cpp').includes('agent_guide(cfg.guide_name)'), 'LLM calls carry the tier guide');
ok(src('core/src/internal.h').includes('agent_guide'), 'guide declared for pipeline');
// --- 5e. Theme package (toggle + desaturated neutrals + weights) ---
ok(src('app/src/theme.rs').includes('apply_theme'), 'theme apply entry exists');
ok(src('app/src/theme.rs').includes('ThemeMode::Dark') && src('app/src/theme.rs').includes('ThemeMode::Light'), 'both modes handled');
ok(!src('app/src/theme.rs').includes('0x1e1e2e') && !src('app/src/theme.rs').includes('0x1e_1e_2e'), 'blue-tinted navbar neutral gone');
ok(src('app/src/main.rs').includes('ToggleDarkMode'), 'toggle action registered');
ok(src('app/src/views_settings.rs').includes('theme'), 'theme toggle lives in Settings');
ok(src('app/src/views.rs').includes('nav_button("Settings"'), 'nav has Settings');
ok((src('app/src/views.rs').match(/font_weight/g) || []).length >= 1, 'nav buttons carry weights');
// --- 5f. Custom model path ---
ok(src('pv-backend/src/dirs.rs').includes('MODELS_OVERRIDE'), 'models-dir override exists');
ok(src('app/src/views_settings.rs').includes('models-browse'), 'Browse option in Settings page');
  ok(src('app/src/views_models.rs').includes('Set as Transcribing'), 'STT toggle button in Models page');
  ok(src('app/src/views_models.rs').includes('Set as Summarizing'), 'LLM toggle button in Models page');
  ok(src('app/src/views_models.rs').includes('h_flex'), 'models page uses side-by-side columns');
  ok(src('app/src/views_models.rs').includes('CPU only'), 'compute mode selector in Models page');
  ok(src('app/src/store.rs').includes('compute_mode') && src('pv-backend/src/prefs.rs').includes('compute_mode'), 'compute mode persisted');
  ok(src('app/src/views_settings.rs').includes('Delete converted'), 'delete-after toggle in Settings page');
  ok(src('core/src/audio_ffmpeg.cpp').includes('convert_to_wav_16k') && src('core/src/audio_ffmpeg.cpp').includes('read_wav_16k_mono'), 'audio-first convert + WAV reader in core');
ok(src('core/src/audio_audacity.cpp').includes('render_audacity_to_wav') && src('core/src/audio_audacity.cpp').includes('decode_binx'), 'native Audacity project import (binary-XML decode + mixdown, no vendored Audacity code)');
ok(src('core/CMakeLists.txt').includes('audio_audacity.cpp'), 'project importer compiled into the core DLL');
ok(src('app/src/store.rs').includes('PROJECT_EXTS') && src('app/src/views_input.rs').includes('PROJECT_EXTS'), 'project files stage as audio from picker + gate');
ok(src('pv-backend/src/share.rs').includes('reveal_in_folder') && src('pv-backend/src/share.rs').includes('copy_text'), 'reveal-in-folder + clipboard live in a per-OS backend module');
ok(src('app/src/views_input.rs').includes('warn-{site}-') && src('app/src/views_input.rs').includes('warn-banner-') && src('app/src/views_input.rs').includes('warn_button("row"'), 'warning badges carry per-site id prefixes (duplicate GPUI element ids panic the a11y tree)');
ok(src('app/src/views_input.rs').includes('on_drop') && src('app/src/views_input.rs').includes('ExternalPaths'), 'OS drag-and-drop lands on the Input page');
ok(src('app/src/views_output.rs').includes('copy_output_text') && src('app/src/views_output.rs').includes('reveal_output'), 'Output rows copy + reveal');
ok(src('core/src/pipeline.cpp').includes('srt_from_transcript'), 'video jobs emit an SRT sidecar from timestamped lines');
ok(src('app/src/store.rs').includes('spawn_boot_verify') && src('pv-backend/src/progress.rs').includes('BootVerified'), 'model integrity runs on a boot worker, Start never hashes');
ok(src('app/src/store.rs').includes('restore_staged') && src('pv-backend/src/prefs.rs').includes('staged'), 'staged queue persists across restarts');
ok(src('pv-backend/src/update.rs').includes('check_blocking') && src('app/src/views_settings.rs').includes('check-updates'), 'manual update check, user-initiated only');
ok(src('core/src/audio_ffmpeg.cpp').includes('qarg') && !src('core/src/audio_ffmpeg.cpp').includes('-i \\"'), 'ffmpeg spawn lines are quote-escaped (no raw -i interpolation)');
ok(src('core/src/audio_ffmpeg.cpp').includes('RF64') && src('core/src/audio_ffmpeg.cpp').includes('ds64'), 'WAV reader accepts RF64/ds64 with EOF clamp');
ok(src('core/src/stt_whisper.cpp').includes('initial_prompt') && src('core/src/pipeline.cpp').includes('part_seed_prompt'), 'split takes seed Part N from Part N-1 transcript tail');
  ok(src('app/src/main.rs').includes('#![windows_subsystem = "windows"]'), 'dev build suppresses console window');
  ok(src('app/src/main.rs').includes('MessageBoxW') && src('app/src/main.rs').includes('crash.log'), 'crash popup + crash log in entry point');
  ok(src('app/src/main.rs').includes('SetUnhandledExceptionFilter') && src('app/src/main.rs').includes('MiniDumpWriteDump'), 'native crashes dump + popup');
  ok(src('pv-backend/src/progress.rs').includes('Validate'), 'validation events exist');
  ok(src('core/include/present_core.h').includes('pv_validate_stt') && src('core/include/present_core.h').includes('pv_validate_llm'), 'validation in C ABI');
  ok(src('core/include/present_core.h').includes('cpu_only') && src('pv-backend/src/core_bridge.rs').includes('cpu_only'), 'compute mode travels explicitly, never via env');
  ok(src('app/src/store.rs').includes('validating') && src('app/src/views_input.rs').includes('Loading models'), 'Start waits on model load');

// --- 6. Pipeline failure audit (no silent wedges, no ghosts, no panics) ---
{
  const store = src('app/src/store.rs');
  const errArm = store.slice(store.indexOf('Stage::Error =>'));
  ok(errArm.slice(0, 1600).includes('processing.retain'), 'backend errors retire from processing');
  ok(errArm.slice(0, 1600).includes('file_warnings'), 'backend errors stay retryable');
  ok(errArm.slice(0, 1600).includes('push_error'), 'backend errors surface in Error Center');
  const drained = store.slice(store.indexOf('fn check_drained'));
  ok(drained.slice(0, 600).includes('started = false') && drained.slice(0, 600).includes('backend_live = false'), 'drain re-arms run flags');
  ok(!store.includes('output_remove(&o.md_name)?'), 'delete never aborts on missing files');
  const unreadable = store.slice(store.indexOf('Done but unreadable'));
  ok(unreadable.slice(0, 700).includes('file_warnings'), 'unreadable finishes retire, never ghost');
  const mains = (src('app/src/main.rs').match(/\.expect\(/g) || []).length;
  ok(mains === 1, 'single accepted expect (window open, covered by crash hook)');
  const pipe = src('core/src/pipeline.cpp');
  ok((pipe.match(/pv::pv_log\(/g) || []).length >= 6, 'run_one error branches all log to file');
  ok(pipe.includes('write_file(md, body)') && pipe.includes('return -4'), 'md writes verified, failures are errors not ghosts');
  ok(src('core/src/stt_whisper.cpp').includes('#ifdef PV_HAVE_WHISPER') && src('core/src/stt_whisper.cpp').includes('mock transcript'), 'mock STT stays behind its build flag');
  ok(src('core/src/llm_llama.cpp').includes('#ifdef PV_HAVE_LLAMA'), 'mock LLM stays behind its build flag');
  ok(src('core/src/validate.cpp').includes('catch (...)') && src('core/src/pipeline.cpp').includes('NATIVE EXCEPTION'), 'vendor throws convert to errors, never cross FFI');
  ok(src('core/src/stt_whisper.cpp').includes('tdrz_enable') && src('core/src/stt_whisper.cpp').includes('speaker_turn_next'), 'tinydiarize speaker turns in STT');
  ok(src('config/models.json').includes('small.en-tdrz'), 'diarization model in catalog');
  ok(src('pv-backend/src/prefs.rs').includes('dismissed'), 'dismissed rows persist across restarts');
}

// --- 7. Script contracts ---
ok(src('scripts/setup-windows.ps1').includes("ffmpeg.exe"), 'setup stages ffmpeg.exe');
ok(src('scripts/setup-windows.ps1').includes('git init'), 'setup inits git for submodules');
ok(src('scripts/fetch-models.ps1').includes('size mismatch'), 'fetch verifies sizes');
ok(existsSync('.github/workflows/release.yml'), 'release CI exists');
ok(!src('.github/workflows/release.yml').includes('package-offline'), 'CI has no offline-packaging step');
// --- 7a. Settings-centralized toggles + review drafts + docs/merge/error-center ---
ok(existsSync('app/src/views_settings.rs'), 'Settings page exists');
ok(src('app/src/store.rs').includes('Settings,') && src('app/src/views.rs').includes('Page::Settings'), 'Settings in Page nav');
ok(src('app/src/store.rs').includes('pub const DOC_EXTS') || src('app/src/store.rs').includes('DOC_EXTS'), 'document gate in store');
ok(src('app/src/views_input.rs').includes('pick_doc_files') || src('app/src/views_input.rs').includes('Add documents'), 'document picker on Input page');
ok(!src('app/src/views_input.rs').includes('Delete converted') && !src('app/src/views_models.rs').includes('compute_row('), 'toggles moved out of toolbars');
ok(existsSync('pv-backend/src/docs.rs') && src('pv-backend/src/docs.rs').includes('extract_text'), 'document extractor module');
ok(existsSync('pv-backend/src/merge.rs') && src('pv-backend/src/merge.rs').includes('tfidf_cosine'), 'similarity merge module');
ok(src('pv-backend/src/merge.rs').includes('## Sources'), 'merged notes put Sources on top');
ok(existsSync('pv-backend/src/drafts.rs') && src('pv-backend/src/drafts.rs').includes('index.json'), 'review drafts folder-per-output + index.json');
ok(existsSync('pv-backend/src/diag.rs') && src('pv-backend/src/diag.rs').includes('tail_logs'), 'diagnostics log tails');
ok(src('app/src/store.rs').includes('push_error') && src('app/src/views_input.rs').includes('Errors ('), 'Error Center in GUI');
ok(src('core/include/present_core.h').includes('is_text'), 'text-payload flag in C ABI');
ok(src('core/src/pipeline.cpp').includes('is_text') && src('core/src/pipeline.cpp').includes('Reading text'), 'core skips STT for text payloads');
ok(src('app/src/store.rs').includes('default_classes') && src('app/src/store.rs').includes('run_outdir'), 'Start reads Settings defaults');
ok(src('app/src/store.rs').includes('draft_badges') && src('app/src/views_output.rs').includes('draft_badges'), 'closed drafts badge via index.json');
ok(!src('app/src/views_output.rs').includes('this.read(cx)'), 'no self-entity reads in OutputView (pure render, no double-lease)');
ok(src('app/src/views_output.rs').includes('build_editors') && !src('app/src/views_output.rs').includes('sync_editors'), 'editors built in event handlers only');
ok(src('app/src/views_settings.rs').includes('settings-scroll'), 'settings page scrolls to fit window');
ok(src('app/src/views_input.rs').includes('input-scroll') && src('app/src/views_models.rs').includes('models-scroll') && src('app/src/views_output.rs').includes('output-scroll'), 'input/models/output pages scroll inside the bounded viewport');ok(!src('app/src/views_output.rs').includes('px(560.)'), 'no fixed pixel heights in page scroll containers');
ok(src('app/src/views.rs').includes('size_full') && src('app/src/views.rs').includes('overflow_hidden'), 'root bounds the viewport so pages scroll instead of growing');
ok(src('app/src/main.rs').includes('window_min_size'), 'window collapses to title-bar size by design');
ok((src('pv-backend/src/record.rs').includes('bits_per_sample: 24') || src('pv-backend/src/record.rs').includes('Rf64Writer')) && src('pv-backend/src/rf64.rs').includes('w16(&mut h, 24)'), 'record takes are 24-bit WAV (RF64)');
ok(src('app/src/views_record.rs').includes('record-scroll') && src('app/src/views.rs').includes('Page::Record'), 'Record page separate from Transcribe with transport + scroll');
ok(src('app/src/store.rs').includes('goto_page') && src('app/src/store.rs').includes('rec_auto_stage'), 'leaving Record auto-stages unsent takes to Input');
ok(src('app/src/store.rs').includes('rec_shutdown'), 'quit finalizes live captures without deleting takes');
ok(!src('core/src/meta_day_class.cpp').includes('Day ?'), 'no Day ? fallback (contract: Day N / ISO / date / Day 1)');
ok(src('core/src/meta_day_class.cpp').includes('"Day 1"'), 'dateless files fall back to Day 1');
ok(src('core/src/pipeline.cpp').includes('strip_topic_lead'), 'topics strip LLM header/bullet leads');
ok(src('core/src/pipeline.cpp').includes('lc2.include_guide = false'), 'class inference excludes the guide');
ok(src('core/src/llm_llama.cpp').includes('strip_guide_echo'), 'guide echoes stripped from summaries');
ok(src('core/src/llm_llama.cpp').includes('no generation headroom'), 'generation clamped to batch/ctx headroom');
ok(src('core/include/present_core.h').includes('PV_ABI_VERSION') && src('pv-backend/src/core_bridge.rs').includes('pv_abi_version'), 'ABI handshake guards mixed exe/DLL installs');
ok(src('app/src/main.rs').includes('GetLastError'), 'minidump failures log their reason');
ok(src('app/src/main.rs').includes('on_app_quit'), 'all quit paths abort the backend before exit');
ok(src('app/src/main.rs').includes('on_window_should_close'), 'window chrome close aborts before teardown (app-quit hook never fires there)');
ok(src('app/src/store.rs').includes('shutdown_prepare') && src('app/src/store.rs').includes('cancel_downloads'), 'single quit entry flushes drafts/prefs/downloads before abort');
ok(src('pv-backend/src/queue.rs').includes('abort_and_join'), 'quit joins the worker (joinable thread at exit terminates)');
ok(src('pv-backend/src/core_bridge.rs').includes('ManuallyDrop'), 'DLL never unloads mid-worker');
ok(src('core/src/pipeline.cpp').includes('pv_abort_and_join'), 'bounded abort-and-join with detach on timeout');
ok(src('app/src/main.rs').includes('into_inner'), 'poisoned store lock still aborts on quit');
ok(src('app/src/main.rs').includes('shutdown_trace'), 'shutdown progress traced for log-less close crashes');
ok(src('core/src/pipeline.cpp').includes("t.back() == '-'"), 'titles never end on a dangling dash');
ok(src('pv-backend/src/diag.rs').includes('sweep_empty_dumps'), '0-byte dumps swept at boot');
ok(src('core/src/internal.h').includes('pv_rotate_if_huge'), 'runaway logs rotate');
ok(src('core/src/pipeline.cpp').includes('g_file_idx'), 'core file index is global across runs');
ok(src('pv-backend/src/core_bridge.rs').includes('remember_path'), 'order history is append-only (no index shift)');
ok(src('core/src/llm_llama.cpp').includes('llm_session_open') && src('core/src/llm_llama.cpp').includes('llm_session_generate'), 'persistent LLM session per file');
ok(src('core/src/llm_llama.cpp').includes('Condensing summary (split)'), 'hierarchical REDUCE for long notes');
ok(src('core/include/present_core.h').includes('PV_ABI_VERSION 8'), 'C ABI at version 8 (phase-2 transcript path)');
ok(src('core/include/present_core.h').includes('summary_tier'), 'summary tier travels the job ABI');
ok(src('core/include/present_core.h').includes('audio_retention') && src('pv-backend/src/core_bridge.rs').includes('audio_retention'), 'retention travels the job ABI (videos exempt)');
ok(src('core/include/present_core.h').includes('denoise_mode'), 'denoise mode travels the job ABI');
ok(src('core/src/audio_ffmpeg.cpp').includes('denoise_for_stt'), 'adaptive gate runs on the STT copy only');
ok(src('core/src/llm_llama.cpp').includes('grammar_polish'), 'grammar stage shares the persistent session');
ok(src('core/src/llm_llama.cpp').includes('grammar_fallbacks'), 'word guard keeps drifted chunks verbatim');
ok(src('core/src/pipeline.cpp').includes('## Audio'), 'archived sources named beside the output');
ok(src('app/src/store.rs').includes('set_audio_retention') && src('app/src/store.rs').includes('set_denoise_mode'), 'retention + denoise setters persisted');
ok(src('pv-backend/src/research.rs').includes('budget_for') && src('pv-backend/src/research.rs').includes('notes.md'), 'opt-in research fetches cited sources into notes.md');
ok(src('app/src/views_settings.rs').includes('Research (web, opt-in)') && src('app/src/store.rs').includes('set_web_research'), 'research toggle plainly visible, default off');
ok(src('app/src/views_settings.rs').includes('Slider::new') && src('app/src/store.rs').includes('set_attention'), 'attention slider with detents drives video depth');
ok(src('app/src/views_input.rs').includes('add-video') && src('app/src/store.rs').includes('VIDEO_EXTS'), 'Add video intake separate from Transcribe');
ok(src('core/src/stt_whisper.cpp').includes('stamp_mmss'), 'video STT emits sentence-grouped timestamps');
ok(src('core/src/audio_ffmpeg.cpp').includes('sample_video_frames'), 'attention-driven frame sampling with exact timestamps');
ok(src('core/src/llm_llama.cpp').includes('tier_video_prompt'), 'film-watch REDUCE shape (occurrences/themes/opinion/summary)');
ok(src('core/src/pipeline.cpp').includes('gather_folder_context'), 'video-only folder theme compare (proposals, never merges)');
ok(src('app/src/store.rs').includes('complete_phase1') && src('pv-backend/src/progress.rs').includes('Research'), 'phase-2 research grounds queries in the pass-1 transcript');
ok(src('config/models.json').includes('vision_models'), 'vision catalogue present');
ok(src('pv-backend/src/catalog.rs').includes('vision_entry'), 'two-file vision entries resolve');
ok(src('pv-backend/src/download.rs').includes('start_vision'), 'vision downloads fetch weights + projector under one id');
ok(src('pv-backend/src/models.rs').includes('active_vlm'), 'vision inventory + active slot tracked');
ok(src('core/include/present_core.h').includes('PV_ABI_VERSION 8'), 'C ABI carries the phase-2 transcript path');
ok(src('core/include/present_core.h').includes('transcript_path'), 'pass-1 transcript travels the job ABI');
ok(src('core/src/pipeline.cpp').includes('Transcript provided, skipping STT'), 'phase-2 jobs skip the audio front-end');
ok(src('core/include/present_core.h').includes('vlm_text_model'), 'vision paths travel the job ABI');
ok(src('core/src/vlm_qwen.cpp').includes('vlm_caption'), 'mtmd caption engine present');
ok(src('core/src/pipeline.cpp').includes('sweep_frames_dir(frames_dir)'), 'temp frames sweep once captions land');
ok(src('app/src/views_models.rs').includes('Vision — film frames'), 'Vision column on Models page');
ok(src('app/src/views_settings.rs').includes('vlm_line'), 'Documentary section shows active vision model');
ok(src('core/src/vlm_qwen.cpp').includes('init_penalties'), 'caption sampler breaks repetition loops');
ok(src('core/src/pipeline.cpp').includes('## Frame captions'), 'vision evidence survives extractive fallback');
ok(src('core/src/pipeline.cpp').includes('basis_full'), 'titles derive from prose, not caption fragments');
ok(src('core/include/present_core.h').includes('chunk_tokens'), 'chunks measured in AI tokens');
ok(src('pv-backend/src/prefs.rs').includes('summary_tier'), 'summary tier persisted');
ok(src('app/src/store.rs').includes('set_summary_tier') && src('app/src/store.rs').includes('tier_ratio_pct'), 'tier drives chunk preset + ratio');
ok(src('app/src/views_settings.rs').includes('sumtier'), 'length tier selector in Settings');
ok(existsSync('config/guides/recap.md') && existsSync('config/guides/standard.md') && existsSync('config/guides/detailed.md'), 'per-tier guides exist');
ok(src('core/src/agent_guide.cpp').includes('agent_guide(const std::string& name)'), 'tier guides load beside the exe');
ok(src('app/Cargo.toml').includes('config/guides/'), 'tier guides ship with installers');
ok(src('core/src/llm_llama.cpp').includes('sentence_dup_ratio'), 'repetition relaxes the length target');
ok(src('core/src/llm_llama.cpp').includes('split_by_tokens'), 'MAP splits by token estimate');
ok(src('core/src/pipeline.cpp').includes('Coverage:'), 'summaries carry an explicit Coverage line');
ok(src('core/src/pipeline.cpp').includes('sample_head_tail'), 'CLASS/topic sample head+tail, not head-only');
ok(src('app/src/store.rs').includes('sidecar_int'), 'coverage sidecar parsed for the status line');
ok(src('core/src/pipeline.cpp').includes('SPILL_FLOATS') && src('core/src/stt_whisper.cpp').includes('stt_transcribe_spilled'), 'long audio spills PCM to disk');
ok(src('core/src/vram_guard.cpp').includes('0x1414') && src('core/src/hw_detect.cpp').includes('0x1414'), 'software adapters excluded from GPU readings');
ok(src('app/src/main.rs').includes('rotate_crash_log_if_huge'), 'crash.log rotates like backend logs');
ok(src('app/src/views_settings.rs').includes('TdrDelay'), 'TDR guidance in Diagnostics');
ok(src('app/src/store.rs').includes('clean_caches') && src('app/src/views_settings.rs').includes('clean-caches'), 'cache pruning with Settings button');
ok(src('app/src/store.rs').includes('strip_prefix("Merged")'), 'merged titles strip existing prefix (no double Merged)');
ok(src('app/src/store.rs').includes('dismissed_groups'), 'dismissed merge groups persist');
// --- 7b. Scripts run from any CWD (sentinel walk-up; safe under orchestration) ---
for (const s of ['build', 'setup-windows', 'fetch-models', 'dev/smoke-test', 'dev/measure-models']) {
  ok(src(`scripts/${s}.ps1`).includes('repo root not found above'), `${s}.ps1 is CWD-independent`);
}
ok(!existsSync('scripts/build-all.ps1'), 'legacy build-all.ps1 removed (use build.ps1)');
ok(!existsSync('scripts/smoke-test.ps1') && !existsSync('scripts/measure-models.ps1'), 'test scripts live in scripts/dev/');
ok(src('scripts/build.ps1').includes('Dev staging'), 'build stages backend beside dev binaries');

// --- 8. Conservative supply-chain gates (NIST SSDF PS/RV lite) ---
ok(src('.github/workflows/release.yml').includes('cargo audit'), 'CI runs cargo audit (RUSTSEC)');
ok(existsSync('deny.toml') && src('deny.toml').includes('[advisories]'), 'deny.toml states the dependency policy');
ok(src('.github/workflows/release.yml').includes('cargo deny check'), 'CI enforces the deny policy');
ok(src('.github/workflows/release.yml').includes('Pin gate'), 'CI pin gate fails tags closed on empty sha256');
ok(src('.github/workflows/release.yml').includes('SHA256SUMS'), 'CI publishes SHA256SUMS');
ok(src('.github/workflows/release.yml').includes('cyclonedx') || src('.github/workflows/release.yml').includes('SBOM'), 'CI generates SBOM');
ok(existsSync('.github/dependabot.yml'), 'dependabot watches cargo + actions');
ok(existsSync('SECURITY.md') && src('SECURITY.md').includes('sha256'), 'SECURITY.md states the pin policy');
ok(src('scripts/build.ps1').includes('PV_UPDATE_FEED') && src('scripts/build.ps1').includes('SHA256SUMS'), 'packaging fails feed+empty-pins closed, always writes SHA256SUMS');
ok(src('pv-backend/src/models.rs').includes('expected.is_empty()'), 'Models page badges unpinned entries per-entry (not just sidecar)');
ok(src('pv-backend/src/verify.rs').includes('gguf_magic_ok'), 'boot verify checks GGUF magic on linked blobs');
ok(src('pv-backend/src/verify.rs').includes('size-only check'), 'boot warns when active models are size-only');
ok(src('pv-backend/src/ollama.rs').includes('verify_link_blocking'), 'linked blobs hash-verify on a worker');
ok(src('pv-backend/src/manifest.rs').includes('set_link_verified'), 'link verification persists a verified flag');
ok(src('app/src/store.rs').includes('LinkVerified'), 'link-hash results fold into the store');
ok(existsSync('docs/THREAT-MODEL.md') && src('docs/THREAT-MODEL.md').includes('STRIDE'), 'STRIDE threat model present');
ok(src('pv-backend/src/download.rs').includes('verified_complete'), 'complete downloads hash-verify before the network is skipped');
ok(src('pv-backend/src/download.rs').includes('range_start_mismatch'), '206 resumes validate the server window before appending');
ok(src('pv-backend/src/download.rs').includes('downloaded > total'), 'overshot prefixes restart clean instead of failing the retry');

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
