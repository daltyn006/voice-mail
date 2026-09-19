# Threat model — voice mail (STRIDE, 2026-09-18)

Scope: the offline Windows desktop app and its local media. Out of scope:
HuggingFace / Ollama upstream integrity, installer PKI beyond Authenticode +
`SHA256SUMS`, and `%LOCALAPPDATA%` ACLs (see `security.txt` open item 6).

## Data-flow (text DFD)

```
[Mic / files / videos / docs / projects] --(1)--> [Input page / Record]
[HF Hub] --(2, user-initiated)--> [Downloader (.part resume)] --(3)--> [models/ + manifest]
[Ollama store] --(4, link in place)--> [manifest link record]
[models/] --(5)--> [core DLL: ffmpeg decode -> STT -> LLM/VLM -> diff] --(6)--> [out dir .md + .json + app.db]
[opt-in research] <--(7, user-initiated)--> [web: DDG + capped page fetches]
[Settings] --> [prefs.json / staged queue / drafts]
```

Trust boundaries: (a) process edge — every file read; (b) network edge —
HF downloads + research fetches + update feed, all user-initiated;
(c) FFI edge — `present_core.dll` C ABI with version handshake.

## STRIDE per element

| # | Element | S | T | R | I | D | E | Mitigation (file) |
|---|---|---|---|---|---|---|---|---|
| 1 | Audio/video/doc/project intake | – | hostile WAV/RF64 headers, binx bombs, XXE-ish XML | – | path traversal, ADS, symlink escape | multi-GB OOM | – | u64 RF64 parse + EOF clamp + 32 GiB cap (`audio_ffmpeg.cpp`, `rf64.rs`); binx node/depth caps + single-pass normalize; `confine` + canonicalize-and-stay-put + UNC/removable/ADS blocks (`queue.rs`, `paths.rs`); chunked 64 MiB reads; spill past 32 M floats |
| 2 | Model download | – | bitrot / truncated weights | – | hash mismatch silently trusted | disk-full wedge | – | SHA-256 pins, hard-fail + delete on mismatch (`download.rs::check_pinned`); `verified_complete` re-hash before skip; overshoot restart + `Content-Range` guard (`fetch_one`); sticky RF64/disk-full errors; manifest records hash |
| 3 | Boot verify | TOCTOU verify→load | quarantined file reappears | – | – | – | – | quarantine to `*.corrupted` + tier fallback (`verify.rs`); re-stat at load (`models.rs::resolve_one`); file locking deferred (accepted risk) |
| 4 | Ollama link | blob rotated under us | manifest digest lies | – | – | – | – | magic + size at link, GGUF re-check at boot, background full-hash worker drops mismatches (`ollama.rs::verify_link_blocking`, `LinkVerified` event) |
| 5 | ffmpeg spawn | – | – | – | command injection via filenames | – | – | `qarg()` quoting on all command lines, no shell (`audio_ffmpeg.cpp`) |
| 6 | Research fetch | SSRF to file:/LAN | malicious page → notes | – | creds leak in URL | unbounded fetch hangs run | – | http(s)-only + host required + no `@` userinfo (`research.rs::url_allowed`); 15 s timeout + 512 KiB cap; fenced `## External context`, film facts take precedence; default OFF |
| 7 | Update check | feed spoof → fake "update" text | – | – | version string exfil (none sent) | polling drains battery | – | manual-only, baked `PV_UPDATE_FEED`, 15 s timeout, text-only result (`update.rs`) |
| 8 | FFI / DLL | stale-DLL struct confusion | vendor throw across FFI | – | – | worker leak at quit | – | `PV_ABI_VERSION` handshake; `catch_unwind` + `catch (...)` → errors (`core_bridge.rs`, `validate.cpp`); `ManuallyDrop` never-unload; bounded abort-and-join |
| 9 | Queued outputs | – | – | silent overwrite of notes | traversal out of out dir | – | – | never-overwrite uniquify (`pipeline.cpp`); `ParentDir` confine (`queue.rs`); retention deletes only on success |
| 10 | Crash handling | – | – | lost diagnostics | dump contains transcript text | log disk exhaustion | – | minidump + `crash.log` stay local, 0-byte sweep, `pv_rotate_if_huge` |

Legend: S spoofing, T tampering, R repudiation, I information disclosure,
D denial of service, E elevation of privilege.

## Residual risks (accepted, tracked in security.txt)

1. Empty `sha256` pins → size-only enforcement until the Hub-OID diff lands.
2. Verify→load TOCTOU (no file locking) — hostile local actor only.
3. Sidecar override is UI-convention, not a cryptographic gate.
4. Installer-time (NSIS/WiX, data-dir ACLs) out of scope of this document.

## Review cadence

Revisit quarterly alongside the `PINS.md` track review, and on any new
intake format, network egress, or FFI surface change.
