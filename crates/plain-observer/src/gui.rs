use std::collections::VecDeque;

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use plain_contracts::telemetry::{CellProbeReport, ConservationLedger};
use plain_contracts::{CellCoord, DevTelemetryPort, OverlayKind, ViewportRect};
use plain_world::WorldSimulation;

mod fonts;

pub fn run_gui(sim: WorldSimulation) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1480.0, 900.0])
            .with_title("遗电平原 — Rust World Core 开发者实时观测器 (F9)"),
        ..Default::default()
    };
    eframe::run_native(
        "Aftervolt Developer Observer",
        options,
        Box::new(|cc| {
            match fonts::install(&cc.egui_ctx) {
                Some((path, index)) => eprintln!(
                    "plain-observer: loaded CJK font {} (face {index})",
                    path.display()
                ),
                None => eprintln!(
                    "plain-observer: no usable CJK font found. Install a Simplified Chinese font or set PLAIN_OBSERVER_FONT to its path."
                ),
            }
            Ok(Box::new(ObserverApp::new(sim)))
        }),
    )
}

struct ObserverApp {
    sim: WorldSimulation,
    overlay: OverlayKind,
    playing: bool,
    fine_steps_per_frame: u32,
    pause_on_era_change: bool,
    show_anchors: bool,
    show_basin_lines: bool,
    compare_electric_fields: bool,

    // Camera in cell coordinates
    cam_center_x: f64,
    cam_center_y: f64,
    cam_span_x: f64,

    // Cached GPU textures
    viewport_tex: Option<egui::TextureHandle>,
    comparison_tex: Option<egui::TextureHandle>,
    overview_tex: Option<egui::TextureHandle>,
    needs_overview_refresh: bool,

    // Probed cell inspection & historical telemetry plots
    probed: Option<CellProbeReport>,
    ledger: ConservationLedger,
    water_history: VecDeque<[f64; 2]>,
    moisture_history: Vec<VecDeque<[f64; 2]>>,
    frame_us_history: VecDeque<[f64; 2]>,
    sample_counter: u64,
}

impl ObserverApp {
    fn new(sim: WorldSimulation) -> Self {
        let w = sim.world.width_cells() as f64;
        let h = sim.world.height_cells() as f64;
        let region_count = sim.world.regions.len();
        let ledger = sim.conservation_ledger();
        Self {
            sim,
            overlay: OverlayKind::CompositeTerrainFluid,
            playing: false,
            fine_steps_per_frame: 1,
            pause_on_era_change: false,
            show_anchors: true,
            show_basin_lines: true,
            compare_electric_fields: false,
            cam_center_x: w * 0.5,
            cam_center_y: h * 0.52,
            cam_span_x: (w * 0.35).min(1800.0),
            viewport_tex: None,
            comparison_tex: None,
            overview_tex: None,
            needs_overview_refresh: true,
            probed: None,
            ledger,
            water_history: VecDeque::with_capacity(240),
            moisture_history: vec![VecDeque::with_capacity(240); region_count],
            frame_us_history: VecDeque::with_capacity(240),
            sample_counter: 0,
        }
    }

    fn companion_electric_overlay(&self) -> Option<OverlayKind> {
        match self.overlay {
            OverlayKind::ElectricPotential => Some(OverlayKind::ConductivityField),
            OverlayKind::ConductivityField => Some(OverlayKind::ElectricPotential),
            _ => None,
        }
    }

    fn current_viewport(&self, aspect: f64) -> ViewportRect {
        let w = self.sim.world.width_cells() as f64;
        let h = self.sim.world.height_cells() as f64;
        let span_x = self.cam_span_x.clamp(64.0, w);
        let span_y = (span_x / aspect.max(0.2)).clamp(48.0, h);
        let half_x = span_x * 0.5;
        let half_y = span_y * 0.5;
        let min_x = (self.cam_center_x - half_x).clamp(0.0, (w - span_x).max(0.0));
        let min_y = (self.cam_center_y - half_y).clamp(0.0, (h - span_y).max(0.0));
        ViewportRect {
            min_x: min_x as i32,
            min_y: min_y as i32,
            max_x: (min_x + span_x).min(w - 1.0) as i32,
            max_y: (min_y + span_y).min(h - 1.0) as i32,
        }
    }

