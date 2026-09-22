use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use chrono::Local;
use eframe::egui;

use crate::cron::job::{CronJob, CronSource, CrontabFile};
use crate::cron::{CronSchedule, next_job_id, serialize_file, validate_schedule};
use crate::crontab::user as user_crontab;
use crate::privilege::is_root;
use crate::system_cron::{cron_d, crontab as system_crontab};

/// How often the app re-reads all sources in the background to pick up
/// external edits. Poll-based on purpose: the user crontab lives in a spool
/// file that is typically not readable (so inotify cannot cover it), while
/// this one mechanism watches all three source kinds uniformly.
const AUTO_REFRESH_INTERVAL: Duration = Duration::from_secs(20);

/// Result of reading all cron sources, produced off the UI thread.
struct LoadOutput {
    files: Vec<CrontabFile>,
    errors: Vec<String>,
    /// Produced by the periodic external-change check rather than a manual
    /// refresh; such results are only applied when nothing is in flight and
    /// the content actually changed.
    auto: bool,
}

/// Main application state for the cron job manager GUI.
pub struct CronJobManagerApp {
    sources: Vec<CrontabFile>,
    selected_source_index: Option<usize>,
    selected_job_id: Option<String>,
    /// Dismissible error from a user action (save, delete, ...).
    error_message: Option<String>,
    /// Warnings from the latest load; sources that failed to read are absent
    /// from the sidebar, so these must all stay visible.
    load_errors: Vec<String>,
    /// True while a manual refresh is reading sources in the background.
    loading: bool,
    /// True after sources were swapped because they changed on disk.
    external_reload_notice: bool,
    show_add_dialog: bool,
    show_edit_dialog: bool,
    show_delete_confirm: bool,
    form_schedule: String,
    form_command: String,
    form_user: String,
    /// Context clone so background threads can wake the UI up.
    ctx: egui::Context,
    load_tx: Sender<LoadOutput>,
    load_rx: Receiver<LoadOutput>,
}

impl Default for CronJobManagerApp {
    fn default() -> Self {
        let mut app = Self::bare();
        app.request_load(false);
        app.request_load(true);
        app
    }
}

