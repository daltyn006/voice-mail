//! Models page: strict Speech | Text columns, one row per model.
//! Catalog roles are fixed; linked models carry the role chosen at link
//! time. Slot assignment (the active pair) is orthogonal to columns.
//!
//! Layout: tier selector (Lite/Medium/Large) + the selected tier's
//! downloaded-pair panel on top; Speech | Text columns of downloaded
//! models below; Ollama scan list last.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{div, rgb, App, Context, Entity, FontWeight, IntoElement, Render, Window};

use crate::store::Store;
use crate::theme;

/// Display labels for the catalog tiers (ids stay lite/standard/full).
const TIERS: &[(&str, &str)] = &[("lite", "Lite"), ("standard", "Medium"), ("full", "Large")];

pub struct ModelsView {
    store: Entity<Store>,
}

impl ModelsView {
    pub fn new(cx: &mut App, store: Entity<Store>) -> Entity<Self> {
        store.update(cx, |s, _| {
            s.refresh_models();
            // Open on the tier that holds the current active pair, if any.
            for (id, _) in TIERS {
                if let Some((a, b, _)) = Store::tier_info(id) {
                    if a == s.active_stt && b == s.active_llm {
                        s.models_tier = id.to_string();
                        break;
                    }
                }
            }
        });
        cx.new(|_| ModelsView { store })
    }

    fn start_then_pump(store: &Entity<Store>, id: String, cx: &mut App) {
        let res = store.update(cx, |s, _| s.start_download(&id));
        match res {
            Ok(rx) => crate::store::spawn_pump(store.clone(), rx, cx),
            Err(e) => store.update(cx, |s, cx| {
                s.status = format!("Download failed: {e}");
                cx.notify();
            }),
        }
    }

    /// Vision twin: downloads text GGUF + mmproj under one id/progress.
    fn start_vision_then_pump(store: &Entity<Store>, id: String, cx: &mut App) {
        let res = store.update(cx, |s, _| s.start_vision_download(&id));
        match res {
            Ok(rx) => crate::store::spawn_pump(store.clone(), rx, cx),
            Err(e) => store.update(cx, |s, cx| {
                s.status = format!("Vision download failed: {e}");
                cx.notify();
            }),
        }
    }

