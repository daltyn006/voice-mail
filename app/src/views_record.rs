//! Record page: in-app voice capture (Audacity-style transport, Feishin-style
//! live waveform). Takes are 24-bit WAV at the device's native rate under
//! `<data>/recordings/` — never auto-transcribed, never auto-deleted.
//!
//! State lives in `Store` (transport + take + peaks); this view owns no data
//! beyond a refresh tick. Buttons only — no hotkeys on this page.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, rgb, App, Context, Entity, FontWeight, IntoElement, Render, Window};

use crate::store::{RecStatus, Store};
use crate::theme;

pub struct RecordView {
    store: Entity<Store>,
}

impl RecordView {
    pub fn assemble(_window: &mut Window, _cx: &mut App, store: Entity<Store>) -> Self {
        RecordView { store }
    }
}

/// Background refresh tick: polls recorder counters (~5 fps) and notifies so
/// the timer + waveform stay live. Exits on its own when capture/playback
/// ends; at most one tick runs (guarded by `rec_tick_live`).
pub fn spawn_rec_tick(store: Entity<Store>, cx: &mut gpui_kit::App) {
    let already = store.update(cx, |s, _| {
        if s.rec_tick_live {
            true
        } else {
            s.rec_tick_live = true;
            false
        }
    });
    if already {
        return;
    }
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(200))
                .await;
            let live = cx.update(|cx| {
                let mut live = false;
                store.update(cx, |s, cx| {
                    s.rec_poll();
                    live = matches!(
                        s.rec_status,
                        RecStatus::Recording | RecStatus::Playing
                    );
                    cx.notify();
                });
                live
            });
            if !live {
                let _ = cx.update(|cx| {
                    store.update(cx, |s, _| {
                        s.rec_tick_live = false;
                    });
                });
                break;
            }
        }
    })
    .detach();
}

fn waveform(peaks: &[f32], dark: bool, clips: u64) -> impl IntoElement {
    // Downsample to ≤96 bars; bar heights scale to a 64px lane.
    let stride = (peaks.len().max(1) + 95) / 96;
    let bars: Vec<_> = peaks
        .iter()
        .step_by(stride.max(1))
        .take(96)
        .map(|p| {
            let h = 2.0 + p.clamp(0.0, 1.0) * 62.0;
            let hot = *p >= 0.999;
            div()
                .w(px(4.))
                .h(px(h))
                .bg(rgb(if hot {
                    0xc0_39_2b_ff // clip red (both themes — must shout)
                } else if dark {
                    0x5d_8a_a8_ff // muted steel-blue on near-black
                } else {
                    0x7a_7a_7a_ff // neutral gray on white
                }))
        })
        .collect();
    v_flex()
        .gap_1()
        .p_2()
        .bg(rgb(theme::surface(dark)))
        .child(
            h_flex()
                .gap_1()
                .items_end()
                .h(px(68.))
                .children(bars)
                .into_any_element(),
        )
        .child(
            div()
                .child(if clips > 0 {
                    format!("⚠ {clips} clip(s) — move the mic back")
                } else {
                    "levels look good".to_string()
                })
                .font_weight(FontWeight::SEMIBOLD),
        )
}

