//! The terminal window.
//!
//! Four dockable views so far: KEDJA (suppliers, the company, customers),
//! BEVIS (the evidence behind the selected relation, with approve and
//! reject), GRANSKA (the review queue) and HÄNDELSER (stored events).
//! It reads and writes the same database file as the command line.
//!
//! The chain is drawn by hand as three columns of boxes. The plan names
//! `egui_graphs`, and a first version used it, but on a real screen its
//! automatic zoom made the labels unreadable. A free form graph view for
//! chains several steps deep is still to be built.
//!
//! STATUS: tested without a window, and started on a virtual display to
//! take the picture in `docs/`. Nobody has clicked in it yet.

mod command;

use command::{Command, View};
use eframe::egui;
use egui_dock::{DockArea, DockState, NodeIndex as DockNode, TabViewer};
use term_core::time::unix_to_iso_minutes;
use term_core::{Cik, Company, Direction, Event, ReviewStatus, StoredRelation, Unit};
use term_store::{ChainView, Store};

const BOX_HEIGHT: f32 = 46.0;
const BOX_GAP: f32 = 10.0;
const HEADER_HEIGHT: f32 = 26.0;
const MARGIN: f32 = 12.0;

/// Where the boxes of the chain go.
#[derive(Debug, Clone, PartialEq)]
struct ChainLayout {
    company: egui::Rect,
    suppliers: Vec<egui::Rect>,
    customers: Vec<egui::Rect>,
    /// Total height needed, so that the pane can scroll.
    height: f32,
}

fn column_height(rows: usize) -> f32 {
    rows as f32 * BOX_HEIGHT + rows.saturating_sub(1) as f32 * BOX_GAP
}

/// Three columns inside `width`, starting at `origin`: suppliers to the
/// left, the company in the middle, customers to the right. Each column
/// is centred on the same horizontal line.
fn chain_layout(origin: egui::Pos2, width: f32, suppliers: usize, customers: usize) -> ChainLayout {
    let lane = (width - 2.0 * MARGIN) / 3.0;
    let box_width = (lane - 28.0).clamp(90.0, 280.0);
    let body = column_height(suppliers.max(customers).max(1));
    let top = origin.y + HEADER_HEIGHT;
    let centre_y = top + body / 2.0;

    let column = |lane_index: f32, rows: usize| -> Vec<egui::Rect> {
        let x = origin.x + MARGIN + lane * lane_index + (lane - box_width) / 2.0;
        let start = centre_y - column_height(rows) / 2.0;
        (0..rows)
            .map(|i| {
                egui::Rect::from_min_size(
                    egui::pos2(x, start + i as f32 * (BOX_HEIGHT + BOX_GAP)),
                    egui::vec2(box_width, BOX_HEIGHT),
                )
            })
            .collect()
    };

    ChainLayout {
        company: column(1.0, 1)[0],
        suppliers: column(0.0, suppliers),
        customers: column(2.0, customers),
        height: HEADER_HEIGHT + body + MARGIN,
    }
}

struct State {
    store: Store,
    command: String,
    message: String,
    cik: Option<Cik>,
    company: Option<Company>,
    show_unreviewed: bool,
    chain: Option<ChainView>,
    selected: Option<i64>,
    note: String,
    pending: Vec<StoredRelation>,
    events: Vec<Event>,
    focus: Option<View>,
}

fn status_text(status: ReviewStatus) -> &'static str {
    match status {
        ReviewStatus::Pending => "väntar på granskning",
        ReviewStatus::Approved => "godkänd",
        ReviewStatus::Rejected => "avvisad",
    }
}

fn amount_text(r: &StoredRelation) -> String {
    match &r.relation.amount {
        Some(a) => {
            let unit = if a.unit == Unit::Percent { "%" } else { "USD" };
            match &a.basis {
                Some(basis) => format!("{} {unit} av {basis}", a.value),
                None => format!("{} {unit}", a.value),
            }
        }
        None => "inget belopp angivet".to_string(),
    }
}

/// Second line of a box: share, level and review status.
fn box_detail(r: &StoredRelation) -> String {
    let share = match &r.relation.amount {
        Some(a) if a.unit == Unit::Percent => format!("{} %", a.value),
        Some(a) => format!("{} USD", a.value),
        None => "utan belopp".to_string(),
    };
    let status = match r.status {
        ReviewStatus::Approved => "godkänd",
        ReviewStatus::Pending => "väntar",
        ReviewStatus::Rejected => "avvisad",
    };
    format!("{share}  |  {:?}  |  {status}", r.relation.confidence)
}