    /// One card of the downloaded-pair panel: status chip, live download
    /// progress with Cancel while fetching, Download when missing.
    fn pair_card(
        store: &Entity<Store>,
        id: &str,
        dark: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (m, dl) = {
            let snap = store.read(cx);
            (
                snap.models.iter().find(|m| m.id == id).cloned(),
                snap.downloads.get(id).cloned().unwrap_or_default(),
            )
        };
        let Some(m) = m else {
            return div().into_any_element();
        };
        let size = format!("{:.1} GB", m.bytes as f64 / 1073741824.0);
        let chip = if m.complete {
            if m.active { "downloaded ✓ active ✓" } else { "downloaded ✓" }
        } else if m.present {
            "partial"
        } else {
            "missing"
        };
        let store_d = store.clone();
        let store_c = store.clone();
        let id_dl = id.to_string();
        let id_c = id.to_string();
        let mut card = h_flex()
            .gap_2()
            .flex_wrap()
            .p_1()
            .bg(rgb(theme::surface(dark)))
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .child(format!("{} ({size})", m.id))
                    .font_weight(FontWeight::SEMIBOLD),
            )
            .child(div().child(chip).font_weight(FontWeight::SEMIBOLD));
        if dl.total > 0 && !dl.done {
            let pct = (dl.downloaded as f32 / dl.total as f32 * 100.0).round();
            card = card
                .child(Progress::new(format!("tp-{id}")).value(pct))
                .child(div().child(format!("{pct}%")))
                .child(
                    Button::new(format!("tpc-{id}"))
                        .label("Cancel")
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            let id = id_c.clone();
                            store_c.update(cx, |s, cx| {
                                s.cancel_download(&id);
                                cx.notify();
                            });
                        }),
                );
        } else if !m.complete {
            // Vision rows fetch the pair (weights + projector) under one id.
            let is_vlm = m.role == "vlm";
            card = card.child(
                Button::new(format!("tpdl-{id}"))
                    .label(if is_vlm { "Download pair" } else { "Download" })
                    .primary()
                    .font_weight(FontWeight::SEMIBOLD)
                    .on_click(move |_, _, cx| {
                        if is_vlm {
                            Self::start_vision_then_pump(&store_d, id_dl.clone(), cx);
                        } else {
                            Self::start_then_pump(&store_d, id_dl.clone(), cx);
                        }
                    }),
            );
        }
        card.into_any_element()
    }

    // NOTE: compute selector moved to Settings → Processing
    // (kept strings for reference: "Automatic (GPU)", "CPU only").
    /// Tier selector row: Lite / Medium / Large. Stored in `Store.models_tier`
    /// (single source of truth); the pair panel below follows it.
    fn tier_selector(current: &str, store: &Entity<Store>) -> impl IntoElement {
        let mut row = h_flex()
            .gap_2()
            .flex_wrap()
            .child(div().child("Tier:").font_weight(FontWeight::BOLD));
        for (id, label) in TIERS {
            let mut b = Button::new(format!("tier-{id}"))
                .label(*label)
                .font_weight(FontWeight::SEMIBOLD);
            if *id == current {
                b = b.primary();
            }
            let store = store.clone();
            let id = id.to_string();
            row = row.child(b.on_click(move |_, _, cx| {
                let id = id.clone();
                store.update(cx, |s, cx| {
                    s.set_models_tier(&id);
                    cx.notify();
                });
            }));
        }
        row.into_any_element()
    }

    /// Downloaded-pair panel for the selected tier: the two model cards
    /// plus the switch that makes the pair active (the part that used to
    /// be missing — downloads alone never changed the active pair).
    /// A third Vision card follows the same pattern (weights + projector
    /// under one id); the switch also activates a complete tier VLM.
    fn tier_panel(
        store: &Entity<Store>,
        tier: &str,
        dark: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label = TIERS
            .iter()
            .find(|(id, _)| *id == tier)
            .map(|(_, l)| *l)
            .unwrap_or("Standard");
        let Some((pair_s, pair_l, _)) = Store::tier_info(tier) else {
            return div().into_any_element();
        };
        let (vlm_id, vlm_ready, vlm_active) = {
            let snap = store.read(cx);
            match snap
                .models
                .iter()
                .find(|m| m.role == "vlm" && m.tier == tier)
            {
                Some(m) => (
                    m.id.clone(),
                    m.complete,
                    snap.active_vlm == m.id,
                ),
                None => (String::new(), false, false),
            }
        };
        let (ready, active) = {
            let snap = store.read(cx);
            let done = |id: &str| snap.models.iter().any(|m| m.id == id && m.complete);
            (
                done(&pair_s) && done(&pair_l),
                snap.active_stt == pair_s && snap.active_llm == pair_l,
            )
        };
        let switch: gpui_kit::AnyElement = if active {
            div()
                .child(format!("{label} pair active ✓"))
                .font_weight(FontWeight::SEMIBOLD)
                .into_any_element()
        } else if ready {
            let store_u = store.clone();
            let (use_s, use_l, use_v) = (pair_s.clone(), pair_l.clone(), vlm_id.clone());
            let use_v_ready = vlm_ready;
            Button::new(format!("use-{tier}"))
                .label(format!("Use {label} pair"))
                .primary()
                .font_weight(FontWeight::SEMIBOLD)
                .on_click(move |_, _, cx| {
                    let (s, l, v) = (use_s.clone(), use_l.clone(), use_v.clone());
                    store_u.update(cx, |st, cx| {
                        st.set_active_pair(&s, &l);
                        if use_v_ready && !v.is_empty() {
                            st.set_active_vlm(&v);
                        }
                        cx.notify();
                    });
                })
                .into_any_element()
        } else {
            div()
                .child("Download both files, then switch.")
                .font_weight(FontWeight::SEMIBOLD)
                .into_any_element()
        };
        let mut panel = v_flex()
            .gap_1()
            .child(
                div()
                    .child(format!("{label} pair:"))
                    .font_weight(FontWeight::BOLD),
            )
            .child(Self::pair_card(store, &pair_s, dark, cx))
            .child(Self::pair_card(store, &pair_l, dark, cx))
            .child(switch);
        if !vlm_id.is_empty() {
            panel = panel
                .child(
                    div()
                        .child(format!(
                            "{label} vision:{}",
                            if vlm_active { " active ✓" } else { "" }
                        ))
                        .font_weight(FontWeight::BOLD),
                )
                .child(Self::pair_card(store, &vlm_id, dark, cx));
            if !vlm_active && vlm_ready {
                let store_v = store.clone();
                let vid = vlm_id.clone();
                panel = panel.child(
                    Button::new(format!("use-vlm-{tier}"))
                        .label(format!("Use {label} vision"))
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            let vid = vid.clone();
                            store_v.update(cx, |st, cx| {
                                st.set_active_vlm(&vid);
                                cx.notify();
                            });
                        }),
                );
            }
        }
        panel.into_any_element()
    }

    fn model_row(
        &self,
        m: &pv_backend::models::ModelStatus,
        dark: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = m.id.clone();
        let store = self.store.clone();
        let store_v = self.store.clone();
        let dl = self
            .store
            .read(cx)
            .downloads
            .get(&id)
            .cloned()
            .unwrap_or_default();
        let size = format!("{:.1} GB", m.bytes as f64 / 1073741824.0);
        let chip = if m.complete {
            if m.active { "active ✓" } else { "ready" }
        } else if m.present {
            "partial"
        } else {
            "missing"
        };

        let mut row = h_flex()
            .gap_2()
            .flex_wrap()
            .p_1()
            .bg(rgb(theme::surface(dark)))
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .child(format!("{} ({}, {})", m.id, size, m.tier))
                    .font_weight(FontWeight::SEMIBOLD),
            )
            .child(div().child(chip).font_weight(FontWeight::SEMIBOLD));

        if m.id.starts_with("ollama:") {
            // Linked rows live in their linked-role column; the toggles
            // assign the matching active slot. Vision links serve as the
            // text half (projector comes from a downloaded Vision model).
            let store_l = store.clone();
            let store_s = store.clone();
            let store_u = store.clone();
            let store_v = store.clone();
            let id_l = id.clone();
            let id_s = id.clone();
            let id_u = id.clone();
            let id_v = id.clone();
            if m.role == "vlm" {
                row = row
                    .child(
                        Button::new(format!("set-vlm-{id_v}"))
                            .label("Set as Vision (VLM)")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click(move |_, _, cx| {
                                let id = id_v.clone();
                                store_v.update(cx, |s, cx| {
                                    s.set_active_vlm(&id);
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new(format!("unl-{id_u}"))
                            .label("Unlink")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click(move |_, _, cx| {
                                let id = id_u.clone();
                                store_u.update(cx, |s, cx| {
                                    s.unlink_model(&id);
                                    cx.notify();
                                });
                            }),
                    );
            } else {
            row = row
                .child(
                    Button::new(format!("set-llm-{id_l}"))
                        .label("Set as Summarizing (LLM)")
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            let id = id_l.clone();
                            store_l.update(cx, |s, cx| {
                                let cur = s.active_stt.clone();
                                s.set_active_pair(&cur, &id);
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new(format!("set-stt-{id_s}"))
                        .label("Set as Transcribing (STT)")
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            let id = id_s.clone();
                            store_s.update(cx, |s, cx| {
                                let cur = s.active_llm.clone();
                                s.set_active_pair(&id, &cur);
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new(format!("unl-{id_u}"))
                        .label("Unlink")
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            let id = id_u.clone();
                            store_u.update(cx, |s, cx| {
                                s.unlink_model(&id);
                                cx.notify();
                            });
                        }),
                );
            }
        } else if m.complete {
            // Catalog models keep their fixed role; the toggle chooses the
            // active pair slot for that role.
            if m.role == "stt" {
                let store_t = store.clone();
                let id_t = id.clone();
                row = row.child(
                    Button::new(format!("set-stt-{id_t}"))
                        .label("Set as Transcribing (STT)")
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            let id = id_t.clone();
                            store_t.update(cx, |s, cx| {
                                let cur = s.active_llm.clone();
                                s.set_active_pair(&id, &cur);
                                cx.notify();
                            });
                        }),
                );
            } else if m.role == "llm" {
                let store_l = store.clone();
                let id_l = id.clone();
                row = row.child(
                    Button::new(format!("set-llm-{id_l}"))
                        .label("Set as Summarizing (LLM)")
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            let id = id_l.clone();
                            store_l.update(cx, |s, cx| {
                                let cur = s.active_stt.clone();
                                s.set_active_pair(&cur, &id);
                                cx.notify();
                            });
                        }),
                );
            } else if m.role == "vlm" {
                let store_v = store.clone();
                let id_v = id.clone();
                row = row.child(
                    Button::new(format!("set-vlm-{id_v}"))
                        .label("Set as Vision (VLM)")
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            let id = id_v.clone();
                            store_v.update(cx, |s, cx| {
                                s.set_active_vlm(&id);
                                cx.notify();
                            });
                        }),
                );
            }
        } else {
            let id_dl = id.clone();
            let is_vlm = m.role == "vlm";
            row = row.child(
                Button::new(format!("dl-{id}"))
                    .label(if is_vlm { "Download pair" } else { "Download" })
                    .primary()
                    .font_weight(FontWeight::SEMIBOLD)
                    .on_click({
                        let store = store.clone();
                        move |_, _, cx| {
                            if is_vlm {
                                Self::start_vision_then_pump(&store, id_dl.clone(), cx);
                            } else {
                                Self::start_then_pump(&store, id_dl.clone(), cx);
                            }
                        }
                    }),
            );
        }

        if !m.complete {
            let store_c = store.clone();
            let id_c = id.clone();
            // Live progress while fetching (same as pair_card); idle rows
            // show 0% with Cancel available.
            let pct = if dl.total > 0 && !dl.done {
                (dl.downloaded as f32 / dl.total as f32 * 100.0).round()
            } else {
                0.0
            };
            row = row
                .child(Progress::new(format!("dl-{}", id)).value(pct))
                .child(div().child(format!("{pct}%")).font_weight(FontWeight::SEMIBOLD))
                .child(
                    Button::new(format!("dlc-{id_c}"))
                        .label("Cancel")
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            store_c.update(cx, |s, cx| {
                                s.cancel_download(&id_c);
                                cx.notify();
                            });
                        }),
                );
        }
        row = row.child(
            Button::new(format!("ver-{id}"))
                .label("Verify")
                .font_weight(FontWeight::SEMIBOLD)
                .on_click(move |_, _, cx| {
                    store_v.update(cx, |s, cx| {
                        s.status = s.verify_model(&id);
                        cx.notify();
                    });
                }),
        );
        row.into_any_element()
    }
}

