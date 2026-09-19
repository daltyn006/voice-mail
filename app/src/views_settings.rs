//! Settings page: single home for every toggle (single source of truth).
//!
//! Toolbars elsewhere are stripped to run controls; everything here writes
//! through `Store::set_*` (persisted to `ui.json`). Text fields are view
//! entities flushed on Save (same pattern as the old Input Start button).

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, App, Context, Entity, FontWeight, IntoElement, Render, Subscription, Window};

use crate::store::{Store, ViewMode};
use crate::theme;

pub struct SettingsView {
    store: Entity<Store>,
    classes: Entity<InputState>,
    outdir: Entity<InputState>,
    reviews: Entity<InputState>,
    chunk: Entity<InputState>,
    window: Entity<InputState>,
    budget: Entity<InputState>,
    suggest: Entity<InputState>,
    prompt: Entity<InputState>,
    docmax: Entity<InputState>,
    attention: Entity<SliderState>,
    /// Held (never read) so the slider subscription stays alive.
    _subs: Vec<Subscription>,
}

impl SettingsView {
    pub fn new(window: &mut Window, cx: &mut App, store: Entity<Store>) -> Entity<Self> {
        let att_init = store.read(cx).attention.clamp(0, 100) as f32;
        let attention = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(100.)
                .step(1.)
                .default_value(SliderValue::Single(att_init))
        });
        let store_c = store.clone();
        let att_c = attention.clone();
        cx.new(|cx| {
            // Drag-release only (not every Change tick): one persisted write
            // per gesture instead of prefs spam mid-drag.
            let sub = cx.subscribe(
                &att_c,
                move |_this: &mut Self, _, ev: &SliderEvent, cx| {
                    if let SliderEvent::Release(SliderValue::Single(v)) = ev {
                        let v = (*v as i32).clamp(0, 100);
                        store_c.update(cx, |s, cx| {
                            s.set_attention(v);
                            cx.notify();
                        });
                    }
                },
            );
            SettingsView {
                store,
                classes: cx.new(|cx| InputState::new(window, cx)),
                outdir: cx.new(|cx| InputState::new(window, cx)),
                reviews: cx.new(|cx| InputState::new(window, cx)),
                chunk: cx.new(|cx| InputState::new(window, cx)),
                window: cx.new(|cx| InputState::new(window, cx)),
                budget: cx.new(|cx| InputState::new(window, cx)),
                suggest: cx.new(|cx| InputState::new(window, cx)),
                prompt: cx.new(|cx| InputState::new(window, cx)),
                docmax: cx.new(|cx| InputState::new(window, cx)),
                attention: att_c,
                _subs: vec![sub],
            }
        })
    }
}

fn section(title: &str, theme: &str, high_contrast: bool) -> impl IntoElement {
    crate::theme::title(title.to_string(), theme, high_contrast)
}

/// Attention detent button: sets the store value AND moves the slider thumb
/// so the two can never disagree (choice() only owns the store).
fn detent(
    id: &'static str,
    label: &'static str,
    value: i32,
    slider: Entity<SliderState>,
    store: Entity<Store>,
) -> gpui_kit::AnyElement {
    Button::new(id)
        .label(label)
        .font_weight(FontWeight::SEMIBOLD)
        .on_click(move |_, window, cx| {
            let v = value;
            store.update(cx, |s, cx| {
                s.set_attention(v);
                cx.notify();
            });
            slider.update(cx, |st, cx| {
                st.set_value(SliderValue::Single(v as f32), window, cx);
            });
        })
        .into_any_element()
}