impl State {
    fn new(store: Store) -> Self {
        let mut state = Self {
            store,
            command: String::new(),
            message: "Skriv ett bolag och en vy, till exempel: GM KEDJA".to_string(),
            cik: None,
            company: None,
            show_unreviewed: true,
            chain: None,
            selected: None,
            note: String::new(),
            pending: Vec::new(),
            events: Vec::new(),
            focus: None,
        };
        state.reload_pending();
        state.reload_events();
        state
    }

    fn reload_pending(&mut self) {
        match self.store.relations_with_status(ReviewStatus::Pending) {
            Ok(list) => self.pending = list,
            Err(e) => self.message = format!("Kunde inte läsa granskningskön: {e}"),
        }
    }

    fn reload_events(&mut self) {
        match self.store.events(500) {
            Ok(list) => self.events = list,
            Err(e) => self.message = format!("Kunde inte läsa händelser: {e}"),
        }
    }

    fn run_command(&mut self) {
        let input = std::mem::take(&mut self.command);
        match command::parse(&input) {
            Ok(Command::Focus(view)) => {
                if view == View::Handelser {
                    self.reload_events();
                }
                self.focus = Some(view);
            }
            Ok(Command::Open { target, view }) => {
                self.open(&target);
                self.focus = Some(view);
            }
            Err(message) => self.message = message,
        }
    }

    fn open(&mut self, target: &str) {
        let found = match target.parse::<Cik>() {
            Ok(cik) => Ok(Some(cik)),
            Err(_) => self.store.company_by_ticker(target).map(|c| c.map(|c| c.cik)),
        };
        match found {
            Ok(Some(cik)) => {
                self.cik = Some(cik);
                self.selected = None;
                self.reload_chain();
            }
            Ok(None) => self.message = format!("Hittar inget bolag med ticker {target}."),
            Err(e) => self.message = format!("Databasfel: {e}"),
        }
    }

    fn reload_chain(&mut self) {
        let Some(cik) = self.cik else { return };
        let filter = if self.show_unreviewed { None } else { Some(ReviewStatus::Approved) };
        match self.store.chain(cik, filter) {
            Ok(mut view) => {
                // Rejected relations stay in the store but are not drawn.
                view.suppliers.retain(|r| r.status != ReviewStatus::Rejected);
                view.customers.retain(|r| r.status != ReviewStatus::Rejected);
                self.company = view.company.clone();
                self.message = format!(
                    "{}: {} leverantörer, {} kunder",
                    self.company_name(),
                    view.suppliers.len(),
                    view.customers.len()
                );
                self.chain = Some(view);
            }
            Err(e) => self.message = format!("Databasfel: {e}"),
        }
    }

    fn company_name(&self) -> String {
        match (&self.company, self.cik) {
            (Some(c), _) => c.name.clone(),
            (None, Some(cik)) => format!("CIK {cik}"),
            (None, None) => "inget bolag".to_string(),
        }
    }

    fn selected_relation(&self) -> Option<StoredRelation> {
        self.store.relation(self.selected?).ok().flatten()
    }

    fn review(&mut self, id: i64, status: ReviewStatus) {
        let note = self.note.trim();
        let note = if note.is_empty() { None } else { Some(note) };
        match self.store.set_review(id, status, note) {
            Ok(r) => {
                self.message = format!("{} är nu {}.", r.relation.counterparty.name, status_text(r.status));
                self.note.clear();
            }
            Err(e) => self.message = format!("Kunde inte spara beslutet: {e}"),
        }
        self.reload_pending();
        self.reload_chain();
    }

    // ----- panes -----

