//! Input page: staged files (audio + documents), Start (Settings defaults),
//! live progress, merge suggestions/prompts, and the Error Center.
//!
//! All toggles live in Settings; this page keeps run controls only.
//! State lives in `Store`; this view owns no data.

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::ExternalPaths;
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::{div, px, rgba, Context, Entity, FontWeight, IntoElement, Render, Window};

use crate::store::{InputFile, ProcFile, Store, ViewMode, AUDIO_EXTS, DOC_EXTS, PROJECT_EXTS, VIDEO_EXTS};
use crate::theme;

pub struct InputView {
    store: Entity<Store>,
}

impl InputView {
    pub fn assemble(store: Entity<Store>) -> Self {
        InputView { store }
    }
}

fn pick_audio_files() -> Option<Vec<std::path::PathBuf>> {
    // Audio/Video containers plus Audacity-family projects (rendered
    // natively by the core before the normal STT path).
    let mut kinds: Vec<&str> = Vec::with_capacity(AUDIO_EXTS.len() + PROJECT_EXTS.len());
    kinds.extend_from_slice(AUDIO_EXTS);
    kinds.extend_from_slice(PROJECT_EXTS);
    rfd::FileDialog::new()
        .set_title("Add audio files (single or batch)")
        .add_filter("Audio/Video/Project", &kinds)
        .pick_files()
}

fn pick_doc_files() -> Option<Vec<std::path::PathBuf>> {
    rfd::FileDialog::new()
        .set_title("Add documents (single or batch)")
        .add_filter("Documents", DOC_EXTS)
        .add_filter("All files", &["*"])
        .pick_files()
}

/// Video intake (separate from Transcribe): containers are read in place —
/// never copied into output, never deleted. The pipeline runs timestamped
/// STT on the audio track plus frame sampling and opt-in research.
fn pick_video_files() -> Option<Vec<std::path::PathBuf>> {
    rfd::FileDialog::new()
        .set_title("Add video (watched in place — never copied or deleted)")
        .add_filter("Video", VIDEO_EXTS)
        .pick_files()
}

fn start_button(
    started: bool,
    loading: bool,
    compute: &str,
    store: Entity<Store>,
) -> impl IntoElement {
    let tag = if compute == "cpu" { "CPU" } else { "GPU" };
    let (label, icon) = if loading {
        (format!("Loading models ({tag})…"), IconName::LoaderCircle)
    } else if started {
        ("Started".to_string(), IconName::Check)
    } else {
        ("Start".to_string(), IconName::Play)
    };
    Button::new("start")
        .label(label)
        .icon(icon)
        .font_weight(FontWeight::SEMIBOLD)
        .primary()
        .on_click(move |_, _, cx| {
        let rx = {
            let res = store.update(cx, |s, _| s.begin_run());
            match res {
                Ok(rx) => rx,
                Err(e) => {
                    store.update(cx, |s, cx| {
                        s.push_error("Input", e, String::new());
                        cx.notify();
                    });
                    return;
                }
            }
        };
        crate::store::spawn_pump(store.clone(), rx, cx);
    })
}

fn delete_button(id: u64, store: Entity<Store>) -> impl IntoElement {
    Button::new(format!("del-{id}"))
        .icon(IconName::X)
        .tooltip("Remove from queue")
        .on_click(move |_, _, cx| {
            store.update(cx, |s, cx| {
                s.remove_input(id);
                cx.notify();
            });
        })
}

fn merge_check(id: u64, checked: bool, store: Entity<Store>) -> impl IntoElement {
    Button::new(format!("mg-{id}"))
        .icon(if checked {
            IconName::SquareCheck
        } else {
            IconName::Square
        })
        .tooltip("Toggle merge selection")
        .font_weight(FontWeight::SEMIBOLD)
        .on_click(move |_, _, cx| {
            store.update(cx, |s, cx| {
                s.toggle_merge_select(id);
                cx.notify();
            });
        })
}

fn fmt_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Warning badge (triangle icon shows `reason` in the status line). `site` disambiguates
/// the element id: the same path renders in the banner AND in its staged
/// card within one frame, and duplicate GPUI element ids panic the a11y tree
/// (fatal here — debug-assertions stay on in release, see PINS.md).
fn warn_button(site: &str, path_str: String, reason: String, store: Entity<Store>) -> gpui_kit::AnyElement {
    Button::new(format!("warn-{site}-{path_str}"))
        .icon(IconName::TriangleAlert)
        .tooltip("Show warning")
        .font_weight(FontWeight::SEMIBOLD)
        .on_click(move |_, _, cx| {
            let _ = store.update(cx, |s, cx| {
                s.status = format!("{}: {}", path_str, reason);
                cx.notify();
            });
        })
        .into_any_element()
}