fn choice(
    group: &str,
    options: &[(&str, &str, bool)],
    store: Entity<Store>,
) -> gpui_kit::AnyElement {
    let mut row = h_flex().gap_2().flex_wrap();
    for (id, label, active) in options {
        let mut b = Button::new(format!("{group}-{id}"))
            .label(*label)
            .font_weight(FontWeight::SEMIBOLD);
        if *active {
            b = b.primary();
        }
        let store = store.clone();
        let id = id.to_string();
        let group = group.to_string();
        row = row.child(b.on_click(move |_, _, cx| {
            let (group, id) = (group.clone(), id.clone());
            store.update(cx, |s, cx| {
                match group.as_str() {
                    "theme" => {
                        s.set_theme_mode(&id);
                    }
                    "nav" => {
                        s.set_nav_mode(&id);
                    }
                    "density" => s.set_view_mode(match id.as_str() {
                        "list" => ViewMode::List,
                        "details" => ViewMode::Details,
                        _ => ViewMode::Large,
                    }),
                    "compute" => s.set_compute_mode(&id),
                    "delconv" => s.set_delete_converted(id == "on"),
                    "retention" => s.set_audio_retention(&id),
                    "denoise" => s.set_denoise_mode(&id),
                    "websrch" => s.set_web_research(id == "on"),
                    "sumtier" => s.set_summary_tier(&id),
                    "mergemode" => s.set_merge_mode(&id),
                    "mergeprompt" => s.set_merge_auto_prompt(id == "on"),
                    "pdfmode" => s.set_pdf_mode(&id),
                    _ => {
                        s.push_error("Settings", format!("unknown option group: {group}"), String::new());
                    }
                }
                cx.notify();
            });
            // Kit globals must follow every theme switch (a store value
            // alone leaves the old palette live — the invisible-text bug).
            if group.as_str() == "theme" {
                let (tm, hc) = {
                    let snap = store.read(cx);
                    (snap.theme_mode.clone(), snap.high_contrast)
                };
                crate::theme::apply_theme(&tm, hc, cx);
            }
        }));
    }
    row.into_any_element()
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.store.read(cx);
        let theme_mode = s.theme_mode.clone();
        let high_contrast = s.high_contrast;
        let nav_mode = s.nav_mode.clone();
        let reduce_motion = s.reduce_motion;
        let theme_id = theme_mode.as_str();
        let density = match s.view {
            ViewMode::List => "list",
            ViewMode::Details => "details",
            _ => "large",
        }
        .to_string();
        let compute = s.compute_mode.clone();
        let delconv = s.delete_converted;
        let retention = s.audio_retention.clone();
        let denoise = s.denoise_mode.clone();
        let mmode = s.merge_mode.clone();
        let mprompt = s.merge_auto_prompt;
        let pdfmode = s.pdf_mode.clone();
        let outdir_cur = s
            .default_outdir
            .clone()
            .unwrap_or_else(|| "(default output folder)".to_string());
        let models_cur = Store::effective_models_dir();
        let reviews_cur = s
            .review_drafts_dir
            .clone()
            .unwrap_or_else(|| "(default reviews folder)".to_string());
        let classes_cur = if s.default_classes.is_empty() {
            "(no default classes)".to_string()
        } else {
            s.default_classes.clone()
        };
        let adv = format!(
            "window {}s · VRAM {}% (chunk size lives in Summary)",
            s.audio_window_sec, s.vram_budget_pct
        );
        let tier = s.summary_tier.clone();
        let chunk_line = format!(
            "chunks {} tokens · target ~{}% ({} guide)",
            s.chunk_tokens,
            Store::tier_ratio_pct(&tier),
            tier,
        );
        let merge = format!(
            "suggest ≥{:.2} · prompt ≥{:.2} · mode {} · auto-prompt {}",
            s.merge_suggest,
            s.merge_prompt,
            s.merge_mode,
            if s.merge_auto_prompt { "on" } else { "off" }
        );
        let web = s.web_research;
        let vlm_line = {
            let active = if s.active_vlm.is_empty() {
                "(none — videos run transcript-only)".to_string()
            } else {
                s.active_vlm.clone()
            };
            match s.vlm_for_attention(s.attention) {
                Some(sug) if sug != s.active_vlm && !s.active_vlm.is_empty() => {
                    format!("Vision: {active} (suggested for this attention: {sug})")
                }
                _ => format!("Vision: {active}"),
            }
        };
        let doc = format!("max {} chars/file · scanned-PDF {}", s.doc_max_chars, s.pdf_mode);
        let status = s.status.clone();
        let store = self.store.clone();

        let classes_e = self.classes.clone();
        let outdir_e = self.outdir.clone();
        let reviews_e = self.reviews.clone();
        let chunk_e = self.chunk.clone();
        let window_e = self.window.clone();
        let budget_e = self.budget.clone();
        let suggest_e = self.suggest.clone();
        let prompt_e = self.prompt.clone();
        let docmax_e = self.docmax.clone();
        let attention_e = self.attention.clone();

        // Narrow centered column (long lines tire the eye) with roomy
        // section gaps; the outer page owns scrolling.
        let mut body = v_flex()
            .gap(px(theme::SPACE_LG))
            .p(px(theme::SPACE_XL))
            .max_w(px(760.))
            .w_full()
            .mx_auto();
        body = body
            .child(section("Appearance", &theme_mode, high_contrast))
            .child(choice(
                "theme",
                &[
                    ("dark", "Dark", theme_id == "dark"),
                    ("light", "Light", theme_id == "light"),
                    ("gruvbox_dark", "Gruvbox Dark", theme_id == "gruvbox_dark"),
                    ("gruvbox_light", "Gruvbox Light", theme_id == "gruvbox_light"),
                    ("coffee_dark", "Coffee Dark", theme_id == "coffee_dark"),
                    ("coffee_light", "Coffee Light", theme_id == "coffee_light"),
                ],
                store.clone(),
            ))
            .child(choice(
                "nav",
                &[
                    ("tabs", "Tabs", nav_mode == "tabs"),
                    ("sidebar", "Sidebar", nav_mode == "sidebar"),
                ],
                store.clone(),
            ))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        div()
                            .flex_1()
                            .child("High contrast (max text contrast, keeps theme colors)")
                            .font_weight(FontWeight::SEMIBOLD),
                    )
                    .child(
                        Button::new("high-contrast")
                            .label(if high_contrast { "On" } else { "Off" })
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    let (tm, hc) = store.update(cx, |s, cx| {
                                        let out = s.set_high_contrast(!s.high_contrast);
                                        cx.notify();
                                        out
                                    });
                                    crate::theme::apply_theme(&tm, hc, cx);
                                }
                            }),
                    ),
            )
            .child(choice(
                "density",
                &[
                    ("large", "Icons", density == "large"),
                    ("list", "List", density == "list"),
                    ("details", "Details", density == "details"),
                ],
                store.clone(),
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child("Reduce motion")
                            .font_weight(FontWeight::SEMIBOLD),
                    )
                    .child(
                        Button::new("reduce-motion")
                            .label(if reduce_motion { "On" } else { "Off" })
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        s.set_reduce_motion(!s.reduce_motion);
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            );
        body = body
            .child(section("Run defaults (Start uses these)", &theme_mode, high_contrast))
            .child(
                h_flex()
                    .gap_2()
                    .child(theme::label("Default classes", &theme_mode, high_contrast))
                    .child(theme::value(classes_cur, &theme_mode, high_contrast)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(div().flex_1().min_w(px(180.)).child(Input::new(&classes_e)))
                    .child(
                        Button::new("set-classes")
                            .label("Save classes")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    let t = classes_e.read(cx).text().to_string();
                                    store.update(cx, |s, cx| {
                                        s.set_default_classes(t);
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            )
            .child(theme::hint("Start transcribes + summarizes with these; no per-run boxes.", &theme_mode, high_contrast));
        body = body
            .child(section("Processing", &theme_mode, high_contrast))
            .child(choice(
                "compute",
                &[
                    ("auto", "Automatic (GPU)", compute == "auto"),
                    ("cpu", "CPU only", compute == "cpu"),
                ],
                store.clone(),
            ))
            .child(theme::value(adv, &theme_mode, high_contrast))
            .child(choice(
                "denoise",
                &[
                    ("recommended", "Denoise: recommended (adaptive)", denoise == "recommended"),
                    ("off", "Denoise: off", denoise == "off"),
                    ("aggressive", "Denoise: aggressive", denoise == "aggressive"),
                ],
                store.clone(),
            ))
            .child(
                theme::hint("Adaptive room-tone gate on the STT copy only — archival takes stay untouched. If voices clip and mic effects are already on outside the app, try Off.", &theme_mode, high_contrast),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(div().flex_1().child(Input::new(&window_e)))
                    .child(div().flex_1().child(Input::new(&budget_e)))
                    .child(
                        Button::new("set-adv")
                            .label("Apply (window/VRAM)")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    let (b, c) = (
                                        window_e.read(cx).text().to_string(),
                                        budget_e.read(cx).text().to_string(),
                                    );
                                    store.update(cx, |s, cx| {
                                        if let Ok(v) = b.trim().parse::<i32>() {
                                            s.set_audio_window(v);
                                        }
                                        if let Ok(v) = c.trim().parse::<i32>() {
                                            s.set_vram_budget(v);
                                        }
                                        s.status = "Processing tuning saved.".to_string();
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            );
        body = body
            .child(section("Summary (length tier drives chunks + guide + ratio)", &theme_mode, high_contrast))
            .child(choice(
                "sumtier",
                &[
                    ("recap", "Recap ~25%", tier == "recap"),
                    ("standard", "Standard ~50%", tier == "standard"),
                    ("detailed", "Detailed ~75%", tier == "detailed"),
                ],
                store.clone(),
            ))
            .child(theme::value(chunk_line, &theme_mode, high_contrast))
            .child(
                theme::hint(
                    "Small 1000 · Medium 4000 · Large 8000 tokens. \
                        Large wants a Llama/Qwen summarizer (Gemma is 8k-native) \
                        and headroom past 12GB VRAM — otherwise quality degrades.",
                    &theme_mode,
                    high_contrast,
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("chunk-small")
                            .label("Small")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        s.set_chunk_tokens(1000);
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("chunk-medium")
                            .label("Medium")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        s.set_chunk_tokens(4000);
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("chunk-large")
                            .label("Large")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        s.set_chunk_tokens(8000);
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(div().flex_1().child(Input::new(&chunk_e)))
                    .child(
                        Button::new("set-chunk")
                            .label("Custom tokens")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    let t = chunk_e.read(cx).text().to_string();
                                    store.update(cx, |s, cx| {
                                        if let Ok(v) = t.trim().parse::<i32>() {
                                            s.set_chunk_tokens(v);
                                        }
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            );
        body = body
            .child(section("Storage", &theme_mode, high_contrast))
            .child(
                h_flex()
                    .gap_2()
                    .child(theme::label("Output folder", &theme_mode, high_contrast))
                    .child(theme::value(outdir_cur, &theme_mode, high_contrast)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(div().flex_1().child(Input::new(&outdir_e)))
                    .child(
                        Button::new("set-outdir")
                            .label("Save output folder")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    let t = outdir_e.read(cx).text().to_string();
                                    store.update(cx, |s, cx| {
                                        if let Err(e) = s.set_default_outdir(&t) {
                                            s.push_error("Settings", format!("Bad output folder: {e}"), String::new());
                                        }
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("browse-outdir")
                            .label("Browse…")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                                        let p = p.to_string_lossy().into_owned();
                                        store.update(cx, |s, cx| {
                                            if let Err(e) = s.set_default_outdir(&p) {
                                                s.push_error("Settings", format!("Bad output folder: {e}"), String::new());
                                            }
                                            cx.notify();
                                        });
                                    }
                                }
                            }),
                    )
                    .child(
                        Button::new("reset-outdir")
                            .label("Reset")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        s.reset_default_outdir();
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            )
            .child(div().child(format!("Models folder: {models_cur}")).font_weight(FontWeight::SEMIBOLD))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("models-browse")
                            .label("Change models folder…")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                                        let p = p.to_string_lossy().into_owned();
                                        store.update(cx, |s, cx| {
                                            if let Err(e) = s.set_custom_models_dir(&p) {
                                                s.push_error("Settings", format!("Bad models folder: {e}"), String::new());
                                            }
                                            cx.notify();
                                        });
                                    }
                                }
                            }),
                    )
                    .child(
                        Button::new("models-reset")
                            .label("Reset models folder")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        s.reset_models_dir();
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            )
            .child(choice(
                "delconv",
                &[("on", "Delete converted: ON", delconv), ("off", "Delete converted: OFF", !delconv)],
                store.clone(),
            ))
            .child(choice(
                "retention",
                &[
                    ("keep", "Sources: keep", retention == "keep"),
                    ("delete", "Sources: delete on success", retention == "delete"),
                    ("archive", "Sources: archive beside output", retention == "archive"),
                ],
                store.clone(),
            ))
            .child(
                theme::hint("Delete/archive apply to successful outputs only — failures, aborts, and videos are never touched.", &theme_mode, high_contrast),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(theme::label("Review drafts", &theme_mode, high_contrast))
                    .child(theme::value(reviews_cur, &theme_mode, high_contrast)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(div().flex_1().child(Input::new(&reviews_e)))
                    .child(
                        Button::new("set-reviews")
                            .label("Save drafts folder")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                let reviews_e = reviews_e.clone();
                                move |_, _, cx| {
                                    let t = reviews_e.read(cx).text().to_string();
                                    store.update(cx, |s, cx| {
                                        if let Err(e) = s.set_reviews_dir(&t) {
                                            s.push_error("Settings", format!("Bad drafts folder: {e}"), String::new());
                                        }
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("move-reviews")
                            .label("Move existing drafts")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                let reviews_e = reviews_e.clone();
                                move |_, _, cx| {
                                    let t = reviews_e.read(cx).text().to_string();
                                    store.update(cx, |s, cx| {
                                        if let Err(e) = s.move_review_drafts(&t) {
                                            s.push_error("Settings", format!("Move drafts failed: {e}"), String::new());
                                        }
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("reset-reviews")
                            .label("Reset")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        s.reset_reviews_dir();
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            );
        body = body
            .child(section("Documents", &theme_mode, high_contrast))
            .child(theme::value(doc, &theme_mode, high_contrast))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(div().flex_1().min_w(px(180.)).child(Input::new(&docmax_e)))
                    .child(
                        Button::new("set-docmax")
                            .label("Save max chars")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    let t = docmax_e.read(cx).text().to_string();
                                    store.update(cx, |s, cx| {
                                        if let Ok(v) = t.trim().parse::<i32>() {
                                            s.set_doc_max(v);
                                            s.status = "Document cap saved.".to_string();
                                        }
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            )
            .child(choice(
                "pdfmode",
                &[("warn", "Scanned PDF: warn+skip", pdfmode == "warn"), ("shell", "Scanned PDF: keep shell", pdfmode == "shell")],
                store.clone(),
            ));
        body = body
            .child(section("Merging", &theme_mode, high_contrast))
            .child(theme::value(merge, &theme_mode, high_contrast))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(div().flex_1().min_w(px(140.)).child(Input::new(&suggest_e)))
                    .child(div().flex_1().min_w(px(140.)).child(Input::new(&prompt_e)))
                    .child(
                        Button::new("set-merge")
                            .label("Save thresholds")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    let (a, b) = (
                                        suggest_e.read(cx).text().to_string(),
                                        prompt_e.read(cx).text().to_string(),
                                    );
                                    store.update(cx, |s, cx| {
                                        let lo = a.trim().parse::<f32>().unwrap_or(s.merge_suggest);
                                        let hi = b.trim().parse::<f32>().unwrap_or(s.merge_prompt);
                                        s.set_merge_thresholds(lo, hi);
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            )
            .child(choice(
                "mergemode",
                &[
                    ("ask", "Default: ask", mmode == "ask"),
                    ("executive", "Default: executive", mmode == "executive"),
                    ("long", "Default: long", mmode == "long"),
                ],
                store.clone(),
            ))
            .child(choice(
                "mergeprompt",
                &[("on", "Auto-prompt: ON", mprompt), ("off", "Auto-prompt: OFF", !mprompt)],
                store.clone(),
            ));
        body = body
            .child(section("Documentary (video attention)", &theme_mode, high_contrast))
            .child(
                theme::value(
                    format!(
                        "Attention {} ({}): Overview = sparse frames + short overview; Academic = dense frames + full analysis.",
                        s.attention,
                        Store::attention_label(s.attention),
                    ),
                    &theme_mode,
                    high_contrast,
                ),
            )
            .child(div().flex_1().child(Slider::new(&attention_e)))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(detent("att-overview", "Overview", 15, attention_e.clone(), store.clone()))
                    .child(detent("att-balanced", "Balanced", 50, attention_e.clone(), store.clone()))
                    .child(detent("att-academic", "Academic", 85, attention_e.clone(), store.clone())),
            )
            .child(
                theme::hint(format!("{vlm_line} (change on the Models page → Vision)"), &theme_mode, high_contrast),
            );
        body = body
            .child(section("Research (web, opt-in)", &theme_mode, high_contrast))
            .child(choice(
                "websrch",
                &[("off", "Web research: OFF (offline)", !web), ("on", "Web research: ON (up to 10 cited sources)", web)],
                store.clone(),
            ))
            .child(
                theme::hint("Off by default. When on, video jobs may fetch pages AFTER watching — web text is cited per-claim and never mixed with film facts.", &theme_mode, high_contrast),
            );
        body = body
            .child(section("Diagnostics", &theme_mode, high_contrast))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("diag-clear")
                            .label("Clear logs")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        match pv_backend::diag::clear_logs() {
                                            Ok(()) => s.status = "Logs cleared.".to_string(),
                                            Err(e) => s.push_error("Settings", format!("Clear logs failed: {e}"), String::new()),
                                        }
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("unhide-all")
                            .label("Unhide dismissed outputs")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        s.unhide_dismissed();
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("sweep-drafts")
                            .label("Clean orphan drafts")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        s.sweep_drafts();
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("clean-caches")
                            .label("Clean caches")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        s.clean_caches();
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("check-updates")
                            .label("Check for updates")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    let rx = match store.update(cx, |s, _| s.check_updates()) {
                                        Ok(rx) => rx,
                                        Err(e) => {
                                            store.update(cx, |s, cx| {
                                                s.push_error("Settings", e, String::new());
                                                cx.notify();
                                            });
                                            return;
                                        }
                                    };
                                    crate::store::spawn_pump(store.clone(), rx, cx);
                                    store.update(cx, |_, cx| cx.notify());
                                }
                            }),
                    )
                    .child(
                        Button::new("view-logs")
                            .label("View logs")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        s.show_logs = !s.show_logs;
                                        if s.show_logs {
                                            s.log_view = s.log_tails();
                                        }
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            );
        // Log viewer (loaded on toggle, never per-frame).
        {
            let tails = {
                let snap = self.store.read(cx);
                snap.show_logs.then(|| snap.log_view.clone())
            };
            if let Some(tails) = tails {
                let mut logs = v_flex().gap_1();
                for t in &tails {
                    let mut block = v_flex().gap_1().child(
                        div()
                            .child(format!("{} ({}){}", t.name, t.path, if t.truncated { " — tail" } else { "" }))
                            .font_weight(FontWeight::BOLD),
                    );
                    for line in t.lines.iter().take(60) {
                        block = block.child(div().child(line.clone()));
                    }
                    logs = logs.child(block);
                }
                body = body.child(logs.into_any_element());
            }
        }
        body = body.child(
            theme::hint(
                "Long GPU runs dying with no error? Windows kills GPU drivers that stay \
                        busy ~2s (TDR timeout) — a long model dispatch on a big file can trip it, \
                        and no code can catch that kill. Documented remedy for the Full tier: set \
                        TdrDelay to 10 under HKLM\\SYSTEM\\CurrentControlSet\\Control\\GraphicsDrivers \
                        (regedit, admin rights; reboot after). This app never touches the registry — \
                        alternatively retry the file on CPU (Settings → Processing).",
                &theme_mode,
                high_contrast,
            ),
        );
        body = body.child(theme::value(status, &theme_mode, high_contrast));
        // Scrollable so all seven sections fit any window size (nav stays
        // fixed — only this page body scrolls).
        div()
            .flex_1()
            .h_full()
            .overflow_y_scrollbar()
            .id("settings-scroll")
            .child(body)
            .into_any_element()
    }
}