impl Render for RecordView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Poll first so a freshly-ended playback flips to Stopped on view.
        self.store.update(cx, |s, _| s.rec_poll());
        let snap = self.store.read(cx);
        let (status, line, peaks, clips, rec_status, devices, cur_dev, rates, cur_rate) = (
            snap.status.clone(),
            snap.rec_status_line(),
            snap.rec_peaks.clone(),
            snap.rec_clips,
            snap.rec_status,
            self.store.read(cx).rec_devices(),
            snap.rec_device.clone(),
            self.store.read(cx).rec_device_rates(),
            snap.rec_rate_opt,
        );
        let dark = snap.dark_mode;
        let store = self.store.clone();

        let mut body = v_flex().gap_2().p_3();
        body = body
            .child(div().child("Record — voice capture (24-bit WAV)").font_weight(FontWeight::BOLD))
            .child(div().child(line).font_weight(FontWeight::SEMIBOLD))
            .child(waveform(&peaks, dark, clips).into_any_element());

        // Device picker: Default + detected inputs (cap 8 buttons).
        {
            let mut row = h_flex()
                .gap_2()
                .flex_wrap()
                .child(div().child("Input:").font_weight(FontWeight::BOLD));
            let mk = |id: String, label: &str, active: bool, dev: Option<String>| {
                let mut b = Button::new(id).label(label).font_weight(FontWeight::SEMIBOLD);
                if active {
                    b = b.primary();
                }
                let store = store.clone();
                b.on_click(move |_, _, cx| {
                    let dev = dev.clone();
                    store.update(cx, |s, cx| {
                        if matches!(
                            s.rec_status,
                            RecStatus::Recording | RecStatus::Paused
                        ) {
                            s.status = "Stop recording before switching inputs.".to_string();
                        } else {
                            s.rec_device = dev;
                            s.rec_rate_opt = None; // rate follows the new device
                            s.status = "Input selected.".to_string();
                        }
                        cx.notify();
                    });
                })
            };
            row = row.child(mk(
                "rec-dev-default".to_string(),
                "Default",
                cur_dev.is_none(),
                None,
            ));
            for d in devices.iter().take(8) {
                row = row.child(mk(
                    format!("rec-dev-{d}"),
                    d,
                    cur_dev.as_deref() == Some(d),
                    Some(d.clone()),
                ));
            }
            body = body.child(row.into_any_element());
        }

        // Rate picker: Native + driver-reported rates.
        {
            let mut row = h_flex()
                .gap_2()
                .flex_wrap()
                .child(div().child("Rate:").font_weight(FontWeight::BOLD));
            let mk = |id: String, label: String, active: bool, rate: Option<u32>| {
                let mut b = Button::new(id).label(label).font_weight(FontWeight::SEMIBOLD);
                if active {
                    b = b.primary();
                }
                let store = store.clone();
                b.on_click(move |_, _, cx| {
                    let r = rate;
                    store.update(cx, |s, cx| {
                        if matches!(
                            s.rec_status,
                            RecStatus::Recording | RecStatus::Paused
                        ) {
                            s.status = "Stop recording before switching rates.".to_string();
                        } else {
                            s.rec_rate_opt = r;
                            s.status = "Rate selected.".to_string();
                        }
                        cx.notify();
                    });
                })
            };
            row = row.child(mk(
                "rec-rate-native".to_string(),
                "Native".to_string(),
                cur_rate.is_none(),
                None,
            ));
            for r in rates.iter().take(6) {
                row = row.child(mk(
                    format!("rec-rate-{r}"),
                    format!("{r} Hz"),
                    cur_rate == Some(*r),
                    Some(*r),
                ));
            }
            body = body.child(row.into_any_element());
        }

        // Transport: Record / Play / Pause / Stop / Clear / Send to queue.
        {
            let s_rec = store.clone();
            let s_play = store.clone();
            let s_pause = store.clone();
            let s_stop = store.clone();
            let s_clear = store.clone();
            let s_send = store.clone();
            body = body.child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("rec-record")
                            .label(if rec_status == RecStatus::Paused {
                                "● Resume"
                            } else {
                                "● Record"
                            })
                            .danger()
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click(move |_, _, cx| {
                                let mut kick = false;
                                s_rec.update(cx, |s, cx| {
                                    match s.rec_start() {
                                        Ok(()) => kick = true,
                                        Err(e) => s.status = e,
                                    }
                                    cx.notify();
                                });
                                // Spawned outside the update: tick takes its
                                // own borrow, never nested (double-borrow
                                // panics the entity).
                                if kick {
                                    spawn_rec_tick(s_rec.clone(), cx);
                                }
                            }),
                    )
                    .child(
                        Button::new("rec-play")
                            .label("▶ Play")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click(move |_, _, cx| {
                                let mut kick = false;
                                s_play.update(cx, |s, cx| {
                                    match s.rec_play() {
                                        Ok(()) => kick = true,
                                        Err(e) => s.status = e,
                                    }
                                    cx.notify();
                                });
                                if kick {
                                    spawn_rec_tick(s_play.clone(), cx);
                                }
                            }),
                    )
                    .child(
                        Button::new("rec-pause")
                            .label("❚❚ Pause")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click(move |_, _, cx| {
                                s_pause.update(cx, |s, cx| {
                                    s.rec_pause();
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new("rec-stop")
                            .label("■ Stop")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click(move |_, _, cx| {
                                s_stop.update(cx, |s, cx| {
                                    if let Err(e) = s.rec_stop() {
                                        s.status = e;
                                    }
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new("rec-clear")
                            .label("Clear")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click(move |_, _, cx| {
                                s_clear.update(cx, |s, cx| {
                                    match s.rec_clear() {
                                        Ok(()) => {}
                                        Err(e) => s.status = e,
                                    }
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new("rec-send")
                            .label("Send to queue")
                            .primary()
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click(move |_, _, cx| {
                                s_send.update(cx, |s, cx| {
                                    match s.rec_send_to_input() {
                                        Ok(()) => {}
                                        Err(e) => s.status = e,
                                    }
                                    cx.notify();
                                });
                            }),
                    )
                    .into_any_element(),
            );
        }

        body = body
            .child(
                div()
                    .child("Takes land in Recordings; Send to queue stages them on Input. Leaving this page auto-sends an unsent take.")
                    .font_weight(FontWeight::SEMIBOLD),
            )
            .child(div().child(status).font_weight(FontWeight::SEMIBOLD));

        div()
            .flex_1()
            .h_full()
            .overflow_y_scrollbar()
            .id("record-scroll")
            .child(body)
            .into_any_element()
    }
}
