use super::*;

impl Editor {
    fn render_layout(
        &mut self,
        node: &LayoutNode,
        path: Vec<bool>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match node {
            LayoutNode::Panel { panel } => self.render_panel(*panel, cx),
            LayoutNode::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let horizontal = matches!(axis, Axis::Horizontal);
                let mut first_path = path.clone();
                first_path.push(false);
                let mut second_path = path.clone();
                second_path.push(true);
                let first_view = self.render_layout(first, first_path, cx);
                let second_view = self.render_layout(second, second_path, cx);
                let handle = div()
                    .id(gpui::SharedString::from(format!("handle-{path:?}")))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(horizontal, |this| this.w(px(7.0)).cursor_col_resize())
                    .when(!horizontal, |this| this.h(px(7.0)).cursor_row_resize())
                    .bg(color(&self.theme.background))
                    .hover(|this| this.bg(color(&self.theme.selected)))
                    .child(
                        div()
                            .when(horizontal, |this| this.w(px(1.0)).h(px(30.0)))
                            .when(!horizontal, |this| this.h(px(1.0)).w(px(30.0)))
                            .bg(color(&self.theme.border)),
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
                    .id(gpui::SharedString::from(format!("split-{path:?}")))
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

    fn render_panel(&mut self, panel: PanelId, cx: &mut Context<Self>) -> gpui::AnyElement {
        let title = panel_title(panel);
        let header = div()
            .id(gpui::SharedString::from(format!("header-{title}")))
            .h(px(40.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .text_sm()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(color(&self.theme.text))
            .border_b_1()
            .border_color(color(&self.theme.border))
            .cursor_move()
            .child(title)
            .child(
                div()
                    .text_color(color(&self.theme.muted_text))
                    .font_weight(gpui::FontWeight::NORMAL)
                    .child("⋮⋮"),
            )
            .on_drag(PanelDrag(panel), move |_, _, _, cx| {
                cx.new(move |_| DragGhost(title))
            });
        let content = match panel {
            PanelId::Assets => self.assets_panel(cx),
            PanelId::Preview => self.preview_panel(cx),
            PanelId::Transcript => self.transcript_panel(cx),
            PanelId::Timeline => self.timeline_panel(cx),
        };
        div()
            .id(gpui::SharedString::from(format!("panel-{title}")))
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(color(&self.theme.panel))
            .text_color(color(&self.theme.text))
            .border_1()
            .border_color(color(&self.theme.border))
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
        let mut body = div().id("asset-list").flex_1().overflow_y_scroll().p_3();
        body = body.child(
            primary_control("+ Import videos", &self.theme)
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
                control("Download English model", &self.theme)
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
                    control("Transcribe selected", &self.theme)
                        .on_click(cx.listener(|this, _, _, cx| this.transcribe_selected(cx))),
                )
                .child(
                    control("Auto cut selected", &self.theme)
                        .on_click(cx.listener(|this, _, _, cx| this.auto_cut_selected(cx))),
                ),
        );
        for asset in &self.project.assets {
            let id = asset.id.clone();
            let label = PathBuf::from(&asset.path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| asset.path.clone());
            body = body.child(
                div()
                    .id(gpui::SharedString::from(format!("asset-{id}")))
                    .h(px(56.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .mb_2()
                    .border_1()
                    .border_color(color(&self.theme.border))
                    .rounded_md()
                    .cursor_pointer()
                    .when(self.selected_asset.as_deref() == Some(&id), |this| {
                        this.bg(color(&self.theme.selected))
                    })
                    .hover(|this| this.bg(color(&self.theme.selected)))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_sm().child(label))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(color(&self.theme.muted_text))
                                    .child(format!("{} · {}", id, time_label(asset.duration_ms))),
                            ),
                    )
                    .child(div().text_color(color(&self.theme.muted_text)).child("›"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_asset = Some(id.clone());
                        this.seek_source(&id, 0, cx);
                    })),
            );
        }
        body.into_any_element()
    }

    fn preview_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
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
                .text_color(color(&self.theme.muted_text))
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
                        primary_control(if self.playing { "Pause" } else { "Play" }, &self.theme)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_play(cx))),
                    )
                    .child(
                        control("−1s", &self.theme).on_click(cx.listener(|this, _, _, cx| {
                            this.seek(this.preview_at_ms.saturating_sub(1000), cx)
                        })),
                    )
                    .child(
                        control("+1s", &self.theme).on_click(cx.listener(|this, _, _, cx| {
                            this.seek(this.preview_at_ms.saturating_add(1000), cx)
                        })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_right()
                            .text_sm()
                            .text_color(color(&self.theme.muted_text))
                            .child(format!(
                                "{} / {}",
                                time_label(self.preview_at_ms),
                                time_label(self.project.duration_ms())
                            )),
                    ),
            )
            .into_any_element()
    }

    fn transcript_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut body = div()
            .id("transcript-list")
            .flex_1()
            .overflow_y_scroll()
            .p_3();
        let entries: Vec<_> = self
            .project
            .transcript
            .iter()
            .filter(|entry| self.selected_asset.as_deref() == Some(entry.asset_id.as_str()))
            .collect();
        if entries.is_empty() {
            body = body.child(
                div()
                    .p_4()
                    .rounded_md()
                    .border_1()
                    .border_color(color(&self.theme.border))
                    .text_color(color(&self.theme.muted_text))
                    .child("Transcribe the selected asset to see its text here."),
            );
        }
        let group_size = if self.config.transcript_word_timestamps {
            8
        } else {
            1
        };
        for group in entries.chunks(group_size) {
            let first = group[0];
            let asset_id = first.asset_id.clone();
            let at_ms = first.source_start_ms;
            let text = group
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            body = body.child(
                div()
                    .id(gpui::SharedString::from(first.id.clone()))
                    .p_3()
                    .mb_2()
                    .rounded_md()
                    .border_1()
                    .border_color(color(&self.theme.border))
                    .cursor_pointer()
                    .hover(|this| this.bg(color(&self.theme.selected)))
                    .child(
                        div()
                            .text_xs()
                            .text_color(color(&self.theme.accent))
                            .mb_1()
                            .child(time_label(at_ms)),
                    )
                    .child(div().text_sm().child(text))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.seek_source(&asset_id, at_ms, cx)),
                    ),
            );
        }
        body.into_any_element()
    }

    fn timeline_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let total = self.project.duration_ms().max(1);
        let playhead = relative(self.preview_at_ms.min(total) as f32 / total as f32);
        let timeline_bounds = self.timeline_bounds.clone();
        let mut ruler = div()
            .h(px(24.0))
            .relative()
            .border_b_1()
            .border_color(color(&self.theme.border));
        for step in 0..4 {
            let at = total * step / 4;
            ruler = ruler.child(
                div()
                    .absolute()
                    .left(relative(step as f32 / 4.0))
                    .top(px(4.0))
                    .text_xs()
                    .text_color(color(&self.theme.muted_text))
                    .child(time_label(at)),
            );
        }
        let mut video = div()
            .h(px(52.0))
            .flex()
            .min_w_0()
            .bg(color(&self.theme.background));
        let mut audio = div()
            .h(px(52.0))
            .flex()
            .min_w_0()
            .bg(color(&self.theme.background));
        for segment in &self.project.segments {
            let width = relative(segment.duration_ms() as f32 / total as f32);
            let id = segment.id.clone();
            video = video.child(
                div()
                    .id(gpui::SharedString::from(format!("video-{id}")))
                    .w(width)
                    .h_full()
                    .min_w_0()
                    .overflow_hidden()
                    .border_1()
                    .border_color(color(&self.theme.accent))
                    .bg(color(&self.theme.selected))
                    .rounded_sm()
                    .p_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(color(&self.theme.muted_text))
                            .child("CLIP"),
                    )
                    .child(div().text_sm().child(segment.asset_id.clone())),
            );
            audio = audio.child(
                div()
                    .id(gpui::SharedString::from(format!("audio-{id}")))
                    .w(width)
                    .h_full()
                    .min_w_0()
                    .border_1()
                    .border_color(color(&self.theme.border))
                    .bg(color(&self.theme.panel))
                    .rounded_sm()
                    .p_2()
                    .text_sm()
                    .text_color(color(&self.theme.muted_text))
                    .child("◁  Linked audio"),
            );
        }
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
                    .child(control("Mark In", &self.theme).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.mark_in = Some(this.preview_at_ms);
                            cx.notify();
                        },
                    )))
                    .child(control("Mark Out", &self.theme).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.mark_out = Some(this.preview_at_ms);
                            cx.notify();
                        },
                    )))
                    .child(
                        control("Remove range", &self.theme)
                            .on_click(cx.listener(|this, _, _, cx| this.cut_marks(cx))),
                    )
                    .child(
                        control("Undo", &self.theme)
                            .on_click(cx.listener(|this, _, _, cx| this.undo(cx))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_right()
                            .text_sm()
                            .text_color(color(&self.theme.muted_text))
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
                            .text_color(color(&self.theme.muted_text))
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
                            .border_color(color(&self.theme.border))
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
                            .child(
                                div()
                                    .absolute()
                                    .left(playhead)
                                    .top_0()
                                    .bottom_0()
                                    .w(px(2.0))
                                    .bg(color(&self.theme.accent)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left(playhead)
                                    .top_0()
                                    .w(px(11.0))
                                    .h(px(11.0))
                                    .rounded_sm()
                                    .bg(color(&self.theme.accent)),
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = self.config.layout.clone();
        let workspace = self.render_layout(&layout, Vec::new(), cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(color(&self.theme.background))
            .text_color(color(&self.theme.text))
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
                                    .bg(color(&self.theme.text))
                                    .text_color(color(&self.theme.panel))
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
                                            .text_color(color(&self.theme.muted_text))
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
                            &self.theme,
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
                    .border_t_1()
                    .border_color(color(&self.theme.border))
                    .text_xs()
                    .text_color(color(&self.theme.muted_text))
                    .child(self.status.clone()),
            )
    }
}