    fn chain_pane(&mut self, ui: &mut egui::Ui) {
        if self.cik.is_none() {
            ui.label("Inget bolag valt. Skriv till exempel GM KEDJA i kommandoraden.");
            return;
        }
        ui.horizontal(|ui| {
            ui.heading(self.company_name());
            if ui.checkbox(&mut self.show_unreviewed, "Visa även ogranskade").changed() {
                self.reload_chain();
            }
        });
        ui.label("Klicka på en ruta för att se belägget.");
        ui.add_space(6.0);

        let Some(chain) = self.chain.clone() else { return };
        let mut clicked = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let width = ui.available_width();
            let probe = chain_layout(egui::Pos2::ZERO, width, chain.suppliers.len(), chain.customers.len());
            let (area, _) = ui.allocate_exact_size(egui::vec2(width, probe.height), egui::Sense::hover());
            let layout = chain_layout(area.min, width, chain.suppliers.len(), chain.customers.len());
            let painter = ui.painter_at(area);
            let visuals = ui.visuals().clone();
            let line = egui::Stroke::new(1.5, visuals.weak_text_color());
            let small = egui::FontId::proportional(12.0);
            let normal = egui::FontId::proportional(14.0);

            for (title, rects) in [("LEVERANTÖRER", &layout.suppliers), ("KUNDER", &layout.customers)] {
                if let Some(first) = rects.first() {
                    painter.text(
                        egui::pos2(first.left(), area.top() + 4.0),
                        egui::Align2::LEFT_TOP,
                        title,
                        small.clone(),
                        visuals.weak_text_color(),
                    );
                }
            }

            for rect in &layout.suppliers {
                painter.line_segment([rect.right_center(), layout.company.left_center()], line);
            }
            for rect in &layout.customers {
                painter.line_segment([layout.company.right_center(), rect.left_center()], line);
            }

            painter.rect_filled(layout.company, 6.0, visuals.selection.bg_fill.gamma_multiply(0.5));
            painter.rect_stroke(layout.company, 6.0, visuals.selection.stroke, egui::StrokeKind::Inside);
            let inner = painter.with_clip_rect(layout.company.shrink(6.0));
            inner.text(
                layout.company.left_center() + egui::vec2(10.0, 0.0),
                egui::Align2::LEFT_CENTER,
                self.company_name(),
                normal.clone(),
                visuals.strong_text_color(),
            );

            let all = chain.suppliers.iter().zip(&layout.suppliers).chain(chain.customers.iter().zip(&layout.customers));
            for (relation, rect) in all {
                let response = ui.interact(*rect, ui.id().with(("relation", relation.id)), egui::Sense::click());
                let selected = self.selected == Some(relation.id);
                let fill = if response.hovered() { visuals.widgets.hovered.bg_fill } else { visuals.extreme_bg_color };
                let stroke = if selected {
                    egui::Stroke::new(2.0, visuals.selection.stroke.color)
                } else if relation.status == ReviewStatus::Pending {
                    egui::Stroke::new(1.0, visuals.warn_fg_color)
                } else {
                    visuals.widgets.noninteractive.bg_stroke
                };
                painter.rect_filled(*rect, 6.0, fill);
                painter.rect_stroke(*rect, 6.0, stroke, egui::StrokeKind::Inside);
                let inner = painter.with_clip_rect(rect.shrink(6.0));
                inner.text(
                    rect.left_top() + egui::vec2(10.0, 7.0),
                    egui::Align2::LEFT_TOP,
                    &relation.relation.counterparty.name,
                    normal.clone(),
                    visuals.strong_text_color(),
                );
                inner.text(
                    rect.left_bottom() + egui::vec2(10.0, -7.0),
                    egui::Align2::LEFT_BOTTOM,
                    box_detail(relation),
                    small.clone(),
                    visuals.text_color(),
                );
                if response.clicked() {
                    clicked = Some(relation.id);
                }
            }
        });
        if let Some(id) = clicked {
            self.selected = Some(id);
            self.focus = Some(View::Bevis);
        }
    }

    fn evidence_pane(&mut self, ui: &mut egui::Ui) {
        let Some(r) = self.selected_relation() else {
            ui.label("Ingen relation vald. Klicka på en ruta i KEDJA eller på en rad i GRANSKA.");
            return;
        };
        let role = match r.relation.direction {
            Direction::Customer => "kund till",
            Direction::Supplier => "leverantör till",
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.heading(&r.relation.counterparty.name);
            ui.label(format!("{role} CIK {}", r.relation.subject));
            ui.add_space(4.0);
            ui.label("Citat ur källan:");
            ui.label(
                egui::RichText::new(format!("\u{201d}{}\u{201d}", r.relation.evidence.quote))
                    .italics()
                    .strong(),
            );
            if let Some(url) = &r.relation.evidence.url {
                ui.hyperlink(url);
            }
            ui.separator();
            egui::Grid::new("bevis").num_columns(2).show(ui, |ui| {
                ui.label("Belopp");
                ui.label(amount_text(&r));
                ui.end_row();
                ui.label("Nivå");
                ui.label(format!("{:?}", r.relation.confidence));
                ui.end_row();
                ui.label("Bekräftad av båda");
                ui.label(if r.relation.corroborated { "ja" } else { "nej" });
                ui.end_row();
                ui.label("Matchat bolag");
                ui.label(match r.relation.counterparty.cik {
                    Some(cik) => format!("CIK {cik}"),
                    None => "inte matchat".to_string(),
                });
                ui.end_row();
                ui.label("Räkenskapsår");
                ui.label(r.relation.period.map(|p| p.fiscal_year.to_string()).unwrap_or_else(|| "okänt".into()));
                ui.end_row();
                ui.label("Dokument");
                ui.label(r.relation.evidence.accession.clone().unwrap_or_else(|| "okänt".into()));
                ui.end_row();
                ui.label("Status");
                ui.label(status_text(r.status));
                ui.end_row();
            });
            if let Some(note) = &r.note {
                ui.label(format!("Not: {note}"));
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Not:");
                ui.text_edit_singleline(&mut self.note);
            });
            ui.horizontal(|ui| {
                if ui.button("Godkänn").clicked() {
                    self.review(r.id, ReviewStatus::Approved);
                }
                if ui.button("Avvisa").clicked() {
                    self.review(r.id, ReviewStatus::Rejected);
                }
            });
        });
    }

    fn review_pane(&mut self, ui: &mut egui::Ui) {
        if self.pending.is_empty() {
            ui.label("Granskningskön är tom.");
            return;
        }
        ui.label(format!("{} relationer väntar på beslut.", self.pending.len()));
        let mut clicked = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for r in &self.pending {
                let role = match r.relation.direction {
                    Direction::Customer => "kund till",
                    Direction::Supplier => "leverantör till",
                };
                let text = format!("#{}  {}  {role} CIK {}", r.id, r.relation.counterparty.name, r.relation.subject);
                if ui.selectable_label(self.selected == Some(r.id), text).clicked() {
                    clicked = Some(r.id);
                }
            }
        });
        if let Some(id) = clicked {
            self.selected = Some(id);
            self.focus = Some(View::Bevis);
        }
    }

    fn events_pane(&mut self, ui: &mut egui::Ui) {
        if self.events.is_empty() {
            ui.label("Inga händelser sparade. Hämta med kommandot: term events fetch");
            return;
        }
        ui.label(format!(
            "{} händelser, nyast först. De är ännu inte kopplade till anläggningar i kedjorna.",
            self.events.len()
        ));
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("handelser").num_columns(4).striped(true).show(ui, |ui| {
                for e in &self.events {
                    ui.label(unix_to_iso_minutes(e.time));
                    ui.label(&e.kind);
                    ui.label(e.alert.as_deref().unwrap_or(""));
                    match &e.url {
                        Some(url) => ui.hyperlink_to(&e.title, url),
                        None => ui.label(&e.title),
                    };
                    ui.end_row();
                }
            });
        });
    }
}