fn short_name(f: &InputFile) -> String {
    let full = f.path.to_string_lossy().to_string();
    f.path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or(full)
}

fn kind_glyph(f: &InputFile) -> IconName {
    match f.kind {
        crate::store::InputKind::Doc => IconName::FileText,
        crate::store::InputKind::Video => IconName::Clapperboard,
        _ => IconName::AudioLines,
    }
}

/// Icons mode: square tile with a file preview (glyph + type + name).
fn staged_tile(
    f: &InputFile,
    warning: Option<String>,
    checked: bool,
    store: Entity<Store>,
    theme: &str,
    hc: bool,
) -> impl IntoElement {
    let path_str = f.path.to_string_lossy().to_string();
    let ext = f
        .path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_uppercase();
    let mut tile = theme::card(theme, hc)
        .gap(px(theme::SPACE_XS))
        .min_w(px(140.))
        .flex_1()
        .child(div().child(kind_glyph(f)).font_weight(FontWeight::BOLD))
        .child(
            div()
                .child(if ext.is_empty() {
                    "file".to_string()
                } else {
                    ext
                })
                .font_weight(FontWeight::SEMIBOLD),
        )
        .child(
            div()
                .truncate()
                .child(short_name(f))
                .font_weight(FontWeight::SEMIBOLD),
        )
        .child(div().child(format!("{} · {}", fmt_size(f.size), f.state.label())));
    tile = tile.child(
        h_flex()
            .gap_2()
            .child(merge_check(f.id, checked, store.clone()))
            .child(match warning {
                Some(reason) => warn_button("row", path_str, reason, store.clone()),
                None => div().into_any_element(),
            })
            .child(delete_button(f.id, store)),
    );
    tile.into_any_element()
}

/// List mode: compact fixed-width rows that flow into multiple columns.
fn staged_list_row(
    f: &InputFile,
    warning: Option<String>,
    checked: bool,
    store: Entity<Store>,
    theme: &str,
    hc: bool,
) -> impl IntoElement {
    let path_str = f.path.to_string_lossy().to_string();
    h_flex()
        .gap(px(theme::SPACE_SM))
        .p_1()
        .flex_1()
        .min_w(px(200.))
        .bg(rgba(theme::surface(theme, hc)))
        .border_1()
        .border_color(rgba(theme::border(theme, hc)))
        .rounded(px(6.0))
        .child(div().child(kind_glyph(f)).font_weight(FontWeight::BOLD))
        .child(
            div()
                .flex_1()
                .truncate()
                .child(short_name(f))
                .font_weight(FontWeight::SEMIBOLD),
        )
        .child(merge_check(f.id, checked, store.clone()))
        .child(match warning {
            Some(reason) => warn_button("row", path_str, reason, store.clone()),
            None => div().into_any_element(),
        })
        .child(delete_button(f.id, store))
        .into_any_element()
}

/// Details mode: one full-width explorer-like row per file.
fn staged_details_row(
    f: &InputFile,
    warning: Option<String>,
    checked: bool,
    store: Entity<Store>,
    theme: &str,
    hc: bool,
) -> impl IntoElement {
    let path_str = f.path.to_string_lossy().to_string();
    h_flex()
        .gap(px(theme::SPACE_SM))
        .flex_wrap()
        .p_1()
        .bg(rgba(theme::surface(theme, hc)))
        .border_1()
        .border_color(rgba(theme::border(theme, hc)))
        .rounded(px(6.0))
        .child(div().child(kind_glyph(f)).font_weight(FontWeight::BOLD))
        .child(
            div()
                .flex_1()
                .truncate()
                .child(path_str.clone())
                .font_weight(FontWeight::SEMIBOLD),
        )
        .child(div().child(fmt_size(f.size)).font_weight(FontWeight::SEMIBOLD))
        .child(div().child(f.state.label()).font_weight(FontWeight::SEMIBOLD))
        .child(merge_check(f.id, checked, store.clone()))
        .child(match warning {
            Some(reason) => warn_button("row", path_str, reason, store.clone()),
            None => div().into_any_element(),
        })
        .child(delete_button(f.id, store))
        .into_any_element()
}

