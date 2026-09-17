//! First-run wizard: hardware-detected tier recommendation, one-click pair
//! download with progress, Ollama library scan + link, and an explicit
//! continue-without-models exit. Never a trap: every state has a visible
//! way forward, including fully offline (scan, link, or continue).

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{div, rgb, App, Context, Entity, FontWeight, IntoElement, Render, Window};

use crate::store::{Page, Store};
use crate::theme;

pub struct WizardView {
    store: Entity<Store>,
}

impl WizardView {
    pub fn new(cx: &mut App, store: Entity<Store>) -> Entity<Self> {
        cx.new(|_| WizardView { store })
    }
}

impl Render for WizardView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Wizard renders only while open; the shell guards this too.
        if !self.store.read(cx).wizard_open {
            return div().into_any_element();
        }
        let (tier, detail, jobs, total, stt_id, llm_id) = {
            let snap = self.store.read(cx);
            let (t, d) = (snap.wizard_tier.clone(), snap.wizard_detail.clone());
            let (stt_id, llm_id, total) = Store::tier_info(&t).unwrap_or_default();
            let jobs: Vec<(String, f32, bool, String)> = [stt_id.clone(), llm_id.clone()]
                .into_iter()
                .filter(|id| !id.is_empty())
                .map(|id| {
                    let dl = snap.downloads.get(&id).cloned().unwrap_or_default();
                    let pct = if dl.total > 0 {
                        (dl.downloaded as f32 / dl.total as f32 * 100.0).round()
                    } else {
                        0.0
                    };
                    (id, pct, dl.done, dl.error)
                })
                .collect();
            (t, d, jobs, total, stt_id, llm_id)
        };

        let store = self.store.clone();
        let store_t = self.store.clone();
        let store_i = self.store.clone();
        let store_c = self.store.clone();
        let tier_now = tier.clone();
        let dark = self.store.read(cx).dark_mode;
        let mut body = v_flex()
            .gap_2()
            .p_3()
            .child(
                div()
                    .child("Welcome to Present Voice")
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(theme::fg(dark))),
            )
            .child(div().child(detail).font_weight(FontWeight::SEMIBOLD))
            .child(
                h_flex()
                    .gap_2()
                    .child(tier_button("Lite", "lite", &tier, self.store.clone(), cx))
                    .child(tier_button(
                        "Standard",
                        "standard",
                        &tier,
                        store_t.clone(),
                        cx,
                    ))
                    .child(tier_button("Full", "full", &tier, self.store.clone(), cx)),
            )
            .child(
                div()
                    .child(format!(
                        "{} + {} ≈ {:.1} GB download.",
                        stt_id,
                        llm_id,
                        total as f64 / 1073741824.0
                    ))
                    .font_weight(FontWeight::SEMIBOLD),
            );
        for (id, pct, done, err) in jobs {
            let mut row = h_flex()
                .gap_2()
                .child(
                    div()
                        .child(format!("{id}: {pct}%{}", if done { " done" } else { "" }))
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(Progress::new(format!("wz-{id}")).value(pct));
            if !err.is_empty() {
                row = row.child(
                    div()
                        .child(format!("Error: {err} — retry or import below."))
                        .font_weight(FontWeight::SEMIBOLD),
                );
            }
            body = body.child(row);
        }
        body = body.child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("wz-go").label("Download & Start").primary().font_weight(FontWeight::SEMIBOLD).on_click(move |_, _, cx| {
                        let tier_now = tier_now.clone();
                        let store = store.clone();
                        store.update(cx, |s, cx| {
                            match s.download_tier(&tier_now) {
                                Ok(list) => {
                                    for rx in list {
                                        crate::store::spawn_pump(store.clone(), rx, cx);
                                    }
                                }
                                Err(e) => s.status = format!("Tier download failed: {e}"),
                            }
                            cx.notify();
                        });
                    }),
                )
                .child(
                    Button::new("wz-scan").label("Scan Ollama library").font_weight(FontWeight::SEMIBOLD).on_click(move |_, _, cx| {
                        store_i.update(cx, |s, cx| {
                            let n = s.scan_ollama();
                            if n > 0 {
                                // Models page is where links happen.
                                s.wizard_open = false;
                                s.page = Page::Models;
                            }
                            cx.notify();
                        });
                    }),
                )
                .child(
                    Button::new("wz-skip").label("Continue without models").font_weight(FontWeight::SEMIBOLD).on_click(move |_, _, cx| {
                        store_c.update(cx, |s, cx| {
                            s.wizard_open = false;
                            s.status = "Running without models: transcription disabled until Models are ready.".to_string();
                            cx.notify();
                        });
                    }),
                ),
        );
        body.into_any_element()
    }
}

fn tier_button(
    label: &'static str,
    tier: &'static str,
    active: &str,
    store: Entity<Store>,
    _cx: &mut Context<WizardView>,
) -> impl IntoElement {
    let mut b = Button::new(format!("wz-tier-{tier}"))
        .label(label)
        .font_weight(FontWeight::SEMIBOLD);
    if active == tier {
        b = b.primary();
    }
    let tier = tier.to_string();
    b.on_click(move |_, _, cx| {
        store.update(cx, |s, cx| {
            s.wizard_tier = tier.clone();
            cx.notify();
        });
    })
}