struct Viewer<'a> {
    state: &'a mut State,
}

impl TabViewer for Viewer<'_> {
    type Tab = View;

    fn id(&mut self, tab: &mut View) -> egui::Id {
        egui::Id::new(*tab)
    }

    fn title(&mut self, tab: &mut View) -> egui::WidgetText {
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut View) {
        match tab {
            View::Kedja => self.state.chain_pane(ui),
            View::Bevis => self.state.evidence_pane(ui),
            View::Granska => self.state.review_pane(ui),
            View::Handelser => self.state.events_pane(ui),
        }
    }

    fn is_closeable(&self, _tab: &View) -> bool {
        false
    }
}

struct Terminal {
    dock: DockState<View>,
    state: State,
}

impl Terminal {
    fn new(store: Store) -> Self {
        // The chain to the left. Evidence top right, events and queue below it.
        let mut dock = DockState::new(vec![View::Kedja]);
        let [_, right] = dock.main_surface_mut().split_right(DockNode::root(), 0.58, vec![View::Bevis]);
        dock.main_surface_mut().split_below(right, 0.5, vec![View::Handelser, View::Granska]);
        Self { dock, state: State::new(store) }
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("kommandorad").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(">");
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.state.command)
                        .hint_text("bolag och vy, till exempel GM KEDJA")
                        .desired_width(280.0),
                );
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if enter || ui.button("Kör").clicked() {
                    self.state.run_command();
                    field.request_focus();
                }
                ui.label(&self.state.message);
            });
            ui.add_space(4.0);
        });

        if let Some(view) = self.state.focus.take() {
            if let Some(location) = self.dock.find_tab(&view) {
                // The location was just found, so this cannot miss.
                let _ = self.dock.set_active_tab(location);
            }
        }

        egui::CentralPanel::default().show(ui, |ui| {
            let mut viewer = Viewer { state: &mut self.state };
            DockArea::new(&mut self.dock)
                .show_close_buttons(false)
                .show_leaf_close_all_buttons(false)
                .show_leaf_collapse_buttons(false)
                .show_inside(ui, &mut viewer);
        });
    }
}

