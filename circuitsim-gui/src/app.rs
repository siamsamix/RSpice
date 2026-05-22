use circuitsim::{parse, run, DcResult, SimulationResult, TranResult};
use egui;
use egui_plot::{Legend, Line, Plot, PlotPoints};

use crate::theme::{
    self, card_frame, editor_frame, panel_frame, section_heading, status_chip, ACCENT, ERROR,
    PLOT_COLORS, SUCCESS, SURFACE, SURFACE_ELEVATED, TEXT_MUTED,
};

const DEFAULT_NETLIST: &str = r#"* RC charging — edit and press Run (F5)
V1 1 0 DC 5
R1 1 2 1k
C1 2 0 1u
.tran 10u 5m
.end
"#;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Example {
    RcCharge,
    VoltageDivider,
    RlCircuit,
}

impl Example {
    fn netlist(self) -> &'static str {
        match self {
            Example::RcCharge => {
                r#"* RC charging
V1 1 0 DC 5
R1 1 2 1k
C1 2 0 1u
.tran 10u 5m
.end
"#
            }
            Example::VoltageDivider => {
                r#"* Voltage divider
V1 1 0 DC 10
R1 1 2 1k
R2 2 0 1k
.op
.end
"#
            }
            Example::RlCircuit => {
                r#"* RL circuit
V1 1 0 DC 12
R1 1 2 100
L1 2 0 1m
.op
.tran 1u 1m
.end
"#
            }
        }
    }

    fn label(self) -> &'static str {
        match self {
            Example::RcCharge => "RC charge",
            Example::VoltageDivider => "Voltage divider",
            Example::RlCircuit => "RL circuit",
        }
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum ResultTab {
    #[default]
    Overview,
    Dc,
    Waveforms,
}

pub struct CircuitSimApp {
    netlist: String,
    file_label: String,
    status: Option<(bool, String)>,
    circuit_summary: Option<String>,
    dc: Option<DcResult>,
    tran: Option<TranResult>,
    plot_nodes: Vec<bool>,
    result_tab: ResultTab,
}

impl Default for CircuitSimApp {
    fn default() -> Self {
        Self {
            netlist: DEFAULT_NETLIST.to_string(),
            file_label: "untitled.cir".to_string(),
            status: None,
            circuit_summary: None,
            dc: None,
            tran: None,
            plot_nodes: vec![false; 16],
            result_tab: ResultTab::Overview,
        }
    }
}

impl CircuitSimApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self::default()
    }

    fn run_simulation(&mut self) {
        match parse(&self.netlist) {
            Ok(netlist) => {
                let summary = format!(
                    "{} nodes · {} R · {} C · {} L · {} V",
                    netlist.circuit.nodes,
                    netlist.circuit.resistors.len(),
                    netlist.circuit.capacitors.len(),
                    netlist.circuit.inductors.len(),
                    netlist.circuit.voltage_sources.len()
                );
                self.circuit_summary = Some(summary);

                match run(&netlist.circuit, &netlist.analysis) {
                    Ok(result) => {
                        self.apply_result(result);
                        self.status = Some((true, "Simulation finished".into()));
                    }
                    Err(e) => {
                        self.clear_results();
                        self.status = Some((false, e.to_string()));
                    }
                }
            }
            Err(e) => {
                self.clear_results();
                self.status = Some((false, e.to_string()));
            }
        }
    }

    fn apply_result(&mut self, result: SimulationResult) {
        self.dc = result.dc;
        self.tran = result.tran.clone();

        let max_nodes = self
            .tran
            .as_ref()
            .and_then(|t| t.points.first())
            .map(|p| p.node_voltages.len())
            .or_else(|| self.dc.as_ref().map(|d| d.node_voltages.len()))
            .unwrap_or(8);

        self.plot_nodes.resize(max_nodes, false);
        for i in 1..max_nodes.min(self.plot_nodes.len()) {
            self.plot_nodes[i] = i <= 3;
        }
    }

    fn clear_results(&mut self) {
        self.circuit_summary = None;
        self.dc = None;
        self.tran = None;
    }

    fn load_example(&mut self, ex: Example) {
        self.netlist = ex.netlist().to_string();
        self.file_label = format!("{}.cir", ex.label().to_lowercase().replace(' ', "_"));
        self.status = Some((true, format!("Loaded example: {}", ex.label())));
        self.clear_results();
    }

    fn open_file(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Netlist", &["cir", "sp", "cir", "txt"])
            .pick_file()
        {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    self.netlist = text;
                    self.file_label = path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "circuit.cir".into());
                    self.status = Some((true, format!("Opened {}", self.file_label)));
                    self.clear_results();
                }
                Err(e) => self.status = Some((false, e.to_string())),
            }
        }
    }

    fn save_file(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Netlist", &["cir"])
            .set_file_name(&self.file_label)
            .save_file()
        {
            match std::fs::write(&path, &self.netlist) {
                Ok(()) => {
                    self.file_label = path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "circuit.cir".into());
                    self.status = Some((true, format!("Saved {}", self.file_label)));
                }
                Err(e) => self.status = Some((false, e.to_string())),
            }
        }
    }
}

