#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod analysis;
mod app;
mod commands;
mod doc;
mod formats;
mod grid;
mod jobs;
mod query;
mod rtl;
mod settings;
mod table;
mod transforms;
#[cfg(test)]
mod tests;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let files: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ficsv")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([520.0, 320.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("ficsv", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, files)))))
}