impl eframe::App for Terminal {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

fn main() -> eframe::Result {
    // term-ui [databasfil] [--run "NTC KEDJA"] [--select <relationens nummer>]
    let mut path = None;
    let mut startup_command = None;
    let mut startup_selection = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--run" => startup_command = args.next(),
            "--select" => startup_selection = args.next().and_then(|v| v.parse::<i64>().ok()),
            _ => path = Some(arg),
        }
    }
    let path = path.or_else(|| std::env::var("TERM_DB").ok()).unwrap_or_else(|| "terminal.db".to_string());
    let store = match Store::open(&path) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("kunde inte öppna {path}: {e}");
            std::process::exit(1);
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Marknadsterminalen")
            .with_inner_size([1360.0, 820.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Marknadsterminalen",
        options,
        Box::new(move |_cc| {
            let mut terminal = Terminal::new(store);
            if let Some(command) = startup_command {
                terminal.state.command = command;
                terminal.state.run_command();
            }
            if startup_selection.is_some() {
                terminal.state.selected = startup_selection;
            }
            Ok(Box::new(terminal))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_core::{Amount, Confidence, Counterparty, Evidence, Relation};

    fn seeded() -> Store {
        let store = Store::open_in_memory().unwrap();
        let cik = Cik::new(42).unwrap();
        store
            .upsert_company(&Company { cik, name: "Nordvik Test Components Inc.".into(), ticker: Some("NTC".into()) })
            .unwrap();
        let rel = |name: &str, direction, pct: Option<f64>| Relation {
            subject: cik,
            counterparty: Counterparty { name: name.into(), cik: None },
            direction,
            amount: pct.map(|value| Amount { value, unit: Unit::Percent, basis: Some("net sales".into()) }),
            period: None,
            evidence: Evidence { accession: None, url: None, quote: format!("{name} in a made up sentence") },
            confidence: Confidence::Named,
            corroborated: false,
        };
        store.upsert_relation(&rel("Haldenfjord Motors Corp", Direction::Customer, Some(21.0))).unwrap();
        store.upsert_relation(&rel("Brightwater Retail Group", Direction::Customer, Some(12.5))).unwrap();
        store.upsert_relation(&rel("Polarvik Castings Ltd", Direction::Supplier, None)).unwrap();
        store
    }

    fn frames(terminal: &mut Terminal, n: usize) {
        let ctx = egui::Context::default();
        for _ in 0..n {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1360.0, 820.0))),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| terminal.draw(ui));
            assert!(!output.shapes.is_empty(), "the frame drew nothing");
            // No renderer is attached, so the font texture is never uploaded.
            output.textures_delta.clear();
        }
    }

    #[test]
    fn layout_puts_suppliers_left_and_customers_right() {
        for (width, suppliers, customers) in [(780.0, 3, 5), (420.0, 1, 1), (1600.0, 0, 12), (780.0, 0, 0)] {
            let l = chain_layout(egui::pos2(10.0, 20.0), width, suppliers, customers);
            assert_eq!((l.suppliers.len(), l.customers.len()), (suppliers, customers));
            for s in &l.suppliers {
                assert!(s.right() < l.company.left(), "supplier overlaps the company at width {width}");
            }
            for c in &l.customers {
                assert!(c.left() > l.company.right(), "customer overlaps the company at width {width}");
            }
            let all: Vec<&egui::Rect> = l.suppliers.iter().chain(&l.customers).chain([&l.company]).collect();
            for r in &all {
                assert!(r.left() >= 10.0 && r.right() <= 10.0 + width, "box outside the pane at width {width}");
                assert!(r.top() >= 20.0 && r.bottom() <= 20.0 + l.height, "box outside the height");
            }
            for column in [&l.suppliers, &l.customers] {
                for pair in column.windows(2) {
                    assert!(pair[0].bottom() < pair[1].top(), "boxes in a column overlap");
                }
            }
        }
    }

    #[test]
    fn columns_share_one_centre_line() {
        let l = chain_layout(egui::Pos2::ZERO, 900.0, 2, 6);
        let mid = |rects: &[egui::Rect]| (rects.first().unwrap().top() + rects.last().unwrap().bottom()) / 2.0;
        assert!((mid(&l.suppliers) - l.company.center().y).abs() < 0.01);
        assert!((mid(&l.customers) - l.company.center().y).abs() < 0.01);
    }

    #[test]
    fn empty_database_draws_without_a_company() {
        let mut terminal = Terminal::new(Store::open_in_memory().unwrap());
        frames(&mut terminal, 3);
        assert!(terminal.state.cik.is_none());
    }

    #[test]
    fn command_opens_a_company_and_loads_the_chain() {
        let mut terminal = Terminal::new(seeded());
        terminal.state.command = "ntc kedja".into();
        terminal.state.run_command();
        assert_eq!(terminal.state.cik, Some(Cik::new(42).unwrap()));
        let chain = terminal.state.chain.as_ref().unwrap();
        assert_eq!((chain.suppliers.len(), chain.customers.len()), (1, 2));
        assert_eq!(chain.customers[0].relation.counterparty.name, "Haldenfjord Motors Corp", "largest share first");
        assert!(terminal.state.message.contains("1 leverantörer, 2 kunder"));
        frames(&mut terminal, 4);
    }

    #[test]
    fn unknown_ticker_and_bad_command_leave_a_message() {
        let mut terminal = Terminal::new(seeded());
        terminal.state.command = "ZZZ".into();
        terminal.state.run_command();
        assert!(terminal.state.message.contains("ZZZ"));
        assert!(terminal.state.cik.is_none());
        terminal.state.command = "NTC KURS".into();
        terminal.state.run_command();
        assert!(terminal.state.message.contains("Okänd vy"));
    }

    #[test]
    fn approving_from_the_evidence_pane_updates_store_and_queue() {
        let mut terminal = Terminal::new(seeded());
        terminal.state.open("NTC");
        assert_eq!(terminal.state.pending.len(), 3);
        let id = terminal.state.pending[0].id;
        terminal.state.selected = Some(id);
        frames(&mut terminal, 2);

        terminal.state.note = "kontrollerad".into();
        terminal.state.review(id, ReviewStatus::Approved);
        let stored = terminal.state.store.relation(id).unwrap().unwrap();
        assert_eq!(stored.status, ReviewStatus::Approved);
        assert_eq!(stored.note.as_deref(), Some("kontrollerad"));
        assert_eq!(terminal.state.pending.len(), 2);
        assert!(terminal.state.note.is_empty());
        frames(&mut terminal, 2);
    }

    #[test]
    fn rejected_relations_leave_the_chain_and_filter_hides_unreviewed() {
        let mut terminal = Terminal::new(seeded());
        terminal.state.open("NTC");
        let ids: Vec<i64> = terminal.state.pending.iter().map(|r| r.id).collect();
        let count = |t: &Terminal| {
            let c = t.state.chain.as_ref().unwrap();
            c.suppliers.len() + c.customers.len()
        };
        terminal.state.review(ids[0], ReviewStatus::Rejected);
        assert_eq!(count(&terminal), 2, "the rejected relation is gone");

        terminal.state.review(ids[1], ReviewStatus::Approved);
        terminal.state.show_unreviewed = false;
        terminal.state.reload_chain();
        assert_eq!(count(&terminal), 1, "only the approved relation is left");
        frames(&mut terminal, 2);
    }

    #[test]
    fn events_view_lists_stored_events() {
        let store = seeded();
        store
            .upsert_events(&[Event {
                source: "usgs".into(),
                external_id: "x1".into(),
                kind: "earthquake".into(),
                title: "M 6.1 made up place".into(),
                time: 1_795_446_657,
                lat: Some(1.0),
                lon: Some(2.0),
                severity: Some(6.1),
                severity_unit: Some("magnitude".into()),
                alert: None,
                country: None,
                url: Some("https://example.org/x1".into()),
            }])
            .unwrap();
        let mut terminal = Terminal::new(store);
        assert_eq!(terminal.state.events.len(), 1);
        terminal.state.command = "händelser".into();
        terminal.state.run_command();
        assert_eq!(terminal.state.focus, Some(View::Handelser));
        frames(&mut terminal, 3);
    }

    #[test]
    fn focus_request_brings_the_view_forward() {
        let mut terminal = Terminal::new(seeded());
        terminal.state.command = "granska".into();
        terminal.state.run_command();
        assert_eq!(terminal.state.focus, Some(View::Granska));
        frames(&mut terminal, 1);
        assert_eq!(terminal.state.focus, None);
    }
}
