//! The terminal window.
//!
//! Three dockable views so far: KEDJA (the chain as a graph), BEVIS (the
//! evidence behind the selected relation, with approve and reject) and
//! GRANSKA (the review queue). It reads and writes the same database file
//! as the command line.
//!
//! STATUS: the logic and several frames of layout are tested without a
//! window. Nobody has looked at the real window yet.

mod command;

use command::{Command, View};
use eframe::egui;
use egui_dock::{DockArea, DockState, NodeIndex as DockNode, TabViewer};
use egui_graphs::{DefaultGraphView, Graph, SettingsInteraction, SettingsNavigation, SettingsStyle};
use term_core::{Cik, Company, Direction, ReviewStatus, StoredRelation, Unit};
use term_store::{ChainView, Store};

/// Node payload: the relation the node stands for. The company in the
/// middle has none.
type ChainGraph = Graph<Option<i64>>;

const COLUMN_GAP: f32 = 320.0;
const ROW_GAP: f32 = 46.0;

struct State {
    store: Store,
    command: String,
    message: String,
    cik: Option<Cik>,
    company: Option<Company>,
    show_unreviewed: bool,
    chain: Option<ChainView>,
    graph: ChainGraph,
    /// Planned node positions. Applied after the graph has been shown,
    /// because the graph library scatters nodes on its first frame.
    positions: Vec<(usize, egui::Pos2)>,
    layout_dirty: bool,
    selected: Option<i64>,
    note: String,
    pending: Vec<StoredRelation>,
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

fn node_label(r: &StoredRelation) -> String {
    let mark = match r.status {
        ReviewStatus::Approved => "",
        ReviewStatus::Pending => " (väntar)",
        ReviewStatus::Rejected => " (avvisad)",
    };
    match &r.relation.amount {
        Some(a) if a.unit == Unit::Percent => format!("{} {} %{mark}", r.relation.counterparty.name, a.value),
        _ => format!("{}{mark}", r.relation.counterparty.name),
    }
}

/// Suppliers in a column to the left, customers to the right.
fn column(count: usize, x: f32) -> Vec<egui::Pos2> {
    let top = -(count.saturating_sub(1) as f32) * ROW_GAP / 2.0;
    (0..count).map(|i| egui::pos2(x, top + i as f32 * ROW_GAP)).collect()
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
            graph: ChainGraph::new(),
            positions: Vec::new(),
            layout_dirty: false,
            selected: None,
            note: String::new(),
            pending: Vec::new(),
            focus: None,
        };
        state.reload_pending();
        state
    }

    fn reload_pending(&mut self) {
        match self.store.relations_with_status(ReviewStatus::Pending) {
            Ok(list) => self.pending = list,
            Err(e) => self.message = format!("Kunde inte läsa granskningskön: {e}"),
        }
    }

    fn run_command(&mut self) {
        let input = std::mem::take(&mut self.command);
        match command::parse(&input) {
            Ok(Command::Focus(view)) => self.focus = Some(view),
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
            Ok(view) => {
                self.company = view.company.clone();
                self.build_graph(cik, &view);
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

    fn build_graph(&mut self, cik: Cik, view: &ChainView) {
        // Rejected relations are kept in the store but are not drawn.
        let visible = |r: &&StoredRelation| r.status != ReviewStatus::Rejected;
        let suppliers: Vec<&StoredRelation> = view.suppliers.iter().filter(visible).collect();
        let customers: Vec<&StoredRelation> = view.customers.iter().filter(visible).collect();

        let mut graph = ChainGraph::new();
        let mut positions = Vec::new();
        let centre_label = view.company.as_ref().map(|c| c.name.clone()).unwrap_or_else(|| format!("CIK {cik}"));
        let centre = graph.add_node_with_label(None, centre_label);
        positions.push((centre.index(), egui::pos2(0.0, 0.0)));

        for (r, pos) in suppliers.iter().zip(column(suppliers.len(), -COLUMN_GAP)) {
            let node = graph.add_node_with_label(Some(r.id), node_label(r));
            graph.add_edge(node, centre, ());
            positions.push((node.index(), pos));
        }
        for (r, pos) in customers.iter().zip(column(customers.len(), COLUMN_GAP)) {
            let node = graph.add_node_with_label(Some(r.id), node_label(r));
            graph.add_edge(centre, node, ());
            positions.push((node.index(), pos));
        }
        self.graph = graph;
        self.positions = positions;
        self.apply_positions();
        self.layout_dirty = true;
    }

    fn apply_positions(&mut self) {
        let wanted = self.positions.clone();
        let indices: Vec<_> = self.graph.nodes_iter().map(|(i, _)| i).collect();
        for index in indices {
            if let Some((_, pos)) = wanted.iter().find(|(i, _)| *i == index.index()) {
                if let Some(node) = self.graph.node_mut(index) {
                    node.set_location(*pos);
                }
            }
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
            ui.strong(self.company_name());
            if ui.checkbox(&mut self.show_unreviewed, "Visa även ogranskade").changed() {
                self.reload_chain();
            }
        });
        ui.label("Leverantörer till vänster, kunder till höger. Klicka på en nod för att se belägget.");

        let interaction = SettingsInteraction::default().with_node_selection_enabled(true);
        let navigation = SettingsNavigation::default().with_fit_to_screen_enabled(true);
        let style = SettingsStyle::default().with_labels_always(true);
        DefaultGraphView::new()
            .with_id(Some("kedja".to_string()))
            .with_interactions(&interaction)
            .with_navigations(&navigation)
            .with_styles(&style)
            .show(ui, &mut self.graph);

        if self.layout_dirty {
            self.apply_positions();
            self.layout_dirty = false;
            ui.request_repaint();
        }

        let picked = self
            .graph
            .selected_nodes()
            .first()
            .and_then(|i| self.graph.node(*i))
            .and_then(|n| *n.payload());
        if picked.is_some() && picked != self.selected {
            self.selected = picked;
            self.focus = Some(View::Bevis);
        }
    }

    fn evidence_pane(&mut self, ui: &mut egui::Ui) {
        let Some(r) = self.selected_relation() else {
            ui.label("Ingen relation vald. Klicka på en nod i KEDJA eller på en rad i GRANSKA.");
            return;
        };
        let role = match r.relation.direction {
            Direction::Customer => "kund till",
            Direction::Supplier => "leverantör till",
        };
        ui.heading(&r.relation.counterparty.name);
        ui.label(format!("{role} CIK {}", r.relation.subject));
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
        if let Some(url) = &r.relation.evidence.url {
            ui.hyperlink(url);
        }
        ui.separator();
        ui.label("Citat ur källan:");
        ui.label(egui::RichText::new(format!("\u{201d}{}\u{201d}", r.relation.evidence.quote)).italics());
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
    }

    fn review_pane(&mut self, ui: &mut egui::Ui) {
        if self.pending.is_empty() {
            ui.label("Granskningskön är tom.");
            return;
        }
        ui.label(format!("{} relationer väntar på beslut.", self.pending.len()));
        let mut clicked = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
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
        let mut dock = DockState::new(vec![View::Kedja]);
        dock.main_surface_mut().split_right(DockNode::root(), 0.68, vec![View::Bevis, View::Granska]);
        Self { dock, state: State::new(store) }
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("kommandorad").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(">");
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.state.command)
                        .hint_text("GM KEDJA")
                        .desired_width(260.0),
                );
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if enter || ui.button("Kör").clicked() {
                    self.state.run_command();
                    field.request_focus();
                }
                ui.label(&self.state.message);
            });
        });

        if let Some(view) = self.state.focus.take() {
            if let Some(location) = self.dock.find_tab(&view) {
                // The location was just found, so this cannot miss.
                let _ = self.dock.set_active_tab(location);
            }
        }

        egui::CentralPanel::default().show(ui, |ui| {
            let mut viewer = Viewer { state: &mut self.state };
            DockArea::new(&mut self.dock).show_inside(ui, &mut viewer);
        });
    }
}