    fn record_telemetry_sample(&mut self) {
        self.ledger = self.sim.conservation_ledger();
        self.sample_counter += 1;
        let x = self.sim.world.clock.fractional_game_day();

        if self.water_history.len() >= 240 {
            self.water_history.pop_front();
        }
        self.water_history
            .push_back([x, self.ledger.total_world_water.0 as f64]);

        for (i, r) in self.ledger.regions.iter().enumerate() {
            if let Some(q) = self.moisture_history.get_mut(i) {
                if q.len() >= 240 {
                    q.pop_front();
                }
                q.push_back([x, r.moisture_index as f64]);
            }
        }

        let prof = self.sim.profiler_snapshot();
        if self.frame_us_history.len() >= 240 {
            self.frame_us_history.pop_front();
        }
        self.frame_us_history
            .push_back([self.sample_counter as f64, prof.latest.total_us as f64]);
    }

    fn step_simulation_if_playing(&mut self, ctx: &egui::Context) {
        if self.playing {
            let mut hit_era_break = false;
            for _ in 0..self.fine_steps_per_frame {
                if self.sim.step_fine_tick() && self.pause_on_era_change {
                    hit_era_break = true;
                    break;
                }
            }
            if hit_era_break {
                self.playing = false;
            }
            self.record_telemetry_sample();
            ctx.request_repaint();
        }
    }

    fn show_top_toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top_controls").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button(if self.playing { "⏸ 暂停" } else { "▶ 运行" })
                    .clicked()
                {
                    self.playing = !self.playing;
                }
                if ui.button("+1 细步 (Tick)").clicked() {
                    self.sim.step_fine_tick();
                    self.record_telemetry_sample();
                }
                if ui.button("+1 宏步 (Macro)").clicked() {
                    self.sim.step_macro_steps(1, true);
                    self.needs_overview_refresh = true;
                    self.record_telemetry_sample();
                }
                let spd = self.sim.world.config.clock.macro_steps_per_game_day as u64;
                if ui.button("+30 游戏日").clicked() {
                    if self.pause_on_era_change {
                        self.sim.step_macro_until_era_change(30 * spd);
                    } else {
                        self.sim.step_macro_steps(30 * spd, true);
                    }
                    self.needs_overview_refresh = true;
                    self.record_telemetry_sample();
                }
                if ui.button("+365 游戏日").clicked() {
                    if self.pause_on_era_change {
                        self.sim.step_macro_until_era_change(365 * spd);
                    } else {
                        self.sim.step_macro_steps(365 * spd, true);
                    }
                    self.needs_overview_refresh = true;
                    self.record_telemetry_sample();
                }
                if ui.button("+1220 日(完整四纪元)").clicked() {
                    let days = self.sim.world.config.four_era_cycle.target_cycle_days as u64;
                    if self.pause_on_era_change {
                        self.sim.step_macro_until_era_change(days * spd);
                    } else {
                        self.sim.step_macro_steps(days * spd, true);
                    }
                    self.needs_overview_refresh = true;
                    self.record_telemetry_sample();
                }

                ui.separator();
                ui.add(
                    egui::Slider::new(&mut self.fine_steps_per_frame, 1..=32)
                        .text("细步/帧"),
                );
                ui.checkbox(&mut self.pause_on_era_change, "纪元切换断点");
                ui.checkbox(&mut self.show_basin_lines, "流域边界");
                ui.checkbox(&mut self.show_anchors, "遗迹锚点/天坑");