impl CronJobManagerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self::bare();
        // Set the real context before spawning loaders: threads capture a
        // context clone to wake the UI, and a detached default context would
        // leave the first frame asleep.
        app.ctx = cc.egui_ctx.clone();
        app.request_load(false);
        app.request_load(true);
        app
    }

    /// Construct the app without starting any load.
    fn bare() -> Self {
        let (load_tx, load_rx) = mpsc::channel();
        Self {
            sources: Vec::new(),
            selected_source_index: None,
            selected_job_id: None,
            error_message: None,
            load_errors: Vec::new(),
            loading: false,
            external_reload_notice: false,
            show_add_dialog: false,
            show_edit_dialog: false,
            show_delete_confirm: false,
            form_schedule: "* * * * *".to_string(),
            form_command: String::new(),
            form_user: "root".to_string(),
            ctx: egui::Context::default(),
            load_tx,
            load_rx,
        }
    }

    /// Kick off a source read on a background thread so a slow or hung
    /// `crontab` invocation can never freeze the UI. Auto checks sleep
    /// first and then read, so their data is fresh when it arrives.
    fn request_load(&mut self, auto: bool) {
        if !auto {
            if self.loading {
                return;
            }
            self.loading = true;
        }
        let tx = self.load_tx.clone();
        let ctx = self.ctx.clone();
        thread::spawn(move || {
            if auto {
                thread::sleep(AUTO_REFRESH_INTERVAL);
            }
            let (files, errors) = read_all_sources();
            let _ = tx.send(LoadOutput {
                files,
                errors,
                auto,
            });
            ctx.request_repaint();
        });
    }

    /// Drain finished loads; called at the top of every frame.
    fn poll_load_outputs(&mut self) {
        while let Ok(out) = self.load_rx.try_recv() {
            self.apply_load_output(out);
        }
    }

    fn apply_load_output(&mut self, out: LoadOutput) {
        if out.auto {
            // Keep the periodic check chain alive no matter what happens below.
            self.request_load(true);
            if self.loading
                || self.show_add_dialog
                || self.show_edit_dialog
                || self.show_delete_confirm
            {
                return; // never clobber an in-flight load or open dialogs
            }
            if !sources_differ(&self.sources, &out.files) {
                return;
            }
            self.external_reload_notice = true;
        } else {
            self.loading = false;
            self.error_message = None;
        }

        // Keep the same source selected across the swap (directory order and
        // job ids are not stable between reads).
        let prev_source = self
            .selected_source_index
            .and_then(|i| self.sources.get(i))
            .map(|f| f.source.clone());
        self.sources = out.files;
        self.selected_source_index = prev_source
            .and_then(|src| self.sources.iter().position(|f| f.source == src))
            .or((!self.sources.is_empty()).then_some(0));
        self.selected_job_id = None;
        self.load_errors = out.errors;
    }

    fn selected_source(&self) -> Option<&CrontabFile> {
        self.selected_source_index.and_then(|i| self.sources.get(i))
    }

    fn selected_job(&self) -> Option<&CronJob> {
        let source = self.selected_source()?;
        self.selected_job_id
            .as_ref()
            .and_then(|id| source.find_job(id))
    }

    fn write_source(&mut self, index: usize) {
        if let Some(file) = self.sources.get(index) {
            let result = match &file.source {
                CronSource::User => user_crontab::write(file),
                CronSource::SystemCrontab => system_crontab::write(file),
                CronSource::CronD { .. } => cron_d::write(file),
            };
            if let Err(e) = result {
                self.error_message = Some(format!("Failed to save {}: {e}", file.source));
            }
        }
    }

    fn open_add_dialog(&mut self) {
        self.form_schedule = "* * * * *".to_string();
        self.form_command.clear();
        self.form_user = "root".to_string();
        self.show_add_dialog = true;
    }

    fn open_edit_dialog(&mut self) {
        let job_info = self.selected_job().map(|job| {
            (
                job.schedule.clone(),
                job.command.clone(),
                job.user.clone().unwrap_or_default(),
            )
        });
        if let Some((schedule, command, user)) = job_info {
            self.form_schedule = schedule;
            self.form_command = command;
            self.form_user = user;
            self.show_edit_dialog = true;
        }
    }

    /// The system source for the currently selected file, if any.
    fn current_source_kind(&self) -> Option<CronSource> {
        self.selected_source().map(|s| s.source.clone())
    }

    /// Validate the form for the given source. Returns Err with a message
    /// aimed at the inline form area.
    fn validate_form(&self, source: &CronSource) -> Result<(), String> {
        if let Err(e) = validate_schedule(self.form_schedule.trim()) {
            return Err(e.to_string());
        }
        if self.form_command.trim().is_empty() {
            return Err("Command must not be empty.".to_string());
        }
        if source.is_system() && self.form_user.trim().is_empty() {
            return Err("User is required for system cron entries.".to_string());
        }
        Ok(())
    }

    fn form_job_fields(&self, source: &CronSource) -> (String, Option<String>, String) {
        let user = if source.is_system() {
            Some(self.form_user.trim().to_string())
        } else {
            None
        };
        (
            self.form_schedule.trim().to_string(),
            user,
            self.form_command.trim().to_string(),
        )
    }

    fn add_job(&mut self) {
        let Some(index) = self.selected_source_index else {
            return;
        };
        let source = self.sources[index].source.clone();
        if !self.can_modify_source(&source) {
            self.error_message = Some("Need root privileges to modify this source.".to_string());
            return;
        }
        if let Err(msg) = self.validate_form(&source) {
            self.error_message = Some(msg);
            return;
        }

        let (schedule, user, command) = self.form_job_fields(&source);
        let job = CronJob::new(next_job_id(), source, true, schedule, user, command);
        self.sources[index].add_job(job);
        self.write_source(index);
        self.show_add_dialog = false;
    }

    fn save_edit(&mut self) {
        let Some(index) = self.selected_source_index else {
            return;
        };
        let source = self.sources[index].source.clone();
        let job_id = self.selected_job_id.clone().unwrap_or_default();
        if !self.sources[index].find_job(&job_id).is_some() {
            return;
        }

        if let Err(msg) = self.validate_form(&source) {
            self.error_message = Some(msg);
            return;
        }
        let (schedule, user, command) = self.form_job_fields(&source);
        let Some(job) = self.sources[index].find_job_mut(&job_id) else {
            return;
        };
        job.schedule = schedule;
        job.user = user;
        job.command = command;
        self.write_source(index);
        self.show_edit_dialog = false;
    }

    fn can_modify_source(&self, source: &CronSource) -> bool {
        !source.is_system() || is_root()
    }

    fn toggle_selected_job(&mut self) {
        let Some(index) = self.selected_source_index else {
            return;
        };
        let source = self.sources[index].source.clone();
        if !self.can_modify_source(&source) {
            self.error_message = Some("Need root privileges to modify this source.".to_string());
            return;
        }
        let job_id = self.selected_job_id.clone().unwrap_or_default();
        // The serializer emits the `# ` prefix for disabled jobs.
        if let Some(job) = self.sources[index].find_job_mut(&job_id) {
            job.enabled = !job.enabled;
        }
        self.write_source(index);
    }

    fn delete_selected_job(&mut self) {
        let Some(index) = self.selected_source_index else {
            return;
        };
        let source = self.sources[index].source.clone();
        if !self.can_modify_source(&source) {
            self.error_message = Some("Need root privileges to modify this source.".to_string());
            return;
        }
        let job_id = self.selected_job_id.clone().unwrap_or_default();
        if self.sources[index].remove_job(&job_id) {
            self.write_source(index);
            self.selected_job_id = None;
        }
        self.show_delete_confirm = false;
    }
}