impl eframe::App for Terminal {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

fn main() -> eframe::Result {
    let path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("TERM_DB").ok())
        .unwrap_or_else(|| "terminal.db".to_string());
    let store = match Store::open(&path) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("kunde inte öppna {path}: {e}");
            std::process::exit(1);
        }
    };
    eframe::run_native(
        "Marknadsterminalen",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(Terminal::new(store)))),
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
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| terminal.draw(ui));
            assert!(!output.shapes.is_empty(), "the frame drew nothing");
            // No renderer is attached, so the font texture is never uploaded.
            output.textures_delta.clear();
        }
    }

    #[test]
    fn empty_database_draws_without_a_company() {
        let mut terminal = Terminal::new(Store::open_in_memory().unwrap());
        frames(&mut terminal, 3);
        assert!(terminal.state.cik.is_none());
    }

    #[test]
    fn command_opens_a_company_and_builds_the_chain() {
        let mut terminal = Terminal::new(seeded());
        terminal.state.command = "ntc kedja".into();
        terminal.state.run_command();
        assert_eq!(terminal.state.cik, Some(Cik::new(42).unwrap()));
        assert_eq!(terminal.state.graph.node_count(), 4);
        assert_eq!(terminal.state.graph.edge_count(), 3);
        assert!(terminal.state.message.contains("1 leverantörer, 2 kunder"));
        frames(&mut terminal, 4);
    }

    #[test]
    fn nodes_keep_the_planned_layout_after_the_first_frames() {
        let mut terminal = Terminal::new(seeded());
        terminal.state.open("NTC");
        frames(&mut terminal, 4);
        let state = &terminal.state;
        let location = |wanted_payload: Option<i64>| {
            state.graph.nodes_iter().find(|(_, n)| *n.payload() == wanted_payload).map(|(_, n)| n.location()).unwrap()
        };
        let chain = state.chain.as_ref().unwrap();
        assert_eq!(location(None), egui::pos2(0.0, 0.0));
        assert_eq!(location(Some(chain.suppliers[0].id)).x, -COLUMN_GAP);
        for customer in &chain.customers {
            assert_eq!(location(Some(customer.id)).x, COLUMN_GAP);
        }
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
    fn approving_from_the_evidence_pane_updates_store_queue_and_graph() {
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
    fn rejected_relations_leave_the_graph_and_filter_hides_unreviewed() {
        let mut terminal = Terminal::new(seeded());
        terminal.state.open("NTC");
        let ids: Vec<i64> = terminal.state.pending.iter().map(|r| r.id).collect();
        terminal.state.review(ids[0], ReviewStatus::Rejected);
        assert_eq!(terminal.state.graph.node_count(), 3, "the rejected node is gone");

        terminal.state.review(ids[1], ReviewStatus::Approved);
        terminal.state.show_unreviewed = false;
        terminal.state.reload_chain();
        assert_eq!(terminal.state.graph.node_count(), 2, "only the company and the approved relation");
        frames(&mut terminal, 2);
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