                ui.separator();
                egui::ComboBox::from_label("图层")
                    .selected_text(self.overlay.label())
                    .show_ui(ui, |ui| {
                        for ov in OverlayKind::ALL {
                            if ui
                                .selectable_value(&mut self.overlay, ov, ov.label())
                                .changed()
                            {
                                self.needs_overview_refresh = true;
                            }
                        }
                    });
                if self.companion_electric_overlay().is_some() {
                    ui.checkbox(&mut self.compare_electric_fields, "同帧对照");
                }
            });

            ui.horizontal(|ui| {
                ui.label(format!(
                    "Day {:.2} | MacroStep {} | Tick {} | 世界尺寸 {}x{} ({:.0}m x {:.0}m) | 周期配置 {}d",
                    self.sim.world.clock.fractional_game_day(),
                    self.sim.world.clock.macro_step.0,
                    self.sim.world.clock.tick.0,
                    self.sim.world.width_cells(),
                    self.sim.world.height_cells(),
                    self.sim.world.config.world_width_meters(),
                    self.sim.world.config.world_height_meters(),
                    self.sim.world.config.four_era_cycle.target_cycle_days
                ));
            });
        });
    }

    fn show_telemetry_sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("telemetry_panel")
            .min_width(360.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // Section A: Global Map Thumbnail (Capability 1)
                    ui.heading("1. 全局地图总览 (点击跳转)");
                    if self.needs_overview_refresh || self.overview_tex.is_none() {
                        let ov = self.sim.render_world_overview(320, 110, self.overlay);
                        let img = egui::ColorImage::from_rgba_unmultiplied(
                            [ov.width_px as usize, ov.height_px as usize],
                            &ov.rgba,
                        );
                        self.overview_tex = Some(ctx.load_texture(
                            "world_overview",
                            img,
                            egui::TextureOptions::NEAREST,
                        ));
                        self.needs_overview_refresh = false;
                    }
                    if let Some(tex) = &self.overview_tex {
                        let resp = ui.add(
                            egui::Image::new(tex)
                                .fit_to_exact_size(egui::vec2(330.0, 112.0))
                                .sense(egui::Sense::click()),
                        );
                        if resp.clicked() {
                            if let Some(pos) = resp.interact_pointer_pos() {
                                let nx =
                                    ((pos.x - resp.rect.min.x) / resp.rect.width()).clamp(0.0, 1.0);
                                let ny = (1.0 - (pos.y - resp.rect.min.y) / resp.rect.height())
                                    .clamp(0.0, 1.0);
                                self.cam_center_x = nx as f64 * self.sim.world.width_cells() as f64;
                                self.cam_center_y =
                                    ny as f64 * self.sim.world.height_cells() as f64;
                            }
                        }
                    }

                    ui.separator();
                    // Section B: Conservation & Basin Era Telemetry (Capability 3)
                    ui.heading("2. 守恒与宏观流域遥测 (Stage 2 种群位预留)");
                    let drift = self.ledger.total_world_water.0 - self.ledger.initial_world_water.0;
                    ui.label(format!(
                        "全图总水量: {} uL (漂移: {} uL)",
                        self.ledger.total_world_water.0, drift
                    ));
                    ui.label(format!(
                        "地表/地下: {} | 大气水汽: {} | 盐壳格: {}",
                        self.ledger.total_surface_fluid.0,
                        self.ledger.total_atmospheric_vapor.0,
                        self.ledger.total_salt_crust_cells
                    ));
                    ui.label(format!(
                        "Chunk 活跃/休眠/脏存盘: {} / {} / {}",
                        self.ledger.active_chunks,
                        self.ledger.sleeping_chunks,
                        self.ledger.dirty_save_chunks
                    ));

                    for r in &self.ledger.regions {
                        ui.group(|ui| {
                            ui.strong(format!("{} — {}", r.name, r.era.label_cn()));
                            ui.label(format!(
                                "气象: {} | 历时 {:.1}d | 湿:{:.2} 盐壳:{:.2} 肥:{:.2} pH:{:.1}",
                                r.weather.label_cn(),
                                r.era_age_days,
                                r.moisture_index,
                                r.salt_crust_ratio,
                                r.avg_fertility,
                                r.avg_ph
                            ));
                        });
                    }

                    // Plot moisture evolution across basins
                    Plot::new("basin_moisture_plot")
                        .height(110.0)
                        .legend(egui_plot::Legend::default())
                        .show(ui, |plot_ui| {
                            for (i, hist) in self.moisture_history.iter().enumerate() {
                                let pts: PlotPoints = hist.iter().copied().collect();
                                plot_ui.line(Line::new(pts).name(format!("流域{}湿度", i + 1)));
                            }
                        });

                    ui.separator();
                    // Section C: Individual / Cell Probe Inspector (Capability 4)
                    ui.heading("3. 网格物理与微环境探针 (左键主视图)");
                    if let Some(p) = &self.probed {
                        ui.label(format!(
                            "坐标: ({}, {}) | {:.1}m, {:.1}m | 流域 {}",
                            p.cell.x,
                            p.cell.y,
                            p.world_pos.x,
                            p.world_pos.y,
                            p.region_id.0 + 1
                        ));
                        ui.label(format!(
                            "材质: {} | 屈服 {} kPa | 酸蚀 {} | 完整度 {}",
                            p.material_name,
                            p.yield_strength_kpa,
                            p.acid_susceptibility,
                            p.integrity
                        ));
                        ui.label(format!(
                            "流体: {:?} ({} / 10000)",
                            p.fluid_kind, p.fluid_volume
                        ));
                        ui.label(format!(
                            "L1: pH={:.1} | 肥={} | 盐={} | 温={:.1} C",
                            p.ph.as_f32(),
                            p.fertility.0,
                            p.salinity.0,
                            p.temperature.as_celsius()
                        ));
                        ui.label(format!(
                            "菌群(腐/导/盐/铁): {}/{}/{}/{} | 墙阻掩码: {:#06b}",
                            p.microbe_saprotroph,
                            p.microbe_conductive,
                            p.microbe_halophile,
                            p.microbe_ferro,
                            p.flux_block_mask
                        ));
                        ui.label(format!(
                            "电势: {:.1}V | 梯度: ({:.2}, {:.2}) V/m",
                            p.electric_potential, p.electric_grad_x, p.electric_grad_y
                        ));
                    } else {
                        ui.label("点击主地图任意位置检查 L0/L1/L3 权威状态。");
                    }

                    ui.separator();
                    // Section D: Built-in Real-Time Profiler (Capability 6)
                    ui.heading("4. 实时分系统性能分析器");
                    let prof = self.sim.profiler_snapshot();
                    ui.label(format!(
                        "最新步耗时: {} us (P50={} / P95={} / P99={} us)",
                        prof.latest.total_us,
                        prof.p50_total_us,
                        prof.p95_total_us,
                        prof.p99_total_us
                    ));
                    ui.label(format!(
                        "明细 us — 流体:{} | L1场:{} | 电场:{} | 宏气候:{} | 追平:{}",
                        prof.latest.fluid_us,
                        prof.latest.fields_us,
                        prof.latest.electric_us,
                        prof.latest.macro_climate_us,
                        prof.latest.catchup_us
                    ));
                    ui.label(format!(
                        "LOD-0 活跃块: {} / {} | 估计内核内存: {:.1} MB",
                        prof.active_lod0_chunks,
                        prof.total_chunks,
                        prof.estimated_memory_bytes as f64 / (1024.0 * 1024.0)
                    ));
                    Plot::new("frame_us_plot").height(90.0).show(ui, |plot_ui| {
                        let pts: PlotPoints = self.frame_us_history.iter().copied().collect();
                        plot_ui.line(Line::new(pts).name("Core Step (us)"));
                    });
                });
            });
    }

    fn show_main_viewport(&mut self, ctx: &egui::Context) -> ViewportRect {
        egui::CentralPanel::default()
            .show(ctx, |ui| {
                let avail = ui.available_size();
                let aspect = (avail.x / avail.y.max(1.0)) as f64;
                let vp = self.current_viewport(aspect);
                // Panning and zooming update the telemetry viewport only.
                self.sim.set_dev_camera(0, vp);

                let tex_w = (avail.x as u32).clamp(160, 1280);
                let tex_h = (avail.y as u32).clamp(120, 820);
                let snap = self.sim.render_viewport(vp, tex_w, tex_h, self.overlay);
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [snap.width_px as usize, snap.height_px as usize],
                    &snap.rgba,
                );
                let tex = self.viewport_tex.insert(ctx.load_texture(
                    "main_viewport",
                    img,
                    egui::TextureOptions::NEAREST,
                ));

                let resp = ui.add(
                    egui::Image::new(&*tex)
                        .fit_to_exact_size(avail)
                        .sense(egui::Sense::click_and_drag()),
                );

                // Drag to pan
                if resp.dragged() {
                    let d = resp.drag_delta();
                    let cells_per_px_x =
                        (vp.max_x - vp.min_x) as f64 / resp.rect.width().max(1.0) as f64;
                    let cells_per_px_y =
                        (vp.max_y - vp.min_y) as f64 / resp.rect.height().max(1.0) as f64;
                    self.cam_center_x = (self.cam_center_x - d.x as f64 * cells_per_px_x)
                        .clamp(0.0, self.sim.world.width_cells() as f64);
                    self.cam_center_y = (self.cam_center_y + d.y as f64 * cells_per_px_y)
                        .clamp(0.0, self.sim.world.height_cells() as f64);
                }

                // Scroll wheel zoom
                if resp.hovered() {
                    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                    if scroll.abs() > 0.1 {
                        let zoom = if scroll > 0.0 { 0.86 } else { 1.16 };
                        self.cam_span_x = (self.cam_span_x * zoom)
                            .clamp(64.0, self.sim.world.width_cells() as f64);
                    }
                }

                // Click to probe exact cell
                if resp.clicked() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let nx = ((pos.x - resp.rect.min.x) / resp.rect.width()).clamp(0.0, 1.0);
                        let ny =
                            (1.0 - (pos.y - resp.rect.min.y) / resp.rect.height()).clamp(0.0, 1.0);
                        let wx = vp.min_x + (nx as f64 * (vp.max_x - vp.min_x) as f64) as i32;
                        let wy = vp.min_y + (ny as f64 * (vp.max_y - vp.min_y) as f64) as i32;
                        self.probed = self.sim.probe_cell(CellCoord::new(wx, wy));
                    }
                }

                // Overlay L3 basin boundaries & geological ruin anchors / sinkholes
                let painter = ui.painter_at(resp.rect);
                let to_screen = |wx: f64, wy: f64| -> egui::Pos2 {
                    let nx = ((wx - vp.min_x as f64) / (vp.max_x - vp.min_x).max(1) as f64) as f32;
                    let ny =
                        1.0 - ((wy - vp.min_y as f64) / (vp.max_y - vp.min_y).max(1) as f64) as f32;
                    egui::pos2(
                        resp.rect.min.x + nx * resp.rect.width(),
                        resp.rect.min.y + ny * resp.rect.height(),
                    )
                };

                if self.show_basin_lines {
                    for r in &self.sim.world.regions {
                        let sx = to_screen(r.x_start as f64, 0.0).x;
                        if sx >= resp.rect.min.x && sx <= resp.rect.max.x {
                            painter.line_segment(
                                [
                                    egui::pos2(sx, resp.rect.min.y),
                                    egui::pos2(sx, resp.rect.max.y),
                                ],
                                egui::Stroke::new(
                                    1.0_f32,
                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 70),
                                ),
                            );
                        }
                    }
                }

                if self.show_anchors {
                    for a in &self.sim.world.topology.ruin_anchors {
                        let p0 = to_screen(a.min_x as f64, a.max_y as f64);
                        let p1 = to_screen(a.max_x as f64, a.min_y as f64);
                        let rect = egui::Rect::from_two_pos(p0, p1);
                        if rect.intersects(resp.rect) {
                            painter.rect_stroke(
                                rect,
                                0.0_f32,
                                egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(255, 160, 64)),
                                egui::StrokeKind::Middle,
                            );
                        }
                    }
                    for sh in &self.sim.world.topology.sinkholes {
                        let p_top = to_screen(sh.x_center as f64, sh.top_y as f64);
                        let p_bot = to_screen(sh.x_center as f64, sh.bottom_y as f64);
                        painter.line_segment(
                            [p_top, p_bot],
                            egui::Stroke::new(
                                1.5_f32,
                                egui::Color32::from_rgba_unmultiplied(90, 220, 255, 130),
                            ),
                        );
                    }
                }
                vp
            })
            .inner
    }

    fn show_electric_comparison_window(&mut self, ctx: &egui::Context, viewport: ViewportRect) {
        if let Some(overlay) = self.companion_electric_overlay() {
            egui::Window::new(overlay.label())
                .id(egui::Id::new("electric_field_comparison"))
                .default_width(380.0)
                .open(&mut self.compare_electric_fields)
                .show(ctx, |ui| {
                    let width = ui.available_width().clamp(120.0, 720.0);
                    let aspect = (viewport.max_x - viewport.min_x).max(1) as f32
                        / (viewport.max_y - viewport.min_y).max(1) as f32;
                    let height = (width / aspect).min(460.0);
                    let size = egui::vec2(height * aspect, height);
                    let snapshot = self.sim.render_viewport(
                        viewport,
                        size.x.ceil() as u32,
                        size.y.ceil() as u32,
                        overlay,
                    );
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [snapshot.width_px as usize, snapshot.height_px as usize],
                        &snapshot.rgba,
                    );
                    let options = egui::TextureOptions::NEAREST;
                    let texture = match &mut self.comparison_tex {
                        Some(texture) => {
                            texture.set(image, options);
                            texture
                        }
                        slot @ None => {
                            slot.insert(ctx.load_texture("electric_comparison", image, options))
                        }
                    };
                    ui.add(egui::Image::new(&*texture).fit_to_exact_size(size));
                });
        }
    }
}

impl eframe::App for ObserverApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.step_simulation_if_playing(ctx);
        self.show_top_toolbar(ctx);
        self.show_telemetry_sidebar(ctx);
        let viewport = self.show_main_viewport(ctx);
        self.show_electric_comparison_window(ctx, viewport);
    }
}