impl Render for ModelsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (active_pair, scan) = {
            let snap = self.store.read(cx);
            (
                format!(
                    "Active Transcriber: {} | Active Summarizer: {} | Active Vision: {}",
                    snap.active_stt,
                    snap.active_llm,
                    if snap.active_vlm.is_empty() {
                        "(none — videos run transcript-only)".to_string()
                    } else {
                        snap.active_vlm.clone()
                    }
                ),
                snap.ollama_scan.clone(),
            )
        };
        let dir = Store::effective_models_dir();
        let dark = self.store.read(cx).dark_mode;
        let tier = self.store.read(cx).models_tier.clone();

        let store_scan = self.store.clone();

        // Speech | Text | Vision columns, strictly role-mapped: every model
        // lives in exactly one column. Catalog roles are fixed; linked models
        // carry the role chosen at link time. Slot assignment is orthogonal.
        // Only downloaded (complete) models are listed — fetching happens
        // in the tier panel above, linking in the Ollama scan below.
        let models = self.store.read(cx).models.clone();
        let stt_models: Vec<_> = models
            .iter()
            .filter(|m| m.role == "stt" && m.complete)
            .cloned()
            .collect();
        let llm_models: Vec<_> = models
            .iter()
            .filter(|m| m.role == "llm" && m.complete)
            .cloned()
            .collect();
        let vlm_models: Vec<_> = models
            .iter()
            .filter(|m| m.role == "vlm" && m.complete)
            .cloned()
            .collect();

        let rows_stt: Vec<_> = stt_models
            .iter()
            .map(|m| self.model_row(m, dark, cx).into_any_element())
            .collect();
        let rows_llm: Vec<_> = llm_models
            .iter()
            .map(|m| self.model_row(m, dark, cx).into_any_element())
            .collect();
        let rows_vlm: Vec<_> = vlm_models
            .iter()
            .map(|m| self.model_row(m, dark, cx).into_any_element())
            .collect();

        let body = v_flex()
            .gap_2()
            .p_3()
            .child(
                div()
                    .child(format!("Models folder: {dir} (change in Settings → Storage)"))
                    .font_weight(FontWeight::BOLD),
            )
            .child(div().child(active_pair).font_weight(FontWeight::SEMIBOLD))
            .child(div().child("Compute: see Settings → Processing.").font_weight(FontWeight::SEMIBOLD))
            .child(Self::tier_selector(&tier, &self.store))
            .child(Self::tier_panel(&self.store, &tier, dark, cx))
            .child(
                div()
                    .child("Linked models load straight from Ollama's store — no copies, no re-downloads.")
                    .font_weight(FontWeight::SEMIBOLD),
            )
            .child(
                Button::new("ollama-scan")
                    .label("Scan Ollama library")
                    .font_weight(FontWeight::SEMIBOLD)
                    .on_click(move |_, _, cx| {
                        store_scan.update(cx, |s, cx| {
                            s.scan_ollama();
                            cx.notify();
                        });
                    }),
            )
            .child(scan_section(&scan, &self.store))
            // Side-by-side STT | LLM | Vision columns
            .child(
                h_flex()
                    .gap_4()
                    .flex_wrap()
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .child(div().child("Speech — transcribe (STT)").font_weight(FontWeight::BOLD))
                            .children(rows_stt),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .child(div().child("Text — summarize (LLM)").font_weight(FontWeight::BOLD))
                            .children(rows_llm),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .child(div().child("Vision — film frames (VLM)").font_weight(FontWeight::BOLD))
                            .children(rows_vlm),
                    ),
            );
        // Page-level scroll (matches Settings/Input): tier panel + columns +
        // Ollama list scroll inside the bounded root slot on small windows.
        div()
            .flex_1()
            .h_full()
            .overflow_y_scrollbar()
            .id("models-scroll")
            .child(body)
            .into_any_element()
    }
}

