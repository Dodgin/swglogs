//! The desktop window (feature `gui`, on by default). One tab so far.
//!
//! The same `swglogs.exe` opens this window unless started with `--headless`;
//! the meter, server, log and source all run exactly as in headless mode and
//! stop when the window is closed.

use std::time::Duration;

use eframe::egui;

use crate::app::Running;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Logging,
}

struct App {
    running: Running,
    tab: Tab,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Logging, "Logging");
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Logging => self.logging(ui),
        });
        // keep the source label / notice fresh without burning CPU
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

impl App {
    fn logging(&self, ui: &mut egui::Ui) {
        let server_error = self.running.server_error.lock().unwrap().clone();
        let notice = self.running.meter.lock().unwrap().notice.as_ref().map(|n| n.text.clone());

        ui.add_space(8.0);
        match server_error {
            Some(e) => {
                ui.colored_label(
                    egui::Color32::LIGHT_RED,
                    format!("SWG Logs could not start its web server: {}", e),
                );
            }
            None => {
                ui.heading("SWG Logs is running");
            }
        }
        ui.add_space(10.0);
        ui.label(egui::RichText::new("Make an in game macro:").strong());
        ui.add_space(4.0);
        ui.code("/browser http://details.swglogs.com");
        ui.add_space(4.0);
        ui.label("to view your meter in-game.");
        if let Some(n) = notice {
            ui.add_space(10.0);
            ui.colored_label(egui::Color32::GOLD, n);
        }
        ui.add_space(14.0);
        ui.label(egui::RichText::new("More features coming soon.").italics());
    }
}

/// Open the window and block until the user closes it.
pub fn run(running: Running) -> Result<(), String> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([640.0, 480.0])
        .with_title("SWG Logs");
    // Window icon, bundled into the exe (the exe's own icon is embedded by
    // build.rs from assets/swglogs.ico).
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/swglogs-icon-256.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        "SWG Logs",
        options,
        Box::new(|_cc| Ok(Box::new(App { running, tab: Tab::Logging }))),
    )
    .map_err(|e| e.to_string())
}
