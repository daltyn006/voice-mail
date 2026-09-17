//! Output page: static snapshot tree plus a unified Review screen per file.
//! Review session data lives in `Store.review` (single source of truth,
//! persisted to the drafts folder + `index.json`); this view owns only the
//! editor widget entities mirroring it, rebuilt when the session id changes.
//!
//! Editors are plain text (no LSP/tree-sitter). All persistence goes through
//! `Store` (`pv-backend` confinement rules).

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, rgb, App, Context, Entity, FontWeight, IntoElement, Render, Subscription, Window,
};

use crate::store::{naive_diff, OutFile, Store};
use crate::theme;

pub struct OutputView {
    store: Entity<Store>,
    /// Which review id the editor entities were built for (None = none).
    editors_for: Option<u64>,
    rev_left: Option<Entity<EditorState>>,
    rev_right: Option<Entity<EditorState>>,
    rev_center: Option<Entity<EditorState>>,
    rename: Entity<InputState>,
    /// Which rename session the rename box was prefilled for.
    rename_for: Option<u64>,
    /// Held (never read) so editor subscriptions stay alive.
    _subs: Vec<Subscription>,
}

impl OutputView {
    pub fn new(window: &mut Window, cx: &mut App, store: Entity<Store>) -> Entity<Self> {
        let rename = cx.new(|cx| InputState::new(window, cx));
        cx.new(|_cx| OutputView {
            store,
            editors_for: None,
            rev_left: None,
            rev_right: None,
            rev_center: None,
            rename,
            rename_for: None,
            _subs: Vec::new(),
        })
    }

    fn combine(name: &str, summary: &str, raw: &str) -> String {
        format!("# {name}\n\n## Summary\n{summary}\n\n## Raw Transcript\n{raw}\n")
    }

    /// Build editor widgets for an open session. EVENT CONTEXT ONLY — never
    /// call from `render` (creating entities mid-layout re-enters the entity
    /// map while this view is leased and panics the process). Sessions only
    /// open via the Review button, which runs here in event context.
    fn build_editors(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        id: u64,
        left: String,
        right: String,
        center: String,
    ) {
        let left = cx.new(|cx| EditorState::new(window, cx).default_value(left));
        let right = cx.new(|cx| EditorState::new(window, cx).default_value(right));
        let center = cx.new(|cx| EditorState::new(window, cx).default_value(center));
        let (lsub, rsub, csub) = (left.clone(), right.clone(), center.clone());
        let store_l = self.store.clone();
        let store_r = self.store.clone();
        let store_c = self.store.clone();
        self.editors_for = Some(id);
        self.rev_left = Some(left);
        self.rev_right = Some(right);
        self.rev_center = Some(center);
        // Typing marks the Store session dirty (persisted on Back/switch).
        // Callbacks receive `&mut Self` directly — never via entity read.
        self._subs = vec![
            cx.subscribe(&lsub, move |this: &mut Self, _ed, _: &InputEvent, cx| {
                this.flush_panes(cx);
                store_l.update(cx, |s, _| s.mark_review_dirty());
            }),
            cx.subscribe(&rsub, move |this: &mut Self, _ed, _: &InputEvent, cx| {
                this.flush_panes(cx);
                store_r.update(cx, |s, _| s.mark_review_dirty());
            }),
            cx.subscribe(&csub, move |this: &mut Self, _ed, _: &InputEvent, cx| {
                this.flush_panes(cx);
                store_c.update(cx, |s, _| s.mark_review_dirty());
            }),
        ];
    }

    /// Drop editor widgets. Pure field mutation — safe anywhere, including
    /// `render` (no entity ops, no notifications).
    fn teardown_editors(&mut self) {
        self.editors_for = None;
        self.rev_left = None;
        self.rev_right = None;
        self.rev_center = None;
        self._subs = Vec::new();
    }

