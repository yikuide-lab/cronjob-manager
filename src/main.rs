use cronjob_manager::app::CronJobManagerApp;
use eframe::NativeOptions;
use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([600.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Cron Job Manager",
        options,
        Box::new(|cc| Ok(Box::new(CronJobManagerApp::new(cc)))),
    )
}