fn staged_card(
    f: &InputFile,
    warning: Option<String>,
    checked: bool,
    mode: ViewMode,
    store: Entity<Store>,
    theme: &str,
    hc: bool,
) -> gpui_kit::AnyElement {
    match mode {
        ViewMode::Large => staged_tile(f, warning, checked, store, theme, hc).into_any_element(),
        ViewMode::List => staged_list_row(f, warning, checked, store, theme, hc).into_any_element(),
        ViewMode::Details => staged_details_row(f, warning, checked, store, theme, hc).into_any_element(),
    }
}

fn active_file_card(
    pf: &ProcFile,
    store: Entity<Store>,
    theme: &str,
    hc: bool,
) -> impl IntoElement {
    let id = pf.id;
    let store_c = store.clone();
    let store_p = store.clone();
    let store_a = store.clone();
    let store_s = store.clone();
    let store_r = store.clone();

    // Store holds 0..100 percentages (see apply_event); clamp defensively.
    let pct_stt = pf.stt.clamp(0.0, 100.0).round() as u32;
    let pct_sum = pf.sum.clamp(0.0, 100.0).round() as u32;

    v_flex()
        .gap(px(theme::SPACE_XS))
        .p_2()
        .bg(rgba(theme::surface(theme, hc)))
        .border_1()
        .border_color(rgba(theme::border(theme, hc)))
        .rounded(px(6.0))
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .child(format!("{}", pf.name))
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(
                    Button::new(format!("xproc-{id}"))
                        .icon(IconName::X)
                        .tooltip("Remove")
                        .on_click(move |_, _, cx| {
                            let active = store_c.read(cx).processing.first().map(|p| p.id) == Some(id);
                            store_c.update(cx, |s, cx| {
                                let _ = s.remove_processing(id, active);
                                cx.notify();
                            });
                        }),
                ),
        )
        .child(Progress::new(format!("stt-{}", id)).value(pf.stt.clamp(0.0, 100.0)))
        .child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(div().child(format!("Voice→Text: {pct_stt}%")).font_weight(FontWeight::SEMIBOLD))
                .child(Progress::new(format!("sum-{}", id)).value(pf.sum.clamp(0.0, 100.0)))
                .child(div().child(format!("Text→Summary: {pct_sum}%")).font_weight(FontWeight::SEMIBOLD)),
        )
        .child(div().child(pf.stage.clone()).font_weight(FontWeight::SEMIBOLD))
        .child(div().child(pf.msg.clone()).font_weight(FontWeight::SEMIBOLD))
        .child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(
                    Button::new(format!("pause-{id}"))
                        .icon(IconName::Pause)
                        .tooltip("Pause")
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            store_p.update(cx, |s, cx| {
                                s.set_paused(true);
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new(format!("abort-{id}"))
                        .label("Abort")
                        .icon(IconName::OctagonX)
                        .danger()
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            store_a.update(cx, |s, cx| {
                                let _ = s.remove_processing(id, true);
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new(format!("skip-{id}"))
                        .label("Skip")
                        .icon(IconName::SkipForward)
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            store_s.update(cx, |s, cx| {
                                if let Err(e) = s.skip_file(id) {
                                    s.push_error("Input", e, String::new());
                                }
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new(format!("retry-{id}"))
                        .label("Retry")
                        .icon(IconName::RotateCw)
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            store_r.update(cx, |s, cx| {
                                if let Err(e) = s.retry_file(id) {
                                    s.push_error("Input", e, String::new());
                                }
                                cx.notify();
                            });
                        }),
                ),
        )
}

fn global_toolbar(store: Entity<Store>) -> impl IntoElement {
    let store_pause = store.clone();
    let store_abort = store.clone();
    let store_continue = store.clone();
    h_flex()
        .gap_2()
        .flex_wrap()
        .child(
            Button::new("pause-all")
                .label("Pause All")
                .icon(IconName::Pause)
                .font_weight(FontWeight::SEMIBOLD)
                .on_click(move |_, _, cx| {
                    store_pause.update(cx, |s, cx| {
                        s.set_paused(true);
                        cx.notify();
                    });
                }),
        )
        .child(
            Button::new("abort-all")
                .label("Abort All")
                .icon(IconName::OctagonX)
                .danger()
                .font_weight(FontWeight::SEMIBOLD)
                .on_click(move |_, _, cx| {
                    store_abort.update(cx, |s, cx| {
                        let _ = s.abort_all();
                        cx.notify();
                    });
                }),
        )
        .child(
            Button::new("continue-all")
                .label("Continue")
                .icon(IconName::Play)
                .primary()
                .font_weight(FontWeight::SEMIBOLD)
                .on_click(move |_, _, cx| {
                    store_continue.update(cx, |s, cx| {
                        s.set_paused(false);
                        cx.notify();
                    });
                }),
        )
}

impl Render for InputView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (count, started, loading, compute, mode, status, files, processing, has_warnings) = {
            let snap = self.store.read(cx);
            let warnings: Vec<_> = snap.file_warnings.iter().collect();
            (
                snap.input.len(),
                snap.started,
                snap.loading.is_some(),
                snap.compute_mode.clone(),
                snap.view,
                snap.status.clone(),
                snap.input.clone(),
                snap.processing.clone(),
                !warnings.is_empty(),
            )
        };
        let has_processing = !processing.is_empty();
        let (theme_id, hc) = {
            let snap = self.store.read(cx);
            (snap.theme_mode.clone(), snap.high_contrast)
        };
        let mut body = v_flex().gap(px(theme::SPACE_LG)).p(px(theme::SPACE_XL));

        // Toolbar row: add buttons + Start only (toggles live in Settings).
        body = body.child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(
                    Button::new("add-files")
                        .label("Add audio")
                        .icon(IconName::Plus)
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click({
                            let store = self.store.clone();
                            move |_, _, cx| {
                                if let Some(paths) = pick_audio_files() {
                                    store.update(cx, |s, cx| {
                                        s.add_files(paths);
                                        cx.notify();
                                    });
                                }
                            }
                        }),
                )
                .child(
                    Button::new("add-docs")
                        .label("Add documents")
                        .icon(IconName::Plus)
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click({
                            let store = self.store.clone();
                            move |_, _, cx| {
                                if let Some(paths) = pick_doc_files() {
                                    store.update(cx, |s, cx| {
                                        s.add_files(paths);
                                        cx.notify();
                                    });
                                }
                            }
                        }),
                )
                .child(
                    Button::new("add-video")
                        .label("Add video")
                        .icon(IconName::Plus)
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click({
                            let store = self.store.clone();
                            move |_, _, cx| {
                                if let Some(paths) = pick_video_files() {
                                    store.update(cx, |s, cx| {
                                        s.add_files(paths);
                                        cx.notify();
                                    });
                                }
                            }
                        }),
                )
                .child(start_button(
                    started,
                    loading,
                    &compute,
                    self.store.clone(),
                ))
                .child(theme::hint("Output + classes from Settings.", &theme_id, hc)));

        // Error Center banner + list.
        {
            let (n_err, show, errs) = {
                let s = self.store.read(cx);
                (s.errors.len(), s.show_errors, s.errors.clone())
            };
            if n_err > 0 {
                let store = self.store.clone();
                let store2 = self.store.clone();
                let store3 = self.store.clone();
                body = body.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("err-toggle")
                                .label(if show {
                                    format!("Hide errors ({n_err})")
                                } else {
                                    format!("Errors ({n_err}) — View")
                                })
                                .font_weight(FontWeight::SEMIBOLD)
                                .on_click(move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        s.show_errors = !s.show_errors;
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            Button::new("err-clear")
                                .label("Clear")
                                .font_weight(FontWeight::SEMIBOLD)
                                .on_click(move |_, _, cx| {
                                    store2.update(cx, |s, cx| {
                                        s.clear_errors();
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            Button::new("goto-settings")
                                .label("Settings")
                                .font_weight(FontWeight::SEMIBOLD)
                                .on_click(move |_, _, cx| {
                                    store3.update(cx, |s, cx| {
                                        s.goto_page(crate::store::Page::Settings);
                                        cx.notify();
                                    });
                                }),
                        ),
                );
                if show {
                    for e in errs.iter().rev().take(8) {
                        let id = e.id;
                        let store = self.store.clone();
                        body = body.child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .truncate()
                                        .child(format!("[{}] {}: {}", e.source, e.msg, e.detail))
                                        .font_weight(FontWeight::SEMIBOLD),
                                )
                                .child(
                                    Button::new(format!("err-x-{id}"))
                                        .icon(IconName::X)
                                        .tooltip("Dismiss error")
                                        .on_click(move |_, _, cx| {
                                            store.update(cx, |s, cx| {
                                                s.dismiss_error(id);
                                                cx.notify();
                                            });
                                        }),
                                ),
                        );
                    }
                }
            }
        }

        // Merge suggestions + prompts.
        {
            let (sugg, prom, sel) = {
                let s = self.store.read(cx);
                (s.suggested.clone(), s.prompted.clone(), s.merge_selection.len())
            };
            for g in &prom {
                let key = g.key.clone();
                let label = format!(
                    "Same lecture? {} ({:.2}) — merge?",
                    g.members.iter().map(|(_, n, _)| n.clone()).collect::<Vec<_>>().join(" + "),
                    g.score
                );
                let s1 = self.store.clone();
                let s2 = self.store.clone();
                let s3 = self.store.clone();
                let (k1, k2, k3) = (key.clone(), key.clone(), key.clone());
                body = body.child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(div().child(label).font_weight(FontWeight::BOLD))
                        .child(
                            Button::new(format!("mg-e-{key}"))
                                .label("Merge: Executive")
                                .primary()
                                .font_weight(FontWeight::SEMIBOLD)
                                .on_click(move |_, _, cx| {
                                    let (k, s1) = (k1.clone(), s1.clone());
                                    s1.update(cx, |s, cx| {
                                        match s.begin_merge_run_group(&k, "executive") {
                                            Ok(Some(rx)) => crate::store::spawn_pump(s1.clone(), rx, cx),
                                            Ok(None) => {}
                                            Err(e) => s.push_error("Merge", e, String::new()),
                                        }
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            Button::new(format!("mg-l-{key}"))
                                .label("Merge: Long")
                                .font_weight(FontWeight::SEMIBOLD)
                                .on_click(move |_, _, cx| {
                                    let (k, s2) = (k2.clone(), s2.clone());
                                    s2.update(cx, |s, cx| {
                                        match s.begin_merge_run_group(&k, "long") {
                                            Ok(Some(rx)) => crate::store::spawn_pump(s2.clone(), rx, cx),
                                            Ok(None) => {}
                                            Err(e) => s.push_error("Merge", e, String::new()),
                                        }
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            Button::new(format!("mg-k-{key}"))
                                .label("Keep separate")
                                .font_weight(FontWeight::SEMIBOLD)
                                .on_click(move |_, _, cx| {
                                    let (k, s3) = (k3.clone(), s3.clone());
                                    s3.update(cx, |s, cx| {
                                        s.dismiss_group(&k);
                                        cx.notify();
                                    });
                                }),
                        ),
                );
            }
            for g in &sugg {
                let key = g.key.clone();
                let label = format!(
                    "Similar: {} ({:.2})",
                    g.members.iter().map(|(_, n, _)| n.clone()).collect::<Vec<_>>().join(" + "),
                    g.score
                );
                let s1 = self.store.clone();
                let s2 = self.store.clone();
                let (k1, k2) = (key.clone(), key.clone());
                body = body.child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(div().child(label).font_weight(FontWeight::SEMIBOLD))
                        .child(
                            Button::new(format!("sg-m-{key}"))
                                .label("Merge")
                                .font_weight(FontWeight::SEMIBOLD)
                                .on_click(move |_, _, cx| {
                                    let (k, s1) = (k1.clone(), s1.clone());
                                    s1.update(cx, |s, cx| {
                                        match s.begin_merge_run_group(&k, "") {
                                            Ok(Some(rx)) => crate::store::spawn_pump(s1.clone(), rx, cx),
                                            Ok(None) => {}
                                            Err(e) => s.push_error("Merge", e, String::new()),
                                        }
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            Button::new(format!("sg-k-{key}"))
                                .label("Dismiss")
                                .font_weight(FontWeight::SEMIBOLD)
                                .on_click(move |_, _, cx| {
                                    let (k, s2) = (k2.clone(), s2.clone());
                                    s2.update(cx, |s, cx| {
                                        s.dismiss_group(&k);
                                        cx.notify();
                                    });
                                }),
                        ),
                );
            }
            if sel > 0 {
                let s1 = self.store.clone();
                let s2 = self.store.clone();
                body = body.child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(div().child(format!("{sel} selected for merge")).font_weight(FontWeight::BOLD))
                        .child(
                            Button::new("mg-sel-e")
                                .label("Merge selected: Executive")
                                .primary()
                                .font_weight(FontWeight::SEMIBOLD)
                                .on_click(move |_, _, cx| {
                                    let s1 = s1.clone();
                                    s1.update(cx, |s, cx| {
                                        match s.begin_merge_run_selected("executive") {
                                            Ok(Some(rx)) => crate::store::spawn_pump(s1.clone(), rx, cx),
                                            Ok(None) => {}
                                            Err(e) => s.push_error("Merge", e, String::new()),
                                        }
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            Button::new("mg-sel-l")
                                .label("Merge selected: Long")
                                .font_weight(FontWeight::SEMIBOLD)
                                .on_click(move |_, _, cx| {
                                    let s2 = s2.clone();
                                    s2.update(cx, |s, cx| {
                                        match s.begin_merge_run_selected("long") {
                                            Ok(Some(rx)) => crate::store::spawn_pump(s2.clone(), rx, cx),
                                            Ok(None) => {}
                                            Err(e) => s.push_error("Merge", e, String::new()),
                                        }
                                        cx.notify();
                                    });
                                }),
                        ),
                );
            }
        }

        // Warning badges
        if has_warnings {
            let warnings = {
                let snap = self.store.read(cx);
                snap.file_warnings.iter().map(|(path, reason)| (path.clone(), reason.clone())).collect::<Vec<_>>()
            };
            for (path, reason) in &warnings {
                let store = self.store.clone();
                let path_str = path.to_string_lossy().to_string();
                let reason_str = reason.clone();
                let path_str2 = path_str.clone();
                let reason_str2 = reason_str.clone();
                body = body.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new(format!("warn-banner-{path_str}"))
                                .icon(IconName::TriangleAlert)
                                .font_weight(FontWeight::SEMIBOLD)
                                .on_click(move |_, _, cx| {
                                    let _ = store.update(cx, |s, cx| {
                                        s.status = format!("{}: {}", path_str2, reason_str2);
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .child(format!("{}: {}", path_str, reason_str))
                                .text_color(rgba(theme::warning(&theme_id, hc))),
                        ),
                );
            }
        }

        // Status
        body = body.child(theme::value(status, &theme_id, hc));

        // Global toolbar when processing is active
        if has_processing {
            body = body.child(global_toolbar(self.store.clone()));
        }

        // Staged files (checkbox marks merge selection)
        body = body.child(
            div()
                .child(format!("{count} file(s) staged. Select files to merge them."))
                .font_weight(FontWeight::BOLD),
        );

        let selected: std::collections::HashSet<u64> =
            self.store.read(cx).merge_selection.clone();
        let rows: Vec<_> = files
            .into_iter()
            .map(|f| {
                let warning = self
                    .store
                    .read(cx)
                    .get_warning(&f.path)
                    .map(|s| s.to_string());
                let checked = selected.contains(&f.id);
                staged_card(&f, warning, checked, mode, self.store.clone(), &theme_id, hc)
            })
            .collect();
        body = body.child(match mode {
            ViewMode::Details => v_flex()
                .gap_1()
                .children(rows)
                .into_any_element(),
            _ => div()
                .flex()
                .flex_row()
                .gap_2()
                .flex_wrap()
                .children(rows)
                .into_any_element(),
        });

        // Active processing files with progress bars and buttons
        if has_processing {
            body = body.child(div().child("Processing…").font_weight(FontWeight::BOLD));
            let cards: Vec<_> = processing
                .iter()
                .map(|pf| active_file_card(pf, self.store.clone(), &theme_id, hc))
                .collect();
            body = body.child(
                v_flex()
                    .gap_2()
                    .children(cards),
            );
        }

        // OS drag-and-drop: files/folders from Explorer land here (folders
        // expand via add_dropped; the root id makes this a drop target).
        // Page-level scroll (matches Settings): the root gives this page a
        // bounded flex slot, so the staged list + progress scroll instead of
        // overflowing the window. Nav stays fixed — only this body scrolls.
        let store_d = self.store.clone();
        div()
            .flex_1()
            .h_full()
            .overflow_y_scrollbar()
            .id("input-scroll")
            .child(body)
            .on_drop(move |paths: &ExternalPaths, _, cx| {
                let dropped: Vec<std::path::PathBuf> = paths.paths().to_vec();
                store_d.update(cx, |s, cx| {
                    s.add_dropped(&dropped);
                    cx.notify();
                });
            })
            .into_any_element()
    }
}