impl eframe::App for CronJobManagerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_load_outputs();

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Cron Job Manager");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.loading {
                        ui.spinner();
                    }
                    ui.add_enabled_ui(!self.loading, |ui| {
                        if ui.button("🔄 Refresh").clicked() {
                            self.request_load(false);
                        }
                    });
                });
            });

            if !is_root() {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "⚠ Running without root privileges. System cron entries are read-only.",
                    );
                });
            }

            for err in &self.load_errors {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(ui.visuals().warn_fg_color, format!("⚠ {err}"));
                });
            }

            if self.external_reload_notice {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "↻ Sources were reloaded because they changed outside this app.",
                    );
                    if ui.button("Dismiss").clicked() {
                        self.external_reload_notice = false;
                    }
                });
            }

            if let Some(err) = self.error_message.clone() {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(ui.visuals().error_fg_color, format!("⚠ {err}"));
                    if ui.button("Dismiss").clicked() {
                        self.error_message = None;
                    }
                });
            }
        });

        egui::SidePanel::left("sources")
            .resizable(true)
            .show(ctx, |ui| {
                ui.heading("Sources");
                ui.separator();
                for (i, file) in self.sources.iter().enumerate() {
                    let label = format!("{} ({} jobs)", file.source, file.jobs().count());
                    let selected = self.selected_source_index == Some(i);
                    if ui.selectable_label(selected, label).clicked() {
                        self.selected_source_index = Some(i);
                        self.selected_job_id = None;
                    }
                }
            });

        let selected_source = self.selected_source().cloned();
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(source) = selected_source {
                let source_name = source.source.to_string();
                let is_system = source.source.is_system();
                let can_modify = !is_system || is_root();
                let jobs: Vec<_> = source.jobs().cloned().collect();

                ui.horizontal(|ui| {
                    ui.heading(source_name);
                    if is_system && !is_root() {
                        ui.colored_label(ui.visuals().warn_fg_color, "(read-only)");
                    }
                });
                ui.separator();

                ui.horizontal(|ui| {
                    ui.add_enabled_ui(can_modify, |ui| {
                        if ui.button("➕ Add").clicked() {
                            self.open_add_dialog();
                        }
                    });
                    let has_selection = self.selected_job_id.is_some();
                    ui.add_enabled_ui(can_modify && has_selection, |ui| {
                        if ui.button("✏ Edit").clicked() {
                            self.open_edit_dialog();
                        }
                    });
                    ui.add_enabled_ui(can_modify && has_selection, |ui| {
                        if ui.button("🗑 Delete").clicked() {
                            self.show_delete_confirm = true;
                        }
                    });
                    ui.add_enabled_ui(can_modify && has_selection, |ui| {
                        if ui.button("⏯ Toggle").clicked() {
                            self.toggle_selected_job();
                        }
                    });
                });

                ui.separator();

                let now = Local::now();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("jobs_table")
                        .striped(true)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("Enabled").strong());
                            ui.label(egui::RichText::new("Schedule").strong());
                            ui.label(egui::RichText::new("Description").strong());
                            if is_system {
                                ui.label(egui::RichText::new("User").strong());
                            }
                            ui.label(egui::RichText::new("Command").strong());
                            ui.label(egui::RichText::new("Next run").strong());
                            ui.label(egui::RichText::new("Select").strong());
                            ui.end_row();

                            for job in jobs {
                                let selected = self.selected_job_id.as_ref() == Some(&job.id);
                                ui.label(if job.enabled { "✓" } else { "—" });
                                ui.monospace(&job.schedule).on_hover_text(&job.schedule);
                                ui.label(schedule_description(&job.schedule));
                                if is_system {
                                    ui.label(job.user.as_deref().unwrap_or("-"));
                                }
                                ui.monospace(&job.command).on_hover_text(&job.command);
                                ui.label(next_run_label(&job, &now));
                                if ui.radio(selected, "").clicked() {
                                    self.selected_job_id = Some(job.id.clone());
                                }
                                ui.end_row();
                            }
                        });
                });
            } else {
                ui.centered_and_justified(|ui| {
                    let text = if self.loading {
                        "Loading cron sources…"
                    } else {
                        "No cron sources available."
                    };
                    ui.label(text);
                });
            }
        });

        // Add dialog.
        if self.show_add_dialog {
            let mut open = true;
            egui::Window::new("Add Cron Job")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    self.job_form(ui, true);
                });
            if !open {
                self.show_add_dialog = false;
            }
        }

        // Edit dialog.
        if self.show_edit_dialog {
            let mut open = true;
            egui::Window::new("Edit Cron Job")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    self.job_form(ui, false);
                });
            if !open {
                self.show_edit_dialog = false;
            }
        }

        // Delete confirmation.
        if self.show_delete_confirm {
            let mut open = true;
            egui::Window::new("Confirm Delete")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label("Are you sure you want to delete the selected cron job?");
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            self.delete_selected_job();
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_delete_confirm = false;
                        }
                    });
                });
            if !open {
                self.show_delete_confirm = false;
            }
        }
    }
}

