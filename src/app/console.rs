use std::cell::{Cell, OnceCell, RefCell};

use gtk4::gdk;
use gtk4::glib;
use gtk4::glib::ParamSpec;
use gtk4::glib::ParamSpecBoolean;
use gtk4::glib::Value;
use gtk4::graphene;
use gtk4::pango;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use vte::{Params, Parser, Perform};

struct ConsoleTailUpdate {
    dirty_from_row: usize,
    logical_rows: Vec<Vec<ScreenCell>>,
}

fn cluster_width(text: &str) -> usize {
    UnicodeWidthStr::width(text).max(1)
}

mod surface_imp {
    use super::*;

    #[derive(Clone, Copy)]
    struct GridMetrics {
        cell_width: i32,
        cell_height: i32,
        baseline: i32,
    }

    #[derive(Default)]
    pub struct ConsoleSurface {
        pub(super) logical_rows: RefCell<Vec<Vec<ScreenCell>>>,
        pub(super) monospace: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ConsoleSurface {
        const NAME: &'static str = "ModelAssistantConsoleSurface";
        type Type = super::ConsoleSurface;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for ConsoleSurface {}

    impl WidgetImpl for ConsoleSurface {
        fn request_mode(&self) -> gtk4::SizeRequestMode {
            gtk4::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk4::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let obj = self.obj();
            let metrics = self.grid_metrics(&obj);
            let logical_rows = self.logical_rows.borrow();
            let max_columns = self.max_columns(&logical_rows) as i32;
            let width = max_columns.saturating_mul(metrics.cell_width.max(1));
            let wrap_columns = self.wrap_columns(for_size, metrics, max_columns as usize);
            let visual_rows = self.visual_row_count(&logical_rows, wrap_columns) as i32;
            let height = visual_rows.saturating_mul(metrics.cell_height.max(1));
            match orientation {
                gtk4::Orientation::Horizontal => {
                    let min_width = metrics.cell_width.max(1);
                    let natural_width = width.max(min_width);
                    (min_width, natural_width, -1, -1)
                }
                gtk4::Orientation::Vertical => (height, height, -1, -1),
                _ => (0, 0, -1, -1),
            }
        }

        fn snapshot(&self, snapshot: &gtk4::Snapshot) {
            let obj = self.obj();
            let metrics = self.grid_metrics(&obj);
            let color = obj.color();
            let logical_rows = self.logical_rows.borrow();
            let wrap_columns = self.wrap_columns(obj.width(), metrics, self.max_columns(&logical_rows));

            self.for_each_visual_line(&logical_rows, wrap_columns, |row, start, end, visual_row| {
                let y = visual_row as f32 * metrics.cell_height as f32;
                for (col_index, cell) in row[start..end].iter().enumerate() {
                    let ScreenCell::Cluster(text) = cell else {
                        continue;
                    };
                    let x = col_index as f32 * metrics.cell_width as f32;
                    self.draw_cluster(snapshot, &obj, &color, text, x, y, metrics);
                }
            });
        }
    }

    impl ConsoleSurface {
        fn max_columns(&self, logical_rows: &[Vec<ScreenCell>]) -> usize {
            logical_rows.iter().map(Vec::len).max().unwrap_or(0)
        }

        fn visual_row_count(&self, logical_rows: &[Vec<ScreenCell>], wrap_columns: usize) -> usize {
            let mut count = 0usize;
            self.for_each_visual_line(logical_rows, wrap_columns, |_row, _start, _end, _visual_row| {
                count += 1;
            });
            count
        }

        fn for_each_visual_line(
            &self,
            logical_rows: &[Vec<ScreenCell>],
            wrap_columns: usize,
            mut visit: impl FnMut(&[ScreenCell], usize, usize, usize),
        ) {
            let mut visual_row = 0usize;
            for row in logical_rows {
                let mut start = 0usize;
                loop {
                    let end = self.visual_break(row, start, wrap_columns);
                    visit(row, start, end, visual_row);
                    visual_row += 1;
                    if end >= row.len() {
                        break;
                    }
                    start = end;
                }
            }
        }

        fn font_description(&self, widget: &super::ConsoleSurface) -> Option<pango::FontDescription> {
            let mut font_desc = widget.pango_context().font_description()?;
            if self.monospace.get() {
                font_desc.set_family("monospace");
            }
            Some(font_desc)
        }

        fn create_layout(&self, widget: &super::ConsoleSurface, text: &str) -> pango::Layout {
            let context = widget.pango_context();
            let layout = pango::Layout::new(&context);
            if let Some(desc) = self.font_description(widget) {
                layout.set_font_description(Some(&desc));
            }
            layout.set_text(text);
            layout
        }

        fn grid_metrics(&self, widget: &super::ConsoleSurface) -> GridMetrics {
            let context = widget.pango_context();
            let font_desc = self.font_description(widget);

            let digit_layout = self.create_layout(widget, "0");
            let (cell_width, _) = digit_layout.pixel_size();

            let metrics = context.metrics(font_desc.as_ref(), None);
            let ascent = ((metrics.ascent() + pango::SCALE / 2) / pango::SCALE).max(1);
            let descent = ((metrics.descent() + pango::SCALE / 2) / pango::SCALE).max(0);
            let cell_height = (ascent + descent).max(1);

            GridMetrics {
                cell_width: cell_width.max(1).saturating_add(1),
                cell_height,
                baseline: ascent,
            }
        }

        fn wrap_columns(&self, available_width: i32, metrics: GridMetrics, max_columns: usize) -> usize {
            if available_width <= 0 {
                return max_columns.max(1);
            }

            let columns = available_width
                .checked_div(metrics.cell_width.max(1))
                .unwrap_or(0)
                .max(1);
            columns as usize
        }

        fn visual_break(&self, row: &[ScreenCell], start: usize, wrap_columns: usize) -> usize {
            if row.is_empty() {
                return 0;
            }
            if start >= row.len() {
                return row.len();
            }

            let hard_end = (start + wrap_columns.max(1)).min(row.len());
            if hard_end == row.len() || !matches!(row[hard_end], ScreenCell::Continuation) {
                return hard_end;
            }

            let mut end = hard_end;
            while end > start && matches!(row[end], ScreenCell::Continuation) {
                end -= 1;
            }

            if end == start { hard_end } else { end }
        }

        fn draw_cluster(
            &self,
            snapshot: &gtk4::Snapshot,
            widget: &super::ConsoleSurface,
            color: &gdk::RGBA,
            text: &str,
            x: f32,
            y: f32,
            metrics: GridMetrics,
        ) {
            let span = cluster_width(text) as i32;

            let layout = self.create_layout(widget, text);
            layout.set_width(span.max(1) * metrics.cell_width * pango::SCALE);
            let layout_baseline = ((layout.baseline() + pango::SCALE / 2) / pango::SCALE).max(0);

            snapshot.save();
            snapshot.translate(&graphene::Point::new(x, y));
            snapshot.push_clip(&graphene::Rect::new(
                0.0,
                0.0,
                (span.max(1) * metrics.cell_width) as f32,
                metrics.cell_height as f32,
            ));
            snapshot.translate(&graphene::Point::new(0.0, (metrics.baseline - layout_baseline) as f32));
            snapshot.append_layout(&layout, color);
            snapshot.pop();
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    pub struct ConsoleSurface(ObjectSubclass<surface_imp::ConsoleSurface>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl ConsoleSurface {
    fn new() -> Self {
        glib::Object::builder::<ConsoleSurface>()
            .property("hexpand", true)
            .property("vexpand", true)
            .build()
    }

    fn clear(&self) {
        self.imp().logical_rows.borrow_mut().clear();
        self.queue_resize();
        self.queue_draw();
    }

    fn replace_tail(&self, dirty_from_row: usize, logical_rows: Vec<Vec<ScreenCell>>) {
        let mut current = self.imp().logical_rows.borrow_mut();
        current.truncate(dirty_from_row);
        current.extend(logical_rows);
        drop(current);
        self.queue_resize();
        self.queue_draw();
    }

    fn set_monospace(&self, monospace: bool) {
        self.imp().monospace.set(monospace);
        self.queue_resize();
        self.queue_draw();
    }

}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ConsoleView {
        pub surface: OnceCell<ConsoleSurface>,
        pub console: RefCell<VirtualConsole>,
        pub monospace: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ConsoleView {
        const NAME: &'static str = "ModelAssistantConsoleView";
        type Type = super::ConsoleView;
        type ParentType = gtk4::Box;
    }

    impl ObjectImpl for ConsoleView {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: std::sync::OnceLock<Vec<ParamSpec>> = std::sync::OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![ParamSpecBoolean::builder("monospace").default_value(false).build()]
            })
        }

        fn set_property(&self, _id: usize, value: &Value, pspec: &ParamSpec) {
            match pspec.name() {
                "monospace" => {
                    let monospace = value.get().expect("monospace should be a bool");
                    self.monospace.set(monospace);
                    if let Some(surface) = self.surface.get() {
                        surface.set_monospace(monospace);
                    }
                }
                _ => unimplemented!(),
            }
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> Value {
            match pspec.name() {
                "monospace" => self.monospace.get().to_value(),
                _ => unimplemented!(),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            let surface = ConsoleSurface::new();
            surface.set_monospace(self.monospace.get());
            surface.set_margin_top(10);
            surface.set_margin_bottom(10);
            surface.set_margin_start(8);
            surface.set_margin_end(8);
            let scroll = gtk4::ScrolledWindow::builder()
                .hexpand(true)
                .vexpand(true)
                .hscrollbar_policy(gtk4::PolicyType::Never)
                .child(&surface)
                .build();

            let obj = self.obj();
            obj.append(&scroll);

            self.surface
                .set(surface)
                .expect("console surface should only be initialized once");
        }
    }

    impl WidgetImpl for ConsoleView {}
    impl BoxImpl for ConsoleView {}
}

glib::wrapper! {
    pub struct ConsoleView(ObjectSubclass<imp::ConsoleView>)
        @extends gtk4::Widget, gtk4::Box,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}


#[derive(Default)]
pub struct ConsoleViewBuilder {
    orientation: Option<gtk4::Orientation>,
    hexpand: Option<bool>,
    vexpand: Option<bool>,
    focusable: Option<bool>,
    margin_top: Option<i32>,
    margin_bottom: Option<i32>,
    margin_start: Option<i32>,
    margin_end: Option<i32>,
    monospace: Option<bool>,
}

impl ConsoleViewBuilder {
    pub fn orientation(mut self, orientation: gtk4::Orientation) -> Self {
        self.orientation = Some(orientation);
        self
    }

    pub fn hexpand(mut self, hexpand: bool) -> Self {
        self.hexpand = Some(hexpand);
        self
    }

    pub fn vexpand(mut self, vexpand: bool) -> Self {
        self.vexpand = Some(vexpand);
        self
    }

    pub fn focusable(mut self, focusable: bool) -> Self {
        self.focusable = Some(focusable);
        self
    }

    pub fn margin_top(mut self, margin_top: i32) -> Self {
        self.margin_top = Some(margin_top);
        self
    }

    pub fn margin_bottom(mut self, margin_bottom: i32) -> Self {
        self.margin_bottom = Some(margin_bottom);
        self
    }

    pub fn margin_start(mut self, margin_start: i32) -> Self {
        self.margin_start = Some(margin_start);
        self
    }

    pub fn margin_end(mut self, margin_end: i32) -> Self {
        self.margin_end = Some(margin_end);
        self
    }

    pub fn monospace(mut self, monospace: bool) -> Self {
        self.monospace = Some(monospace);
        self
    }

    pub fn build(self) -> ConsoleView {
        let mut builder = glib::Object::builder::<ConsoleView>();

        if let Some(orientation) = self.orientation {
            builder = builder.property("orientation", orientation);
        }
        if let Some(hexpand) = self.hexpand {
            builder = builder.property("hexpand", hexpand);
        }
        if let Some(vexpand) = self.vexpand {
            builder = builder.property("vexpand", vexpand);
        }
        if let Some(focusable) = self.focusable {
            builder = builder.property("focusable", focusable);
        }
        if let Some(margin_top) = self.margin_top {
            builder = builder.property("margin-top", margin_top);
        }
        if let Some(margin_bottom) = self.margin_bottom {
            builder = builder.property("margin-bottom", margin_bottom);
        }
        if let Some(margin_start) = self.margin_start {
            builder = builder.property("margin-start", margin_start);
        }
        if let Some(margin_end) = self.margin_end {
            builder = builder.property("margin-end", margin_end);
        }
        if let Some(monospace) = self.monospace {
            builder = builder.property("monospace", monospace);
        }

        builder.build()
    }
}

impl ConsoleView {
    pub fn builder() -> ConsoleViewBuilder {
        ConsoleViewBuilder::default()
    }

    fn surface(&self) -> &ConsoleSurface {
        self.imp()
            .surface
            .get()
            .expect("console surface should be constructed")
    }

    pub fn clear(&self) {
        self.imp().console.borrow_mut().clear();
        self.surface().clear();
    }

    pub fn append_output(&self, data: &[u8]) {
        let mut console = self.imp().console.borrow_mut();
        let Some(update) = console.push_output(data) else {
            return;
        };
        drop(console);

        self.apply_tail_update(update);
    }

    fn apply_tail_update(&self, update: ConsoleTailUpdate) {
        self.surface().replace_tail(update.dirty_from_row, update.logical_rows);
    }
}

#[derive(Default)]
pub struct VirtualConsole {
    parser: Parser,
    screen: ScreenBuffer,
}

impl VirtualConsole {
    pub fn clear(&mut self) {
        self.parser = Parser::new();
        self.screen = ScreenBuffer::default();
    }

    fn push_output(&mut self, data: &[u8]) -> Option<ConsoleTailUpdate> {
        self.screen.begin_update();

        let mut performer = Performer {
            screen: &mut self.screen,
        };
        self.parser.advance(&mut performer, data);

        let dirty_from_row = self.screen.take_dirty_from_row()?;
        Some(ConsoleTailUpdate {
            dirty_from_row,
            logical_rows: self.screen.logical_rows_from(dirty_from_row),
        })
    }
}

struct Performer<'a> {
    screen: &'a mut ScreenBuffer,
}

impl Perform for Performer<'_> {
    fn print(&mut self, c: char) {
        self.screen.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.screen.new_line(),
            b'\r' => self.screen.carriage_return(),
            0x08 => self.screen.backspace(),
            b'\t' => self.screen.tab(),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, _intermediates: &[u8], _ignore: bool, action: char) {
        let mut iter = params.iter();
        let first = iter
            .next()
            .and_then(|param| param.first())
            .copied()
            .unwrap_or(0) as usize;

        match action {
            'C' => self.screen.move_right(first.max(1)),
            'D' => self.screen.move_left(first.max(1)),
            'G' => self.screen.set_cursor_column(first.max(1) - 1),
            'J' => self.screen.erase_in_display(first),
            'K' => self.screen.erase_in_line(first),
            _ => {}
        }
    }
}

#[derive(Clone, Default)]
enum ScreenCell {
    #[default]
    Empty,
    Cluster(String),
    Continuation,
}

impl ScreenCell {
    fn step_width(&self) -> usize {
        match self {
            Self::Empty | Self::Continuation => 1,
            Self::Cluster(text) => cluster_width(text),
        }
    }
}

#[derive(Default)]
struct ScreenBuffer {
    logical_rows: Vec<Vec<ScreenCell>>,
    cursor_row: usize,
    cursor_col: usize,
    dirty_from_row: Option<usize>,
}

impl ScreenBuffer {
    fn begin_update(&mut self) {
        self.dirty_from_row = None;
    }

    fn take_dirty_from_row(&mut self) -> Option<usize> {
        self.dirty_from_row.take()
    }

    fn logical_rows_from(&self, from_row: usize) -> Vec<Vec<ScreenCell>> {
        self.logical_rows.iter().skip(from_row).cloned().collect()
    }

    fn mark_dirty(&mut self, row_index: usize) {
        match &mut self.dirty_from_row {
            Some(existing) => *existing = (*existing).min(row_index),
            None => self.dirty_from_row = Some(row_index),
        }
    }

    fn ensure_cursor_row(&mut self) {
        while self.logical_rows.len() <= self.cursor_row {
            self.logical_rows.push(Vec::new());
            self.mark_dirty(self.logical_rows.len().saturating_sub(1));
        }
    }

    fn cursor_row_cells(&mut self) -> &[ScreenCell] {
        self.ensure_cursor_row();
        &self.logical_rows[self.cursor_row]
    }

    fn cursor_row_cells_mut(&mut self) -> &mut Vec<ScreenCell> {
        self.ensure_cursor_row();
        &mut self.logical_rows[self.cursor_row]
    }

    fn dirty_cursor_row_cells_mut(&mut self) -> &mut Vec<ScreenCell> {
        self.mark_dirty(self.cursor_row);
        self.cursor_row_cells_mut()
    }

    fn ensure_line_width(line: &mut Vec<ScreenCell>, width: usize) {
        while line.len() < width {
            line.push(ScreenCell::Empty);
        }
    }

    fn cluster_start(line: &[ScreenCell], col: usize) -> Option<usize> {
        if line.is_empty() || col >= line.len() {
            return None;
        }

        let mut index = col;
        while index > 0 && matches!(line[index], ScreenCell::Continuation) {
            index -= 1;
        }
        Some(index)
    }

    fn occupied_range(line: &[ScreenCell], col: usize) -> Option<(usize, usize)> {
        let start = Self::cluster_start(line, col)?;
        let end = start.saturating_add(line[start].step_width()).min(line.len());
        Some((start, end))
    }

    fn cluster_before(line: &[ScreenCell], col: usize) -> Option<usize> {
        if line.is_empty() || col == 0 {
            return None;
        }

        let anchor = col.saturating_sub(1).min(line.len().saturating_sub(1));
        let start = Self::cluster_start(line, anchor)?;
        matches!(line[start], ScreenCell::Cluster(_)).then_some(start)
    }

    fn previous_column(line: &[ScreenCell], col: usize) -> usize {
        if col == 0 {
            return 0;
        }

        let previous = col.saturating_sub(1);
        if previous >= line.len() {
            return previous;
        }

        Self::cluster_start(line, previous).expect("previous console column should resolve inside the current line")
    }

    fn next_column(line: &[ScreenCell], col: usize) -> usize {
        if col >= line.len() {
            return col.saturating_add(1);
        }

        col.saturating_add(line[col].step_width())
    }

    fn normalized_column(line: &[ScreenCell], col: usize) -> usize {
        if col >= line.len() {
            return col;
        }

        Self::cluster_start(line, col).expect("console column inside the current line should resolve to a cluster start")
    }

    fn previous_cursor_column(&mut self) -> usize {
        let cursor_col = self.cursor_col;
        Self::previous_column(self.cursor_row_cells(), cursor_col)
    }

    fn next_cursor_column(&mut self) -> usize {
        let cursor_col = self.cursor_col;
        Self::next_column(self.cursor_row_cells(), cursor_col)
    }

    fn normalized_cursor_column(&mut self, col: usize) -> usize {
        Self::normalized_column(self.cursor_row_cells(), col)
    }

    fn clear_logical_row(&mut self, row: usize) {
        if row < self.logical_rows.len() && !self.logical_rows[row].is_empty() {
            self.logical_rows[row].clear();
            self.mark_dirty(row);
        }
    }

    fn clear_occupied_cell(line: &mut [ScreenCell], col: usize) {
        let Some((start, end)) = Self::occupied_range(line, col) else {
            return;
        };

        line[start] = ScreenCell::Empty;
        for index in (start + 1)..end {
            line[index] = ScreenCell::Empty;
        }
    }

    fn trim_trailing_empty(line: &mut Vec<ScreenCell>) {
        while matches!(line.last(), Some(ScreenCell::Empty) | Some(ScreenCell::Continuation)) {
            line.pop();
        }
    }

    fn clear_range(line: &mut Vec<ScreenCell>, start: usize, end: usize) {
        if start >= end {
            return;
        }

        let end = end.min(line.len());
        let mut index = start;
        while index < end {
            let (_, occupied_end) = Self::occupied_range(line, index)
                .expect("cleared console range should always resolve to occupied cells inside the current line");
            Self::clear_occupied_cell(line, index);
            index = occupied_end;
        }
        Self::trim_trailing_empty(line);
    }

    fn append_combining_mark(line: &mut [ScreenCell], col: usize, ch: char) {
        let Some(index) = Self::cluster_before(line, col) else {
            return;
        };

        let ScreenCell::Cluster(text) = &mut line[index] else {
            return;
        };
        text.push(ch);
    }

    fn put_char(&mut self, ch: char) {
        let width = UnicodeWidthChar::width(ch).unwrap_or(1);
        let col = self.cursor_col;

        if width == 0 {
            let line = self.dirty_cursor_row_cells_mut();
            Self::append_combining_mark(line, col, ch);
            return;
        }

        let line = self.dirty_cursor_row_cells_mut();
        Self::ensure_line_width(line, col + width);
        for occupied in col..(col + width) {
            Self::clear_occupied_cell(line, occupied);
        }

        line[col] = ScreenCell::Cluster(ch.to_string());
        for continuation in (col + 1)..(col + width) {
            line[continuation] = ScreenCell::Continuation;
        }

        Self::trim_trailing_empty(line);
        self.cursor_col = col + width;
    }

    fn new_line(&mut self) {
        self.cursor_row += 1;
        self.cursor_col = 0;
        self.ensure_cursor_row();
    }

    fn carriage_return(&mut self) {
        self.cursor_col = 0;
    }

    fn backspace(&mut self) {
        self.cursor_col = self.previous_cursor_column();
    }

    fn tab(&mut self) {
        let next_stop = ((self.cursor_col / 8) + 1) * 8;
        while self.cursor_col < next_stop {
            self.put_char(' ');
        }
    }

    fn move_left(&mut self, count: usize) {
        for _ in 0..count {
            self.cursor_col = self.previous_cursor_column();
        }
    }

    fn move_right(&mut self, count: usize) {
        for _ in 0..count {
            self.cursor_col = self.next_cursor_column();
        }
    }

    fn set_cursor_column(&mut self, col: usize) {
        self.cursor_col = self.normalized_cursor_column(col);
    }

    fn erase_in_line(&mut self, mode: usize) {
        let col = self.cursor_col;
        let line = self.dirty_cursor_row_cells_mut();
        match mode {
            1 => Self::clear_range(line, 0, col.saturating_add(1)),
            2 => line.clear(),
            _ => Self::clear_range(line, col, line.len()),
        }
    }

    fn erase_in_display(&mut self, mode: usize) {
        self.ensure_cursor_row();
        match mode {
            1 => {
                for row in 0..self.cursor_row {
                    self.clear_logical_row(row);
                }
                let col = self.cursor_col;
                let line = self.dirty_cursor_row_cells_mut();
                Self::clear_range(line, 0, col.saturating_add(1));
            }
            2 => {
                self.logical_rows.clear();
                self.cursor_row = 0;
                self.cursor_col = 0;
                self.mark_dirty(0);
            }
            _ => {
                let col = self.cursor_col;
                let line = self.dirty_cursor_row_cells_mut();
                Self::clear_range(line, col, line.len());
                for row in (self.cursor_row + 1)..self.logical_rows.len() {
                    self.clear_logical_row(row);
                }
            }
        }
    }
}