    /// Copy current editor texts into the Store session (no persistence).
    fn flush_panes(&self, cx: &mut App) {
        let t = |e: &Option<Entity<EditorState>>, cx: &mut App| {
            e.as_ref()
                .map(|x| x.read(cx).text().to_string())
                .unwrap_or_default()
        };
        let (l, r, c) = (t(&self.rev_left, cx), t(&self.rev_right, cx), t(&self.rev_center, cx));
        self.store.update(cx, |s, _| s.update_review_texts(l, r, c));
    }

    /// Push Store session texts back into the widgets (after merge/keep/revert).
    fn refresh_widgets(&self, window: &mut Window, cx: &mut App) {
        if let Some(r) = self.store.read(cx).review.clone() {
            if let Some(ref e) = self.rev_left {
                e.update(cx, |st, cx| st.set_value(r.left.clone(), window, cx));
            }
            if let Some(ref e) = self.rev_right {
                e.update(cx, |st, cx| st.set_value(r.right.clone(), window, cx));
            }
            if let Some(ref e) = self.rev_center {
                e.update(cx, |st, cx| st.set_value(r.center.clone(), window, cx));
            }
        }
    }

    fn row(&self, o: &OutFile, cx: &mut Context<Self>) -> impl IntoElement {
        let id = o.id;
        let store_d = self.store.clone();
        let store_r = self.store.clone();
        let this = cx.entity();
        let snap = self.store.read(cx);
        let is_open = snap.review.as_ref().is_some_and(|r| r.id == id);
        let dirty = snap.review.as_ref().is_some_and(|r| r.id == id && r.dirty);
        // Closed-but-persisted drafts (index.json) badge too, so unsaved work
        // is visible without reopening the session.
        let saved_draft = !is_open && snap.draft_badges.contains(&o.md_name);
        let label = format!(
            "{}{}{}",
            o.name,
            if o.skipped { " (no summary)" } else { "" },
            if dirty || saved_draft { " ●" } else { "" }
        );
        let dark = self.store.read(cx).dark_mode;
        let mut row = h_flex().gap_2().flex_wrap().p_1();
        if is_open {
            row = row.bg(rgb(theme::surface(dark)));
        }
        row.child(
            div()
                .flex_1()
                .truncate()
                .child(label)
                .font_weight(FontWeight::SEMIBOLD),
        )
            .child(
                Button::new(format!("orev-{id}"))
                    .label(if is_open { "Reviewing" } else { "Review" })
                    .primary()
                    .font_weight(FontWeight::SEMIBOLD)
                    .on_click({
                        let store_r = store_r.clone();
                        let this = this.clone();
                        move |_, window, cx| {
                        // Flat sequential updates (never nested): open the
                        // persistent draft first, then build widgets from it.
                        // Both steps run in event context — never in render.
                        let opened = store_r.update(cx, |s, cx| {
                            s.heal_review();
                            match s.open_review(id) {
                                Ok(()) => {
                                    cx.notify();
                                    true
                                }
                                Err(e) => {
                                    s.push_error("Output", format!("Open review failed: {e}"), String::new());
                                    cx.notify();
                                    false
                                }
                            }
                        });
                        if opened {
                            let panes = store_r.read(cx).review.clone().map(|r| (r.left, r.right, r.center));
                            if let Some((l, r, c)) = panes {
                                this.update(cx, |v, cx| {
                                    v.build_editors(window, cx, id, l, r, c);
                                    cx.notify();
                                });
                            }
                        }
                        }}),
            )
            .child(
                Button::new(format!("orn-{id}"))
                    .label("Rename")
                    .font_weight(FontWeight::SEMIBOLD)
                        .on_click({
                            let store_n = store_d.clone();
                            let this = this.clone();
                            move |_, window, cx| {
                            // Flat sequential updates: arm the session, then
                            // prefill the rename box (event context only —
                            // render never touches widget entities).
                            store_n.update(cx, |s, cx| {
                                s.rename_begin(id);
                                cx.notify();
                            });
                            let text = store_n.read(cx).renaming.clone().map(|r| r.text).unwrap_or_default();
                            this.update(cx, |v, cx| {
                                v.rename_for = Some(id);
                                v.rename.update(cx, |st, cx| st.set_value(text, window, cx));
                                cx.notify();
                            });
                            }}),
            )
            .child(
                Button::new(format!("ocp-{id}"))
                    .label("Copy")
                    .font_weight(FontWeight::SEMIBOLD)
                    .on_click({
                        let store_c = store_d.clone();
                        move |_, _, cx| {
                            store_c.update(cx, |s, cx| {
                                if let Err(e) = s.copy_output_text(id, false) {
                                    s.push_error("Output", format!("Copy failed: {e}"), String::new());
                                }
                                cx.notify();
                            });
                        }
                    }),
            )
            .child(
                Button::new(format!("ocpr-{id}"))
                    .label("Copy raw")
                    .font_weight(FontWeight::SEMIBOLD)
                    .on_click({
                        let store_c = store_d.clone();
                        move |_, _, cx| {
                            store_c.update(cx, |s, cx| {
                                if let Err(e) = s.copy_output_text(id, true) {
                                    s.push_error("Output", format!("Copy failed: {e}"), String::new());
                                }
                                cx.notify();
                            });
                        }
                    }),
            )
            .child(
                Button::new(format!("ofd-{id}"))
                    .label("Folder")
                    .font_weight(FontWeight::SEMIBOLD)
                    .on_click({
                        let store_f = store_d.clone();
                        move |_, _, cx| {
                            store_f.update(cx, |s, cx| {
                                if let Err(e) = s.reveal_output(id) {
                                    s.push_error("Output", format!("Reveal failed: {e}"), String::new());
                                }
                                cx.notify();
                            });
                        }
                    }),
            )
            .child(
                Button::new(format!("odel-{id}"))
                    .label("×")
                    .font_weight(FontWeight::SEMIBOLD)
                    .on_click({
                        let store_x = store_d.clone();
                        move |_, _, cx| {
                            store_x.update(cx, |s, cx| {
                                if let Err(e) = s.dismiss_output(id) {
                                    s.push_error("Output", format!("Hide failed: {e}"), String::new());
                                }
                                cx.notify();
                            });
                        }
                    }),
            )
            .child(
                Button::new(format!("odel-file-{id}"))
                    .label("🗑")
                    .danger()
                    .on_click({
                        let store_d = store_d.clone();
                        move |_, window, cx| {
                        let store_d = store_d.clone();
                        let name = store_d
                            .read(cx)
                            .output
                            .iter()
                            .find(|x| x.id == id)
                            .map(|o| o.name.clone())
                            .unwrap_or_default();
                        window.open_alert_dialog(cx, move |dialog, _, _| {
                            let name_inner = name.clone();
                            dialog
                                .title("Permanently delete this output?")
                                .child(div().child(format!("{name} and its files will be removed from disk.")))
                                .on_ok({
                                    let store_d = store_d.clone();
                                    move |_, _, cx| {
                                        store_d.update(cx, |s, cx| {
                                            if let Err(e) = s.delete_output(id) {
                                                s.push_error("Output", format!("Delete failed: {e}"), String::new());
                                            } else {
                                                let _ = pv_backend::drafts::delete_draft(
                                                    &pv_backend::drafts::sanitize_stem(&name_inner),
                                                );
                                            }
                                            cx.notify();
                                        });
                                        true
                                    }
                                })
                        });
                        }}),
            )
    }
}

