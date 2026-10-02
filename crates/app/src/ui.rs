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
            .child(Icon::Grip.element().text_color(palette.muted_text))
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
        let mut body = div()
            .id("asset-list")
            .flex_1()
            .overflow_y_scroll()
            .p_3()
            .flex()
            .flex_col()
            .gap_2();
        body = body.child(
            primary_button("import-button", Some(Icon::Plus), "Import videos", &palette)
                .w_full()
                .on_click(cx.listener(|this, _, _, cx| this.prompt_import(cx))),
        );
        if self.config.model_path.is_none() {
            body = body.child(
                button(
                    "download-model",
                    Some(Icon::Download),
                    "Download model",
                    &palette,
                )
                .w_full()
                .tooltip(tooltip("Download the English transcription model", palette))
                .on_click(cx.listener(|this, _, _, cx| this.download_model(cx))),
            );
        }
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .mb_2()
                .child(
                    button("transcribe", Some(Icon::Mic), "Transcribe", &palette)
                        .w_full()
                        .tooltip(tooltip("Transcribe the selected video", palette))
                        .on_click(cx.listener(|this, _, _, cx| this.transcribe_selected(cx))),
                )
                .child(
                    button("auto-cut", Some(Icon::Sparkles), "Auto cut", &palette)
                        .w_full()
                        .tooltip(tooltip("Remove pauses from the selected video", palette))
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
            let selected = self.selected_asset.as_deref() == Some(&id);
            body = body.child(
                div()
                    .id(SharedString::from(format!("asset-{id}")))
                    .h(px(52.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .border_1()
                    .border_color(palette.border)
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |this| {
                        this.bg(palette.selected).border_color(palette.accent)
                    })
                    .hover(|this| this.bg(palette.selected))
                    .child(Icon::Film.element().text_color(if selected {
                        palette.accent
                    } else {
                        palette.muted_text
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .truncate()
                                    .child(file_label(Path::new(&asset.path))),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(palette.muted_text)
                                    .child(format!("{} · {}", id, time_label(asset.duration_ms))),
                            ),
                    )
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
                .flex_col()
                .gap_2()
                .justify_center()
                .items_center()
                .text_color(rgb(0xa1a1aa))
                .child(Icon::Film.element().size_6())
                .child("Import a video to begin")
                .into_any_element()
        };
        let timecode = |text: String| {
            div()
                .w(px(110.0))
                .flex_none()
                .text_sm()
                .font_family("monospace")
                .text_color(palette.muted_text)
                .child(text)
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
                    .px_3()
                    .child(timecode(time_label(playhead)).text_color(palette.text))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .justify_center()
                            .items_center()
                            .gap_1()
                            .child(
                                icon_button(
                                    "go-start",
                                    Icon::SkipBack,
                                    "Go to start  (Home)",
                                    &palette,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.seek(0, cx))),
                            )
                            .child(
                                icon_button("back", Icon::ChevronLeft, "Back 1 s  (←)", &palette)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.seek_by(-1000, cx)),
                                    ),
                            )
                            .child(
                                primary_button(
                                    "play",
                                    Some(if self.playing {
                                        Icon::Pause
                                    } else {
                                        Icon::Play
                                    }),
                                    "",
                                    &palette,
                                )
                                .w(px(44.0))
                                .rounded_full()
                                .tooltip(tooltip("Play / pause  (Space)", palette))
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_play(cx))),
                            )
                            .child(
                                icon_button(
                                    "forward",
                                    Icon::ChevronRight,
                                    "Forward 1 s  (→)",
                                    &palette,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.seek_by(1000, cx))),
                            )
                            .child(
                                icon_button(
                                    "go-end",
                                    Icon::SkipForward,
                                    "Go to end  (End)",
                                    &palette,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        let end = this.last_ms();
                                        this.seek(end, cx)
                                    },
                                )),
                            ),
                    )
                    .child(timecode(time_label(self.project.duration_ms())).text_right()),
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
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .rounded_md()
                        .border_1()
                        .border_color(palette.border)
                        .text_sm()
                        .text_color(palette.muted_text)
                        .child(Icon::Text.element().size_5())
                        .child("Transcribe the selected video to see its text here."),
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

    fn timeline_toolbar(&self, cx: &mut Context<Self>) -> gpui::Div {
        let palette = self.palette;
        let separator = || div().w(px(1.0)).h(px(20.0)).mx_1().bg(palette.border);
        let has_selection = !self.selected_segments.is_empty()
            || (self.mark_in.is_some() && self.mark_out.is_some());
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                button("split", Some(Icon::Scissors), "Split", &palette)
                    .tooltip(tooltip("Split the clip at the playhead  (S)", palette))
                    .on_click(cx.listener(|this, _, _, cx| this.split(cx))),
            )
            .child(
                icon_button(
                    "trim-left",
                    Icon::TrimLeft,
                    "Delete the clip's part before the playhead  (Q)",
                    &palette,
                )
                .on_click(cx.listener(|this, _, _, cx| this.trim(true, cx))),
            )
            .child(
                icon_button(
                    "trim-right",
                    Icon::TrimRight,
                    "Delete the clip's part after the playhead  (W)",
                    &palette,
                )
                .on_click(cx.listener(|this, _, _, cx| this.trim(false, cx))),
            )
            .child(
                icon_button(
                    "delete",
                    Icon::Trash,
                    "Delete selected clips or the In/Out range  (Delete)",
                    &palette,
                )
                .when(!has_selection, |this| this.opacity(0.4))
                .on_click(cx.listener(|this, _, _, cx| this.delete(cx))),
            )
            .child(separator())
            .child(
                icon_button("undo", Icon::Undo, "Undo  (Ctrl+Z)", &palette)
                    .on_click(cx.listener(|this, _, _, cx| this.undo(cx))),
            )
            .child(
                icon_button("redo", Icon::Redo, "Redo  (Ctrl+Shift+Z)", &palette)
                    .on_click(cx.listener(|this, _, _, cx| this.redo(cx))),
            )
            .child(separator())
            .child(
                icon_button("mark-in", Icon::Flag, "Mark In  (I)", &palette)
                    .on_click(cx.listener(|this, _, _, cx| this.set_mark(false, cx))),
            )
            .child(
                icon_button("mark-out", Icon::FlagEnd, "Mark Out  (O)", &palette)
                    .on_click(cx.listener(|this, _, _, cx| this.set_mark(true, cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_right()
                    .pr_2()
                    .text_xs()
                    .text_color(palette.muted_text)
                    .child(self.selection_summary()),
            )
            .child(
                icon_button("zoom-out", Icon::ZoomOut, "Zoom out  (-)", &palette).on_click(
                    cx.listener(|this, _, _, cx| {
                        let at = this.playhead_ms();
                        this.zoom_by(1.0 / 1.5, at, cx)
                    }),
                ),
            )
            .child(
                icon_button(
                    "zoom-fit",
                    Icon::ZoomFit,
                    "Fit timeline  (Shift+Z)",
                    &palette,
                )
                .on_click(cx.listener(|this, _, _, cx| this.zoom_to_fit(cx))),
            )
            .child(
                icon_button(
                    "zoom-in",
                    Icon::ZoomIn,
                    "Zoom in  (+, or Ctrl+scroll)",
                    &palette,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    let at = this.playhead_ms();
                    this.zoom_by(1.5, at, cx)
                })),
            )
    }

    fn selection_summary(&self) -> String {
        let marks = match (self.mark_in, self.mark_out) {
            (None, None) => String::new(),
            (start, end) => format!(
                "In {}  Out {}",
                start.map(time_label).unwrap_or_else(|| "—".into()),
                end.map(time_label).unwrap_or_else(|| "—".into())
            ),
        };
        match self.selected_segments.len() {
            0 => marks,
            1 => "1 clip selected".into(),
            count => format!("{count} clips selected"),
        }
    }

    fn timeline_panel(&self, playhead: u64, cx: &mut Context<Self>) -> gpui::AnyElement {
        let palette = self.palette;
        let total = self.project.duration_ms().max(1);
        let width = self.timeline_bounds.get().width;
        let content = self.content_width();
        let to_x = |ms: u64| self.ms_to_x(ms);
        let visible_ms = |x: f32| x >= -1.0 && x <= width + 1.0;

        // Ruler ticks at a spacing that stays readable at any zoom.
        let ms_per_px = total as f32 / content.max(1.0);
        let step = [
            100, 250, 500, 1_000, 2_000, 5_000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000,
            600_000, 1_800_000,
        ]
        .into_iter()
        .find(|step| *step as f32 / ms_per_px >= 90.0)
        .unwrap_or(3_600_000);
        let first_tick = (self.x_to_ms_offset(0.0) / step) * step;
        let mut ruler = div()
            .h(px(RULER_HEIGHT))
            .relative()
            .border_b_1()
            .border_color(palette.border);
        let mut tick = first_tick;
        while tick <= total && to_x(tick) <= width {
            let x = to_x(tick);
            ruler = ruler.child(
                div()
                    .absolute()
                    .left(px(x))
                    .top_0()
                    .bottom_0()
                    .border_l_1()
                    .border_color(palette.border)
                    .pl_1()
                    .pt(px(4.0))
                    .text_xs()
                    .text_color(palette.muted_text)
                    .child(ruler_label(tick, step)),
            );
            tick += step;
        }

        let lane = || div().h(px(LANE_HEIGHT)).relative().bg(palette.background);
        let (mut video, mut audio) = (lane(), lane());
        let mut position = 0;
        for segment in &self.project.segments {
            let start = position;
            position += segment.duration_ms();
            let (left, right) = (to_x(start), to_x(position));
            if right < 0.0 || left > width {
                continue;
            }
            let clip_width = (right - left - 2.0).max(1.0);
            let selected = self.selected_segments.contains(&segment.id);
            let labelled = clip_width >= MIN_LABELLED_SEGMENT_PX;
            let name = self
                .project
                .asset(&segment.asset_id)
                .map(|asset| file_label(Path::new(&asset.path)))
                .unwrap_or_else(|_| segment.asset_id.clone());
            let clip = |accent: Hsla| {
                div()
                    .absolute()
                    .top(px(4.0))
                    .bottom(px(4.0))
                    .left(px(left + 1.0))
                    .w(px(clip_width))
                    .overflow_hidden()
                    .rounded_md()
                    .border_1()
                    .when(selected, |this| this.border_2())
                    .border_color(if selected { palette.text } else { accent })
                    .bg(accent.opacity(if selected { 0.45 } else { 0.22 }))
            };
            video = video.child(clip(palette.accent).when(labelled, |this| {
                this.flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .child(Icon::Film.element().text_color(palette.text))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().text_xs().truncate().child(name))
                            .when(clip_width >= 110.0, |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .truncate()
                                        .text_color(palette.muted_text)
                                        .child(duration_label(segment.duration_ms())),
                                )
                            }),
                    )
            }));
            audio = audio.child(clip(palette.muted_text).when(labelled, |this| {
                this.flex()
                    .items_center()
                    .px_2()
                    .child(Icon::AudioLines.element().text_color(palette.muted_text))
            }));
        }

        let marks = match (self.mark_in, self.mark_out) {
            (Some(a), Some(b)) => Some((a.min(b), a.max(b))),
            (Some(a), None) | (None, Some(a)) => Some((a, a)),
            (None, None) => None,
        };
        let playhead_x = to_x(playhead);
        let timeline_bounds = self.timeline_bounds.clone();
        let lane_label = |icon: Icon, text: &'static str| {
            div()
                .h(px(LANE_HEIGHT))
                .flex()
                .items_center()
                .gap_1()
                .child(icon.element().size_3p5())
                .child(text)
        };
        let track =
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
                            timeline_bounds.set(TrackBounds {
                                left: f32::from(bounds.origin.x),
                                top: f32::from(bounds.origin.y),
                                width: f32::from(bounds.size.width),
                            });
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
                    let left = to_x(start);
                    this.child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(left))
                            .w(px((to_x(end) - left).max(2.0)))
                            .bg(palette.accent.opacity(0.15))
                            .border_l_2()
                            .border_r_2()
                            .border_color(palette.accent),
                    )
                })
                .when(visible_ms(playhead_x), |this| {
                    this.child(
                        div()
                            .absolute()
                            .left(px(playhead_x - 1.0))
                            .top_0()
                            .bottom_0()
                            .w(px(2.0))
                            .bg(palette.accent),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(playhead_x - 6.0))
                            .top_0()
                            .w(px(12.0))
                            .h(px(12.0))
                            .rounded_b_md()
                            .bg(palette.accent),
                    )
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                        window.focus(&this.focus_handle);
                        this.begin_scrub(event.position, event.modifiers, cx);
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
                .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                    this.scroll_timeline(event, cx)
                }))
                .on_drag(TimelineDrag, |_, _, _, cx| cx.new(|_| TimelineGhost))
                .on_drag_move(
                    cx.listener(|this, event: &DragMoveEvent<TimelineDrag>, _, cx| {
                        this.scrub_to(f32::from(event.event.position.x), cx);
                    }),
                );

        // A thin overview bar showing which part of a zoomed timeline is in view.
        let overview = (self.zoom > 1.0).then(|| {
            div().ml(px(66.0)).h(px(4.0)).relative().child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(self.scroll_px / content.max(1.0)))
                    .w(relative(1.0 / self.zoom))
                    .rounded_full()
                    .bg(palette.muted_text.opacity(0.5)),
            )
        });

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .p_3()
            .gap_2()
            .child(self.timeline_toolbar(cx))
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .child(
                        div()
                            .w(px(66.0))
                            .flex_none()
                            .text_xs()
                            .text_color(palette.muted_text)
                            .child(div().h(px(RULER_HEIGHT)))
                            .child(lane_label(Icon::Film, "Video"))
                            .child(lane_label(Icon::AudioLines, "Audio")),
                    )
                    .child(track),
            )
            .children(overview)
            .into_any_element()
    }

    /// The timeline moment at a horizontal offset inside the visible track.
    fn x_to_ms_offset(&self, x: f32) -> u64 {
        let content = self.content_width();
        if content <= 0.0 {
            return 0;
        }
        (((x + self.scroll_px) / content).clamp(0.0, 1.0) * self.project.duration_ms() as f32)
            as u64
    }
}