fn scan_section(
    scan: &[pv_backend::ollama::OllamaModel],
    store: &Entity<Store>,
) -> impl IntoElement {
    if scan.is_empty() {
        return div().into_any_element();
    }
    let store_l = store.clone();
    let store_s = store.clone();
    let store_v = store.clone();
    let rows: Vec<_> = scan
        .iter()
        .map(|m| {
            let name = m.name.clone();
            let gb = m.bytes as f64 / 1073741824.0;
            let store_l = store_l.clone();
            let store_s = store_s.clone();
            let store_v = store_v.clone();
            let name_l = name.clone();
            let name_s = name.clone();
            let name_v = name.clone();
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .child(format!("{} ({:.1} GB)", name, gb))
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(
                    Button::new(format!("set-llm-{name_l}"))
                        .label("Set as Summarizing (LLM)")
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            let name = name_l.clone();
                            let pump = store_l.clone();
                            store_l.update(cx, |s, cx| {
                                match s.link_ollama(&name, "llm") {
                                    Err(e) => {
                                        s.status = format!("Link failed: {e}");
                                    }
                                    Ok(rx) => {
                                        crate::store::spawn_pump(pump.clone(), rx, cx);
                                        let cur = s.active_stt.clone();
                                        let linked = format!("ollama:{name}");
                                        s.set_active_pair(&cur, &linked);
                                    }
                                }
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new(format!("set-stt-{name_s}"))
                        .label("Set as Transcribing (STT)")
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            let name = name_s.clone();
                            let pump = store_s.clone();
                            store_s.update(cx, |s, cx| {
                                match s.link_ollama(&name, "stt") {
                                    Err(e) => {
                                        s.status = format!("Link failed: {e}");
                                    }
                                    Ok(rx) => {
                                        crate::store::spawn_pump(pump.clone(), rx, cx);
                                        let cur = s.active_llm.clone();
                                        let linked = format!("ollama:{name}");
                                        s.set_active_pair(&linked, &cur);
                                    }
                                }
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new(format!("set-vlm-{name_v}"))
                        .label("Set as Vision (VLM)")
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            let name = name_v.clone();
                            let pump = store_v.clone();
                            store_v.update(cx, |s, cx| {
                                match s.link_ollama(&name, "vlm") {
                                    Err(e) => {
                                        s.status = format!("Link failed: {e}");
                                    }
                                    Ok(rx) => {
                                        crate::store::spawn_pump(pump.clone(), rx, cx);
                                        let linked = format!("ollama:{name}");
                                        s.set_active_vlm(&linked);
                                    }
                                }
                                cx.notify();
                            });
                        }),
                )
        })
        .collect();
    v_flex()
        .gap_1()
        .child(
            div()
                .child("Ollama library — link into a column:")
                .font_weight(FontWeight::BOLD),
        )
        .children(rows)
        .into_any_element()
}