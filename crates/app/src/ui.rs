use super::*;

/// Segments narrower than this on screen skip their labels.
const MIN_LABELLED_SEGMENT_PX: f32 = 64.0;

impl Editor {
    fn render_layout(
        &mut self,
        node: &LayoutNode,
        path: Vec<bool>,
        playhead: u64,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match node {
            LayoutNode::Panel { panel } => self.render_panel(*panel, playhead, cx),
            LayoutNode::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let palette = self.palette;
                let horizontal = matches!(axis, Axis::Horizontal);
                let mut first_path = path.clone();
                first_path.push(false);
                let mut second_path = path.clone();
                second_path.push(true);
                let first_view = self.render_layout(first, first_path, playhead, cx);
                let second_view = self.render_layout(second, second_path, playhead, cx);
                let handle = div()
                    .id(SharedString::from(format!("handle-{path:?}")))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(horizontal, |this| this.w(px(7.0)).cursor_col_resize())
                    .when(!horizontal, |this| this.h(px(7.0)).cursor_row_resize())
                    .bg(palette.background)
                    .hover(|this| this.bg(palette.selected))
                    .child(
                        div()
                            .when(horizontal, |this| this.w(px(1.0)).h(px(30.0)))
                            .when(!horizontal, |this| this.h(px(1.0)).w(px(30.0)))
                            .bg(palette.border),
                    )
                    .on_drag(SplitDrag(path.clone()), |_, _, _, cx| {
                        cx.new(|_| DragGhost("Resize"))
                    });
                let first_box = div()
                    .min_w_0()
                    .min_h_0()
                    .when(horizontal, |this| this.w(relative(*ratio)))
                    .when(!horizontal, |this| this.h(relative(*ratio)))
                    .child(first_view);
                let second_box = div().flex_1().min_w_0().min_h_0().child(second_view);
                div()
                    .id(SharedString::from(format!("split-{path:?}")))
                    .size_full()
                    .flex()
                    .when(horizontal, |this| this.flex_row())
                    .when(!horizontal, |this| this.flex_col())
                    .child(first_box)
                    .child(handle)
                    .child(second_box)
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<SplitDrag>, _, cx| {
                            if event.drag(cx).0 != path {
                                return;
                            }
                            let position = event.event.position;
                            let bounds = event.bounds;
                            let ratio = if horizontal {
                                f32::from(position.x - bounds.origin.x)
                                    / f32::from(bounds.size.width)
                            } else {
                                f32::from(position.y - bounds.origin.y)
                                    / f32::from(bounds.size.height)
                            };
                            if let Some(LayoutNode::Split { ratio: value, .. }) =
                                node_at_mut(&mut this.config.layout, &path)
                            {
                                *value = ratio.clamp(0.10, 0.90);
                                cx.notify();
                            }
                        },
                    ))
                    .on_drop(cx.listener(|this, drag: &SplitDrag, _, cx| {
                        if node_at_mut(&mut this.config.layout, &drag.0).is_some() {
                            this.persist_layout();
                            cx.notify();
                        }
                    }))
                    .into_any_element()
            }
        }
    }

    fn render_panel(
        &mut self,
        panel: PanelId,
        playhead: u64,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let palette = self.palette;
        let title = panel_title(panel);
        let header = div()
            .id(SharedString::from(format!("header-{title}")))
            .h(px(40.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .text_sm()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(palette.text)
            .border_b_1()
            .border_color(palette.border)
            .cursor_move()
            .child(title)
            .child(
                div()
                    .text_color(palette.muted_text)
                    .font_weight(gpui::FontWeight::NORMAL)
                    .child("⋮⋮"),
            )
            .on_drag(PanelDrag(panel), move |_, _, _, cx| {
                cx.new(move |_| DragGhost(title))
            });
        let content = match panel {
            PanelId::Assets => self.assets_panel(cx),
            PanelId::Preview => self.preview_panel(playhead, cx),
            PanelId::Transcript => self.transcript_panel(cx),
            PanelId::Timeline => self.timeline_panel(playhead, cx),
        };
        div()
            .id(SharedString::from(format!("panel-{title}")))
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(palette.panel)
            .text_color(palette.text)
            .border_1()
            .border_color(palette.border)
            .rounded_lg()
            .shadow_sm()
            .overflow_hidden()
            .child(header)
            .child(content)
            .on_drop(cx.listener(move |this, drag: &PanelDrag, _, cx| {
                if drag.0 != panel {
                    this.config.layout.swap(drag.0, panel);
                    this.persist_layout();
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    fn assets_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let palette = self.palette;
        let mut body = div().id("asset-list").flex_1().overflow_y_scroll().p_3();
        body = body.child(
            primary_control("+ Import videos", &palette)
                .id("import-button")
                .w_full()
                .mb_3()
                .on_click(cx.listener(|_, _, _, cx| {
                    let answer = cx.prompt_for_paths(PathPromptOptions {
                        files: true,
                        directories: false,
                        multiple: true,
                        prompt: Some("Import video".into()),
                    });
                    cx.spawn(async move |this, cx| {
                        if let Ok(Ok(Some(paths))) = answer.await {
                            let _ = this.update(cx, |this, cx| this.import_paths(paths, cx));
                        }
                    })
                    .detach();
                })),
        );
        if self.config.model_path.is_none() {
            body = body.child(
                control("Download English model", &palette)
                    .mb_2()
                    .on_click(cx.listener(|this, _, _, cx| this.download_model(cx))),
            );
        }
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .mb_4()
                .child(
                    control("Transcribe selected", &palette)
                        .on_click(cx.listener(|this, _, _, cx| this.transcribe_selected(cx))),
                )
                .child(
                    control("Auto cut selected", &palette)
                        .on_click(cx.listener(|this, _, _, cx| this.auto_cut_selected(cx))),
                ),
        );
        if self.project.assets.is_empty() {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(palette.muted_text)
                    .child("Or drop video files anywhere in the window."),
            );
        }
        for asset in &self.project.assets {
            let id = asset.id.clone();
            let label = file_label(Path::new(&asset.path));
            body = body.child(
                div()
                    .id(SharedString::from(format!("asset-{id}")))
                    .h(px(56.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .mb_2()
                    .border_1()
                    .border_color(palette.border)
                    .rounded_md()
                    .cursor_pointer()
                    .when(self.selected_asset.as_deref() == Some(&id), |this| {
                        this.bg(palette.selected).border_color(palette.accent)
                    })
                    .hover(|this| this.bg(palette.selected))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_sm().truncate().child(label))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(palette.muted_text)
                                    .child(format!("{} · {}", id, time_label(asset.duration_ms))),
                            ),
                    )
                    .child(div().text_color(palette.muted_text).child("›"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_asset(id.clone());
                        this.seek_source(&id, 0, cx);
                    })),
            );
        }
        body.into_any_element()
    }

    fn preview_panel(&self, playhead: u64, cx: &mut Context<Self>) -> gpui::AnyElement {
        let palette = self.palette;
        let image = if let Some(image) = &self.preview_image {
            img(image.clone())
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .justify_center()
                .items_center()
                .text_color(palette.muted_text)
                .child("Import a video to begin")
                .into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .m_3()
                    .mb_0()
                    .rounded_md()
                    .overflow_hidden()
                    .bg(rgb(0x09090b))
                    .child(image),
            )
            .child(
                div()
                    .h(px(54.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .child(
                        primary_control(if self.playing { "Pause" } else { "Play" }, &palette)
                            .w(px(72.0))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_play(cx))),
                    )
                    .child(
                        control("−1s", &palette)
                            .on_click(cx.listener(|this, _, _, cx| this.seek_by(-1000, cx))),
                    )
                    .child(
                        control("+1s", &palette)
                            .on_click(cx.listener(|this, _, _, cx| this.seek_by(1000, cx))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_right()
                            .text_sm()
                            .text_color(palette.muted_text)
                            .child(format!(
                                "{} / {}",
                                time_label(playhead),
                                time_label(self.project.duration_ms())
                            )),
                    ),
            )
            .into_any_element()
    }

    fn transcript_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let palette = self.palette;
        if self.transcript_rows.is_empty() {
            return div()
                .flex_1()
                .p_3()
                .child(
                    div()
                        .p_4()
                        .rounded_md()
                        .border_1()
                        .border_color(palette.border)
                        .text_color(palette.muted_text)
                        .child("Transcribe the selected asset to see its text here."),
                )
                .into_any_element();
        }
        let rows = self.transcript_rows.clone();
        let active = self.active_row;
        let editor = cx.entity().downgrade();
        // Only visible passages are laid out, so long transcripts stay cheap to draw.
        let passages = list(self.transcript_list.clone(), move |index, _, _| {
            let row = &rows[index];
            let asset_id = row.asset_id.clone();
            let at_ms = row.start_ms;
            let editor = editor.clone();
            div()
                .px_3()
                .pb_2()
                .when(index == 0, |this| this.pt_3())
                .child(
                    div()
                        .id(row.id.clone())
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(palette.border)
                        .cursor_pointer()
                        .when(active == Some(index), |this| {
                            this.bg(palette.selected).border_color(palette.accent)
                        })
                        .hover(|this| this.bg(palette.selected))
                        .child(
                            div()
                                .text_xs()
                                .text_color(palette.accent)
                                .mb_1()
                                .child(row.time.clone()),
                        )
                        .child(div().text_sm().child(row.text.clone()))
                        .on_click(move |_, _, cx| {
                            let _ = editor
                                .update(cx, |this, cx| this.seek_source(&asset_id, at_ms, cx));
                        }),
                )
                .into_any_element()
        })
        .size_full();
        div().flex_1().min_h_0().child(passages).into_any_element()
    }

    fn timeline_panel(&self, playhead: u64, cx: &mut Context<Self>) -> gpui::AnyElement {
        let palette = self.palette;
        let total = self.project.duration_ms().max(1);
        let fraction = |ms: u64| relative(ms.min(total) as f32 / total as f32);
        let playhead_at = fraction(playhead);
        let timeline_bounds = self.timeline_bounds.clone();
        let track_width = self.timeline_bounds.get().1;
        let mut ruler = div()
            .h(px(24.0))
            .relative()
            .border_b_1()
            .border_color(palette.border);
        for step in 0..4 {
            let at = total * step / 4;
            ruler = ruler.child(
                div()
                    .absolute()
                    .left(relative(step as f32 / 4.0))
                    .top(px(4.0))
                    .pl_1()
                    .text_xs()
                    .text_color(palette.muted_text)
                    .child(time_label(at)),
            );
        }
        let mut video = div().h(px(52.0)).flex().min_w_0().bg(palette.background);
        let mut audio = div().h(px(52.0)).flex().min_w_0().bg(palette.background);
        for segment in &self.project.segments {
            let share = segment.duration_ms() as f32 / total as f32;
            let labelled = share * track_width >= MIN_LABELLED_SEGMENT_PX;
            video = video.child(
                div()
                    .w(relative(share))
                    .h_full()
                    .min_w_0()
                    .overflow_hidden()
                    .border_1()
                    .border_color(palette.accent)
                    .bg(palette.selected)
                    .rounded_sm()
                    .when(labelled, |this| {
                        this.p_2()
                            .child(div().text_xs().text_color(palette.muted_text).child("CLIP"))
                            .child(div().text_sm().truncate().child(segment.asset_id.clone()))
                    }),
            );
            audio = audio.child(
                div()
                    .w(relative(share))
                    .h_full()
                    .min_w_0()
                    .overflow_hidden()
                    .border_1()
                    .border_color(palette.border)
                    .bg(palette.panel)
                    .rounded_sm()
                    .when(labelled, |this| {
                        this.p_2()
                            .text_sm()
                            .text_color(palette.muted_text)
                            .truncate()
                            .child("◁  Linked audio")
                    }),
            );
        }
        let marks = match (self.mark_in, self.mark_out) {
            (Some(a), Some(b)) => Some((a.min(b), a.max(b))),
            (Some(a), None) | (None, Some(a)) => Some((a, a)),
            (None, None) => None,
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .p_3()
            .gap_3()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        control("Mark In", &palette)
                            .on_click(cx.listener(|this, _, _, cx| this.set_mark(false, cx))),
                    )
                    .child(
                        control("Mark Out", &palette)
                            .on_click(cx.listener(|this, _, _, cx| this.set_mark(true, cx))),
                    )
                    .child(
                        control("Remove range", &palette)
                            .on_click(cx.listener(|this, _, _, cx| this.cut_marks(cx))),
                    )
                    .child(
                        control("Undo", &palette)
                            .on_click(cx.listener(|this, _, _, cx| this.undo(cx))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_right()
                            .text_sm()
                            .text_color(palette.muted_text)
                            .child(format!(
                                "In {}    Out {}",
                                self.mark_in.map(time_label).unwrap_or_else(|| "—".into()),
                                self.mark_out.map(time_label).unwrap_or_else(|| "—".into())
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .child(
                        div()
                            .w(px(58.0))
                            .flex_none()
                            .text_xs()
                            .text_color(palette.muted_text)
                            .child(div().h(px(24.0)).flex().items_center().child("TIME"))
                            .child(div().h(px(52.0)).flex().items_center().child("VIDEO"))
                            .child(div().h(px(52.0)).flex().items_center().child("AUDIO")),
                    )
                    .child(
                        div()
                            .id("timeline-track")
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .cursor_pointer()
                            .rounded_md()
                            .overflow_hidden()
                            .border_1()
                            .border_color(palette.border)
                            .child(ruler)
                            .child(video)
                            .child(audio)
                            .child(
                                canvas(
                                    move |bounds, _, _| {
                                        timeline_bounds.set((
                                            f32::from(bounds.origin.x),
                                            f32::from(bounds.size.width),
                                        ));
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left_0()
                                .right_0(),
                            )
                            .when_some(marks, |this, (start, end)| {
                                this.child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .bottom_0()
                                        .left(fraction(start))
                                        .w(relative((end - start).min(total) as f32 / total as f32))
                                        .min_w(px(2.0))
                                        .bg(palette.accent.opacity(0.18))
                                        .border_l_1()
                                        .border_r_1()
                                        .border_color(palette.accent.opacity(0.6)),
                                )
                            })
                            .child(
                                div()
                                    .absolute()
                                    .left(playhead_at)
                                    .top_0()
                                    .bottom_0()
                                    .w(px(2.0))
                                    .bg(palette.accent),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left(playhead_at)
                                    .top_0()
                                    .w(px(11.0))
                                    .h(px(11.0))
                                    .rounded_sm()
                                    .bg(palette.accent),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                    this.begin_scrub(f32::from(event.position.x), cx);
                                }),
                            )
                            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                                if this.scrubbing && event.dragging() {
                                    this.scrub_to(f32::from(event.position.x), cx);
                                }
                            }))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.finish_scrub(cx)),
                            )
                            .on_mouse_up_out(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.finish_scrub(cx)),
                            )
                            .on_drag(TimelineDrag, |_, _, _, cx| cx.new(|_| TimelineGhost))
                            .on_drag_move(cx.listener(
                                |this, event: &DragMoveEvent<TimelineDrag>, _, cx| {
                                    this.scrub_to(f32::from(event.event.position.x), cx);
                                },
                            )),
                    ),
            )
            .into_any_element()
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Frames replaced since the last draw are no longer referenced; free their textures.
        for image in self.retired_images.drain(..) {
            let _ = window.drop_image(image);
        }
        let playhead = self.playhead_ms();
        if self.playing {
            // Keeps the playhead moving at the display's refresh rate.
            window.request_animation_frame();
        }
        self.follow_playhead(playhead);
        let palette = self.palette;
        let layout = self.config.layout.clone();
        let workspace = self.render_layout(&layout, Vec::new(), playhead, cx);
        div()
            .id("editor")
            .track_focus(&self.focus_handle)
            .key_context("Editor")
            .on_action(cx.listener(|this, _: &TogglePlay, _, cx| this.toggle_play(cx)))
            .on_action(cx.listener(|this, _: &StepBack, _, cx| this.seek_by(-1000, cx)))
            .on_action(cx.listener(|this, _: &StepForward, _, cx| this.seek_by(1000, cx)))
            .on_action(cx.listener(|this, _: &JumpBack, _, cx| this.seek_by(-5000, cx)))
            .on_action(cx.listener(|this, _: &JumpForward, _, cx| this.seek_by(5000, cx)))
            .on_action(cx.listener(|this, _: &GoToStart, _, cx| this.seek(0, cx)))
            .on_action(cx.listener(|this, _: &GoToEnd, _, cx| {
                let end = this.last_ms();
                this.seek(end, cx)
            }))
            .on_action(cx.listener(|this, _: &MarkIn, _, cx| this.set_mark(false, cx)))
            .on_action(cx.listener(|this, _: &MarkOut, _, cx| this.set_mark(true, cx)))
            .on_action(cx.listener(|this, _: &ClearMarks, _, cx| {
                this.mark_in = None;
                this.mark_out = None;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &RemoveRange, _, cx| this.cut_marks(cx)))
            .on_action(cx.listener(|this, _: &Undo, _, cx| this.undo(cx)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.import_paths(paths.paths().to_vec(), cx);
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(palette.background)
            .text_color(palette.text)
            .child(
                div()
                    .h(px(58.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .w(px(28.0))
                                    .h(px(28.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .bg(palette.text)
                                    .text_color(palette.panel)
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("C"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child("Clipperino"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(palette.muted_text)
                                            .child("Local video editor"),
                                    ),
                            ),
                    )
                    .child(
                        control(
                            if self.config.theme == "light" {
                                "Dark mode"
                            } else {
                                "Light mode"
                            },
                            &palette,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_theme(cx))),
                    ),
            )
            .child(div().flex_1().min_h_0().child(workspace))
            .child(
                div()
                    .h(px(30.0))
                    .flex_none()
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .border_t_1()
                    .border_color(palette.border)
                    .text_xs()
                    .text_color(palette.muted_text)
                    .child(div().min_w_0().truncate().child(self.status.clone()))
                    .child(div().flex_none().child(
                        "Space play · ←/→ 1s · Shift 5s · I/O marks · Del remove · Ctrl+Z undo",
                    )),
            )
    }
}