fn duration_label(ms: u64) -> String {
    if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m {:02}s", ms / 60_000, (ms / 1000) % 60)
    }
}

fn ruler_label(ms: u64, step: u64) -> String {
    let (minutes, seconds) = (ms / 60_000, (ms / 1000) % 60);
    if step < 1_000 {
        format!("{minutes}:{seconds:02}.{}", (ms % 1000) / 100)
    } else {
        format!("{minutes}:{seconds:02}")
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
            self.keep_playhead_visible(playhead);
        }
        self.clamp_scroll();
        self.follow_playhead(playhead);
        let palette = self.palette;
        let layout = self.config.layout.clone();
        let workspace = self.render_layout(&layout, Vec::new(), playhead, cx);
        let dark = self.config.theme != "light";
        div()
            .id("editor")
            .track_focus(&self.focus_handle)
            .key_context("Editor")
            .on_action(cx.listener(|this, _: &TogglePlay, _, cx| this.toggle_play(cx)))
            .on_action(cx.listener(|this, _: &StepBack, _, cx| this.seek_by(-1000, cx)))
            .on_action(cx.listener(|this, _: &StepForward, _, cx| this.seek_by(1000, cx)))
            .on_action(cx.listener(|this, _: &JumpBack, _, cx| this.seek_by(-5000, cx)))
            .on_action(cx.listener(|this, _: &JumpForward, _, cx| this.seek_by(5000, cx)))
            .on_action(cx.listener(|this, _: &PrevFrame, _, cx| this.seek_by(-FRAME_MS, cx)))
            .on_action(cx.listener(|this, _: &NextFrame, _, cx| this.seek_by(FRAME_MS, cx)))
            .on_action(cx.listener(|this, _: &PrevEdit, _, cx| this.jump_to_edit(false, cx)))
            .on_action(cx.listener(|this, _: &NextEdit, _, cx| this.jump_to_edit(true, cx)))
            .on_action(cx.listener(|this, _: &GoToStart, _, cx| this.seek(0, cx)))
            .on_action(cx.listener(|this, _: &GoToEnd, _, cx| {
                let end = this.last_ms();
                this.seek(end, cx)
            }))
            .on_action(cx.listener(|this, _: &MarkIn, _, cx| this.set_mark(false, cx)))
            .on_action(cx.listener(|this, _: &MarkOut, _, cx| this.set_mark(true, cx)))
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| this.clear_selection(cx)))
            .on_action(cx.listener(|this, _: &Split, _, cx| this.split(cx)))
            .on_action(cx.listener(|this, _: &TrimLeft, _, cx| this.trim(true, cx)))
            .on_action(cx.listener(|this, _: &TrimRight, _, cx| this.trim(false, cx)))
            .on_action(cx.listener(|this, _: &Delete, _, cx| this.delete(cx)))
            .on_action(cx.listener(|this, _: &Undo, _, cx| this.undo(cx)))
            .on_action(cx.listener(|this, _: &Redo, _, cx| this.redo(cx)))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| {
                let at = this.playhead_ms();
                this.zoom_by(1.5, at, cx)
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| {
                let at = this.playhead_ms();
                this.zoom_by(1.0 / 1.5, at, cx)
            }))
            .on_action(cx.listener(|this, _: &ZoomFit, _, cx| this.zoom_to_fit(cx)))
            .on_action(cx.listener(|this, _: &Import, _, cx| this.prompt_import(cx)))
            .on_action(cx.listener(|this, _: &Export, _, cx| this.prompt_export(cx)))
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
                                    .size(px(28.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .bg(palette.accent)
                                    .child(Icon::Scissors.element().text_color(rgb(0xffffff))),
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
                                            .child(file_label(self.store.path())),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                button("header-import", Some(Icon::Plus), "Import", &palette)
                                    .tooltip(tooltip("Import videos  (Ctrl+I)", palette))
                                    .on_click(cx.listener(|this, _, _, cx| this.prompt_import(cx))),
                            )
                            .child(
                                primary_button(
                                    "header-export",
                                    Some(Icon::Export),
                                    if self.exporting { "Exporting…" } else { "Export" },
                                    &palette,
                                )
                                .when(self.exporting, |this| this.opacity(0.6))
                                .tooltip(tooltip("Render the edit to a video file  (Ctrl+E)", palette))
                                .on_click(cx.listener(|this, _, _, cx| this.prompt_export(cx))),
                            )
                            .child(
                                icon_button(
                                    "theme",
                                    if dark { Icon::Sun } else { Icon::Moon },
                                    if dark { "Light mode" } else { "Dark mode" },
                                    &palette,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_theme(cx))),
                            ),
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
                        "Space play · S split · Q/W delete left/right · Click clip + Del · Ctrl+Z undo",
                    )),
            )
    }
}