impl CronJobManagerApp {
    fn job_form(&mut self, ui: &mut egui::Ui, is_add: bool) {
        let is_system = self
            .current_source_kind()
            .map(|s| s.is_system())
            .unwrap_or(false);

        egui::Grid::new("job_form_grid")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Schedule:");
                ui.text_edit_singleline(&mut self.form_schedule);
                ui.end_row();

                if is_system {
                    ui.label("User:");
                    ui.text_edit_singleline(&mut self.form_user);
                    ui.end_row();
                }

                ui.label("Command:");
                ui.text_edit_singleline(&mut self.form_command);
                ui.end_row();
            });

        ui.add_space(6.0);

        // Live feedback: validation error, or description + next-run preview.
        let source_kind = self.current_source_kind();
        match CronSchedule::parse(self.form_schedule.trim()) {
            Ok(schedule) => {
                ui.label(format!("📝 {}", schedule.describe()));
                if schedule.is_reboot() {
                    ui.label("🔁 Runs once at every boot");
                } else {
                    match schedule.next_after(&Local::now()) {
                        Some(next) => {
                            ui.label(format!("⏭ Next run: {}", next.format("%Y-%m-%d %H:%M")));
                        }
                        None => {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                "⚠ This schedule never matches a real date",
                            );
                        }
                    }
                }
            }
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e.to_string());
            }
        }
        if self.form_command.trim().is_empty() {
            ui.colored_label(ui.visuals().error_fg_color, "Command must not be empty.");
        }
        if is_system && self.form_user.trim().is_empty() {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "User is required for system cron entries.",
            );
        }

        let form_valid = source_kind
            .as_ref()
            .map(|s| self.validate_form(s).is_ok())
            .unwrap_or(false);
        let mut clicked_save = false;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(form_valid, |ui| {
                if ui.button(if is_add { "Add" } else { "Save" }).clicked() {
                    clicked_save = true;
                }
            });
            if ui.button("Cancel").clicked() {
                if is_add {
                    self.show_add_dialog = false;
                } else {
                    self.show_edit_dialog = false;
                }
            }
        });
        if clicked_save {
            if is_add {
                self.add_job();
            } else {
                self.save_edit();
            }
        }
    }
}

/// Read every cron source; failures become per-source error messages.
fn read_all_sources() -> (Vec<CrontabFile>, Vec<String>) {
    let mut files = Vec::new();
    let mut errors = Vec::new();

    match user_crontab::read() {
        Ok(file) => files.push(file),
        Err(e) => errors.push(format!("Failed to load user crontab: {e}")),
    }
    match system_crontab::read() {
        Ok(file) => files.push(file),
        Err(e) => errors.push(format!("Failed to load /etc/crontab: {e}")),
    }
    match cron_d::read_all() {
        Ok(mut more) => files.append(&mut more),
        Err(e) => errors.push(format!("Failed to load /etc/cron.d: {e}")),
    }
    (files, errors)
}