impl Render for OutputView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // PURE render doctrine: this runs under a mutable entity lease, so it
        // must never read/update its own entity, update any entity, or create
        // entities/subscriptions. Sessions heal at mutation sites (Review
        // button heals before open; delete/dismiss/refresh heal too) — here
        // we only *observe*: a dead session renders as closed, and orphaned
        // widgets are dropped (pure field mutation, no entity ops).
        // Widget creation happens exclusively in event handlers.
        let (open_id, renaming) = {
            let snap = self.store.read(cx);
            let live: std::collections::HashSet<u64> =
                snap.output.iter().map(|o| o.id).collect();
            (
                snap.review.as_ref().map(|r| r.id).filter(|id| live.contains(id)),
                snap.renaming.clone().filter(|r| live.contains(&r.id)),
            )
        };
        if open_id.is_none() && self.editors_for.is_some() {
            self.teardown_editors();
        }
        if let Some(rid) = open_id {
            // Widgets are guaranteed by the Review-click builder; if they are
            // ever missing (shouldn't happen), show a reopen prompt instead
            // of creating entities mid-layout.
            if self.editors_for != Some(rid) {
                return div()
                    .child("Review session lost its editors — press Review again.")
                    .into_any_element();
            }
            return self.render_review(rid, window, cx).into_any_element();
        }
        let (files, count) = {
            let snap = self.store.read(cx);
            (snap.output.clone(), snap.output.len())
        };

        let store = self.store.clone();
        let store_rn = self.store.clone();
        let store_rn2 = self.store.clone();
        let this_rn = cx.entity();
        let this_cancel = cx.entity();
        let rename_state = self.rename.clone();

        let mut body = v_flex().gap_2().p_3();
        body = body.child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("out-refresh")
                        .label("Refresh")
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(move |_, _, cx| {
                            store.update(cx, |s, cx| {
                                s.refresh_output();
                                cx.notify();
                            });
                        }),
                )
                .child(
                    div()
                        .child(format!("{count} file(s) in output (static snapshot)."))
                        .font_weight(FontWeight::SEMIBOLD),
                ),
        );

        // Rename strip (prefilled by the Rename button in event context —
        // render never touches widget entities).
        if renaming.is_some() {
            body = body.child(
                h_flex()
                    .gap_2()
                    .child(div().child("New name:").font_weight(FontWeight::BOLD))
                    .child(Input::new(&rename_state))
                    .child(
                        Button::new("rn-ok")
                            .label("Save")
                            .primary()
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click(move |_, _, cx| {
                                let next = rename_state.read(cx).text().to_string();
                                let this_rn = this_rn.clone();
                                store_rn.update(cx, |s, cx| {
                                    s.rename_set_text(next);
                                    match s.rename_commit() {
                                        Ok(n) => s.status = format!("Renamed to {n}."),
                                        Err(e) => s.push_error("Output", format!("Rename failed: {e}"), String::new()),
                                    }
                                    cx.notify();
                                });
                                this_rn.update(cx, |v, cx| {
                                    v.rename_for = None;
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new("rn-cancel")
                            .label("Cancel")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let store_rn2 = store_rn2.clone();
                                let this_cancel = this_cancel.clone();
                                move |_, _, cx| {
                                    // Flat sequential updates (never nested).
                                    store_rn2.update(cx, |s, _| s.rename_cancel());
                                    this_cancel.update(cx, |v, cx| {
                                        v.rename_for = None;
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            );
        }

        let rows: Vec<_> = files.iter().map(|o| self.row(o, cx)).collect();
        body = body.child(
            v_flex()
                .gap_1()
                // Flexible (was a fixed 560px that broke small windows); the
                // page-level `output-scroll` below owns scrolling so wheel
                // events never fight between nested scrollers.
                .flex_1()
                .id("out-tree")
                .children(rows),
        );
        // Page-level bound (matches Settings/Input/Models): the list + tree
        // live inside the root's bounded slot so small windows scroll.
        div()
            .flex_1()
            .h_full()
            .overflow_y_scrollbar()
            .id("output-scroll")
            .child(body)
            .into_any_element()
    }
}

impl OutputView {
    /// Unified Review screen: breadcrumb, action bar (Merge / Keep raw /
    /// Keep summary / Submit / Revert / Back), Raw | Summary panes or the
    /// single merged center pane, plus a live unified +/- diff.
    /// Session data comes from Store; widgets are flushed before each action
    /// and refreshed after structural ones. Drafts persist to disk.
    fn render_review(&mut self, mid: u64, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (target, dark, dirty, merged) = {
            let snap = self.store.read(cx);
            let sess = snap.review.clone().unwrap_or_default();
            (
                snap.output.iter().find(|o| o.id == mid).cloned(),
                snap.dark_mode,
                sess.dirty,
                sess.merged,
            )
        };
        let name = target.as_ref().map(|o| o.name.clone()).unwrap_or_default();
        let store = self.store.clone();
        let this = cx.entity();

        // Live diff from the current pane texts (updates as you type).
        let (ltext, rtext) = match (&self.rev_left, &self.rev_right) {
            (Some(l), Some(r)) => (
                l.read(cx).text().to_string(),
                r.read(cx).text().to_string(),
            ),
            _ => (String::new(), String::new()),
        };
        let rows: Vec<_> = naive_diff(&ltext, &rtext)
            .into_iter()
            .map(|(t, text)| {
                let color = match t {
                    '+' => rgb(0x27ae60ff),
                    '-' => rgb(0xc0392bff),
                    _ => rgb(0x999999ff),
                };
                div()
                    .truncate()
                    .child(div().child(format!("{t} {text}")).text_color(color))
            })
            .collect();

        // Pane handles cloned from `&mut self` fields — no entity reads, so
        // every button below is safe to construct during render.
        let panes = (
            self.rev_left.clone(),
            self.rev_right.clone(),
            self.rev_center.clone(),
        );
        let (left, right, center) = panes.clone();
        let back_panes = panes.clone();
        v_flex()
            .gap_2()
            .p_3()
            .child(
                div()
                    .child(format!(
                        "output / {}{} (draft auto-saves)",
                        name,
                        if dirty { " — Review ●" } else { " — Review" }
                    ))
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(theme::fg(dark))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(Self::review_action(
                        "rev-merge".to_string(),
                        "Merge",
                        false,
                        &this,
                        &store,
                        name.clone(),
                        Action::Merge,
                        panes.clone(),
                        merged,
                        cx,
                    ))
                    .child(Self::review_action(
                        "rev-keep-raw".to_string(),
                        "Keep raw",
                        false,
                        &this,
                        &store,
                        name.clone(),
                        Action::KeepRaw,
                        panes.clone(),
                        merged,
                        cx,
                    ))
                    .child(Self::review_action(
                        "rev-keep-sum".to_string(),
                        "Keep summary",
                        false,
                        &this,
                        &store,
                        name.clone(),
                        Action::KeepSummary,
                        panes.clone(),
                        merged,
                        cx,
                    ))
                    .child(Self::review_action(
                        "rev-submit".to_string(),
                        "Submit",
                        true,
                        &this,
                        &store,
                        name.clone(),
                        Action::Submit,
                        panes.clone(),
                        merged,
                        cx,
                    ))
                    .child(Self::review_action(
                        "rev-revert".to_string(),
                        "Revert",
                        false,
                        &this,
                        &store,
                        name.clone(),
                        Action::Revert,
                        panes.clone(),
                        merged,
                        cx,
                    ))
                    .child(
                        Button::new("rev-back")
                            .label("Back")
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click({
                                let this = this.clone();
                                let store = store.clone();
                                let (bl, br, bc) = back_panes;
                                move |_, _, cx| {
                                    // Flush widgets → persist draft → close.
                                    // Handles were captured at build time —
                                    // no entity reads here.
                                    let t = |e: &Option<Entity<EditorState>>, cx: &mut App| {
                                        e.as_ref()
                                            .map(|x| x.read(cx).text().to_string())
                                            .unwrap_or_default()
                                    };
                                    let (l, r, c) = (t(&bl, cx), t(&br, cx), t(&bc, cx));
                                    store.update(cx, |s, _| {
                                        s.update_review_texts(l, r, c);
                                        let _ = s.persist_review();
                                        s.close_review();
                                    });
                                    this.update(cx, |v, cx| {
                                        v.teardown_editors();
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            )
            .child(if merged {
                match center {
                    Some(ref e) => Editor::new(e).h(px(300.)).into_any_element(),
                    None => div().child("(nothing merged)").into_any_element(),
                }
            } else {
                h_flex()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(
                                div()
                                    .child("− Raw transcript (editable)")
                                    .font_weight(FontWeight::BOLD),
                            )
                            .child(match left {
                                Some(ref e) => Editor::new(e).h(px(300.)).into_any_element(),
                                None => div().child("(no raw text)").into_any_element(),
                            }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(
                                div()
                                    .child("+ AI summary (editable)")
                                    .font_weight(FontWeight::BOLD),
                            )
                            .child(match right {
                                Some(ref e) => Editor::new(e).h(px(300.)).into_any_element(),
                                None => div().child("(no summary)").into_any_element(),
                            }),
                    )
                    .into_any_element()
            })
            .child(div().child("Unified (+/-)").font_weight(FontWeight::BOLD))
            .child(
                v_flex()
                    .gap_0()
                    .h(px(180.))
                    .overflow_y_scrollbar()
                    .id("rev-unified")
                    .children(rows),
            )
            .into_any_element()
    }

    /// One Review action button. Widgets flush into Store first; structural
    /// actions (merge/keep/revert) refresh widgets after. Submit persists to
    /// the `.md`, clears the draft, and closes the session.
    /// Render-purity: pane handles + flags arrive as plain args computed by
    /// the caller from `&mut self` fields — this fn never reads the own
    /// entity, so it is safe to call during render. `this` is update-only.
    #[allow(clippy::too_many_arguments)]
    fn review_action(
        id: String,
        label: &str,
        primary: bool,
        this: &Entity<Self>,
        store: &Entity<Store>,
        name: String,
        action: Action,
        panes: (
            Option<Entity<EditorState>>,
            Option<Entity<EditorState>>,
            Option<Entity<EditorState>>,
        ),
        merged_now: bool,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (left, right, center) = panes;
        let this = this.clone();
        let store = store.clone();
        let mut b = Button::new(id).label(label).font_weight(FontWeight::SEMIBOLD);
        if primary {
            b = b.primary();
        }
        b.on_click(move |_, window, cx| {
            let t = |e: &Option<Entity<EditorState>>, cx: &mut App| {
                e.as_ref()
                    .map(|x| x.read(cx).text().to_string())
                    .unwrap_or_default()
            };
            let (l, r, c) = (t(&left, cx), t(&right, cx), t(&center, cx));
            match action {
                Action::Merge | Action::KeepRaw | Action::KeepSummary => {
                    store.update(cx, |s, _| {
                        s.update_review_texts(l, r, c);
                        match action {
                            Action::Merge => s.review_merge(),
                            Action::KeepRaw => s.review_keep("left"),
                            _ => s.review_keep("right"),
                        }
                    });
                    this.update(cx, |v, cx| {
                        v.refresh_widgets(window, cx);
                        cx.notify();
                    });
                }
                Action::Submit => {
                    let (lc, rc, cc) = (l.clone(), r.clone(), c.clone());
                    store.update(cx, |s, _| {
                        s.update_review_texts(l, r, c);
                    });
                    let content = if merged_now {
                        cc
                    } else {
                        Self::combine(&name, &rc, &lc)
                    };
                    let ok = store.update(cx, |s, cx| {
                        match s.submit_review(content) {
                            Ok(()) => true,
                            Err(e) => {
                                s.push_error("Output", format!("Submit failed: {e}"), String::new());
                                cx.notify();
                                false
                            }
                        }
                    });
                    if ok {
                        this.update(cx, |v, cx| {
                            v.teardown_editors();
                            cx.notify();
                        });
                    }
                }
                Action::Revert => {
                    store.update(cx, |s, _| s.review_revert());
                    this.update(cx, |v, cx| {
                        v.refresh_widgets(window, cx);
                        cx.notify();
                    });
                }
            }
        })
    }
}

/// Review action bar verbs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Merge,
    KeepRaw,
    KeepSummary,
    Submit,
    Revert,
}