impl eframe::App for CircuitSimApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            self.run_simulation();
        }

        egui::TopBottomPanel::top("toolbar")
            .frame(panel_frame(SURFACE))
            .show(ctx, |ui| {
                self.toolbar(ui);
            });

        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::none()
                    .fill(SURFACE)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::symmetric(16.0, 8.0)),
            )
            .show(ctx, |ui| {
                self.status_bar(ui);
            });

        egui::SidePanel::left("editor")
            .resizable(true)
            .default_width(420.0)
            .min_width(280.0)
            .frame(
                egui::Frame::none()
                    .fill(SURFACE_ELEVATED)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(ctx, |ui| {
                self.editor_panel(ui);
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(SURFACE)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(ctx, |ui| {
                self.results_panel(ui);
            });
    }
}

impl CircuitSimApp {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("⚡ CircuitSim")
                    .strong()
                    .size(20.0)
                    .color(ACCENT),
            );
            ui.add_space(12.0);

            let run = ui.add(
                egui::Button::new(egui::RichText::new("▶  Run").strong().color(egui::Color32::WHITE))
                    .fill(ACCENT)
                    .min_size(egui::vec2(100.0, 32.0)),
            );
            if run.clicked() {
                self.run_simulation();
            }
            if ui
                .add(egui::Button::new("Open"))
                .on_hover_text("Open netlist file")
                .clicked()
            {
                self.open_file();
            }
            if ui
                .add(egui::Button::new("Save"))
                .on_hover_text("Save netlist")
                .clicked()
            {
                self.save_file();
            }

            ui.separator();

            ui.menu_button("Examples", |ui| {
                for ex in [
                    Example::RcCharge,
                    Example::VoltageDivider,
                    Example::RlCircuit,
                ] {
                    if ui.button(ex.label()).clicked() {
                        self.load_example(ex);
                        ui.close_menu();
                    }
                }
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new("F5 run · SPICE-style netlist")
                        .color(TEXT_MUTED)
                        .size(12.0),
                );
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some((ok, msg)) = &self.status {
                let color = if *ok { SUCCESS } else { ERROR };
                status_chip(ui, msg, color);
            } else {
                ui.label(
                    egui::RichText::new("Ready")
                        .color(TEXT_MUTED)
                        .italics(),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(&self.file_label)
                        .color(TEXT_MUTED)
                        .family(egui::FontFamily::Monospace),
                );
            });
        });
    }

    fn editor_panel(&mut self, ui: &mut egui::Ui) {
        section_heading(ui, "Netlist");
        ui.label(
            egui::RichText::new("R · C · L · V  —  .op  .tran")
                .color(TEXT_MUTED)
                .size(12.0),
        );
        ui.add_space(6.0);

        editor_frame().show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.netlist)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .desired_rows(28)
                        .lock_focus(true)
                        .code_editor(),
                );
            });
    }

    fn results_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            section_heading(ui, "Results");
            ui.add_space(8.0);
            ui.selectable_value(&mut self.result_tab, ResultTab::Overview, "Overview");
            ui.selectable_value(&mut self.result_tab, ResultTab::Dc, "DC");
            ui.selectable_value(
                &mut self.result_tab,
                ResultTab::Waveforms,
                "Waveforms",
            );
        });
        ui.add_space(8.0);

        match self.result_tab {
            ResultTab::Overview => self.overview_tab(ui),
            ResultTab::Dc => self.dc_tab(ui),
            ResultTab::Waveforms => self.waveforms_tab(ui),
        }
    }

    fn overview_tab(&mut self, ui: &mut egui::Ui) {
        if self.dc.is_none() && self.tran.is_none() {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.label(egui::RichText::new("⚡").size(48.0).color(ACCENT.gamma_multiply(0.4)));
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("No results yet")
                        .size(18.0)
                        .color(TEXT_MUTED),
                );
                ui.label(
                    egui::RichText::new("Edit the netlist and press Run (F5)")
                        .color(TEXT_MUTED),
                );
            });
            return;
        }

        ui.columns(2, |cols| {
            cols[0].vertical(|ui| {
                card_frame().show(ui, |ui| {
                        section_heading(ui, "Circuit");
                        if let Some(s) = &self.circuit_summary {
                            ui.label(s);
                        }
                        if let Some(dc) = &self.dc {
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new("DC operating point")
                                    .color(SUCCESS)
                                    .strong(),
                            );
                            for (i, v) in dc.node_voltages.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.label(format!("V({i})"));
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new(format!("{v:.4e} V"))
                                                    .family(egui::FontFamily::Monospace),
                                            );
                                        },
                                    );
                                });
                            }
                        }
                    });
            });

            cols[1].vertical(|ui| {
                if let Some(tran) = &self.tran {
                    card_frame().show(ui, |ui| {
                            section_heading(ui, "Transient preview");
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} points · open Waveforms for full plot",
                                    tran.points.len()
                                ))
                                .color(TEXT_MUTED)
                                .size(12.0),
                            );
                            ui.add_space(6.0);
                            self.mini_plot(ui, tran);
                        });
                }
            });
        });
    }

    fn mini_plot(&self, ui: &mut egui::Ui, tran: &TranResult) {
        let plot = Plot::new("mini")
            .height(200.0)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .show_axes([true, true])
            .show_background(true);

        plot.show(ui, |plot_ui| {
            for (node, &enabled) in self.plot_nodes.iter().enumerate() {
                if !enabled || node == 0 {
                    continue;
                }
                let points: PlotPoints = tran
                    .points
                    .iter()
                    .map(|p| {
                        [
                            p.time as f64,
                            p.node_voltages.get(node).copied().unwrap_or(0.0) as f64,
                        ]
                    })
                    .collect();
                let color = PLOT_COLORS[node % PLOT_COLORS.len()];
                plot_ui.line(
                    Line::new(points)
                        .name(format!("V({node})"))
                        .color(color)
                        .width(1.5),
                );
            }
        });
    }

    fn dc_tab(&mut self, ui: &mut egui::Ui) {
        let Some(dc) = &self.dc else {
            ui.label(egui::RichText::new("No DC analysis in netlist (.op)").color(TEXT_MUTED));
            return;
        };

        card_frame().show(ui, |ui| {
                egui::Grid::new("dc_grid")
                    .num_columns(2)
                    .spacing([24.0, 8.0])
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("Node").strong());
                        ui.label(egui::RichText::new("Voltage").strong());
                        ui.end_row();
                        for (i, v) in dc.node_voltages.iter().enumerate() {
                            ui.label(format!("V({i})"));
                            ui.label(
                                egui::RichText::new(format!("{v:.6e} V"))
                                    .family(egui::FontFamily::Monospace)
                                    .color(ACCENT),
                            );
                            ui.end_row();
                        }
                    });

                if !dc.source_currents.is_empty() {
                    ui.add_space(16.0);
                    section_heading(ui, "Source currents");
                    egui::Grid::new("src_grid")
                        .num_columns(2)
                        .spacing([24.0, 8.0])
                        .show(ui, |ui| {
                            for (i, &amp) in dc.source_currents.iter().enumerate() {
                                ui.label(format!("I(V{i})"));
                                ui.label(
                                    egui::RichText::new(format!("{amp:.6e} A"))
                                        .family(egui::FontFamily::Monospace),
                                );
                                ui.end_row();
                            }
                        });
                }
            });
    }

    fn waveforms_tab(&mut self, ui: &mut egui::Ui) {
        let Some(tran) = &self.tran else {
            ui.label(
                egui::RichText::new("No transient analysis (.tran) in netlist")
                    .color(TEXT_MUTED),
            );
            return;
        };

        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Plot nodes:").color(TEXT_MUTED));
            for i in 1..self.plot_nodes.len() {
                let mut on = self.plot_nodes[i];
                if ui.checkbox(&mut on, format!("V({i})")).changed() {
                    self.plot_nodes[i] = on;
                }
            }
        });
        ui.add_space(8.0);

        egui::Frame::none()
            .fill(SURFACE_ELEVATED)
            .stroke(egui::Stroke::new(1.0, theme::BORDER))
            .rounding(egui::Rounding::same(10.0))
            .inner_margin(egui::Margin::same(8.0))
            .show(ui, |ui| {
                let plot = Plot::new("waveforms")
                    .height(ui.available_height() - 8.0)
                    .x_axis_label("time (s)")
                    .y_axis_label("voltage (V)")
                    .legend(Legend::default().position(egui_plot::Corner::RightTop))
                    .show_background(true)
                    .allow_boxed_zoom(true);

                plot.show(ui, |plot_ui| {
                    for (node, &enabled) in self.plot_nodes.iter().enumerate() {
                        if !enabled || node == 0 {
                            continue;
                        }
                        let points: PlotPoints = tran
                            .points
                            .iter()
                            .map(|p| {
                                [
                                    p.time as f64,
                                    p.node_voltages.get(node).copied().unwrap_or(0.0) as f64,
                                ]
                            })
                            .collect();
                        let color = PLOT_COLORS[node % PLOT_COLORS.len()];
                        plot_ui.line(
                            Line::new(points)
                                .name(format!("V({node})"))
                                .color(color)
                                .width(2.0),
                        );
                    }
                });
            });
    }
}