/// Whether two source snapshots differ in content (job ids are regenerated
/// per parse, so compare the serialized text per source identity).
fn sources_differ(current: &[CrontabFile], fresh: &[CrontabFile]) -> bool {
    fn fingerprint(files: &[CrontabFile]) -> Vec<(CronSource, String)> {
        files
            .iter()
            .map(|f| (f.source.clone(), serialize_file(f)))
            .collect()
    }
    fingerprint(current) != fingerprint(fresh)
}

/// Short human-readable label for the schedule column.
fn schedule_description(schedule: &str) -> String {
    CronSchedule::parse(schedule)
        .map(|s| s.describe())
        .unwrap_or_else(|_| "(invalid)".to_string())
}

/// "Next run" cell text; only enabled jobs with a matching schedule have one.
fn next_run_label(job: &CronJob, now: &chrono::DateTime<Local>) -> String {
    if !job.enabled {
        return "disabled".to_string();
    }
    match CronSchedule::parse(&job.schedule) {
        Ok(schedule) if schedule.is_reboot() => "after reboot".to_string(),
        Ok(schedule) => match schedule.next_after(now) {
            Some(next) => next.format("%Y-%m-%d %H:%M").to_string(),
            None => "never".to_string(),
        },
        Err(_) => "(invalid)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn initial_load_completes_in_background() {
        let mut app = CronJobManagerApp::default();
        // The manual load must come back without the 20s auto check being
        // involved (it sleeps first, so it cannot interfere here).
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.loading && Instant::now() < deadline {
            app.poll_load_outputs();
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!app.loading, "manual load must finish");
        assert!(
            !app.sources.is_empty() || !app.load_errors.is_empty(),
            "sources loaded or per-source errors reported"
        );
    }

    #[test]
    fn auto_reload_only_applies_when_content_differs() {
        let (tx, rx) = mpsc::channel();
        let mut app = CronJobManagerApp::bare();
        app.load_tx = tx;
        app.load_rx = rx;

        // Identical content (same single source, same text): no swap, no notice.
        let mut file = CrontabFile::new(CronSource::User);
        file.add_job(CronJob::new(
            next_job_id(),
            CronSource::User,
            true,
            "* * * * *".to_string(),
            None,
            "echo x".to_string(),
        ));
        let fresh = vec![file.clone()];
        app.sources = vec![file];
        let _ = app.load_tx.send(LoadOutput {
            files: fresh,
            errors: Vec::new(),
            auto: true,
        });
        app.poll_load_outputs();
        assert!(
            !app.external_reload_notice,
            "unchanged content must not reload"
        );
        assert_eq!(app.sources[0].jobs().count(), 1);

        // Changed content: sources swapped and notice raised.
        let mut changed = CrontabFile::new(CronSource::User);
        changed.add_job(CronJob::new(
            next_job_id(),
            CronSource::User,
            true,
            "0 0 * * *".to_string(),
            None,
            "echo y".to_string(),
        ));
        let _ = app.load_tx.send(LoadOutput {
            files: vec![changed],
            errors: Vec::new(),
            auto: true,
        });
        app.poll_load_outputs();
        assert!(app.external_reload_notice, "changed content must reload");
        assert_eq!(app.sources[0].jobs().next().unwrap().command, "echo y");

        // Open dialog: auto results are withheld.
        app.show_add_dialog = true;
        let _ = app.load_tx.send(LoadOutput {
            files: Vec::new(),
            errors: Vec::new(),
            auto: true,
        });
        app.poll_load_outputs();
        assert!(!app.sources.is_empty(), "dialog must block auto swap");
    }

    #[test]
    fn selection_survives_source_swap() {
        let mut app = CronJobManagerApp::bare();
        app.sources = vec![
            CrontabFile::new(CronSource::User),
            CrontabFile::new(CronSource::SystemCrontab),
        ];
        app.selected_source_index = Some(1);

        let fresh = vec![
            CrontabFile::new(CronSource::User),
            CrontabFile::new(CronSource::SystemCrontab),
        ];
        app.apply_load_output(LoadOutput {
            files: fresh,
            errors: Vec::new(),
            auto: false,
        });
        assert_eq!(app.selected_source_index, Some(1));

        // Source disappeared: fall back to the first one.
        app.apply_load_output(LoadOutput {
            files: vec![CrontabFile::new(CronSource::User)],
            errors: Vec::new(),
            auto: false,
        });
        assert_eq!(app.selected_source_index, Some(0));
    }
}
