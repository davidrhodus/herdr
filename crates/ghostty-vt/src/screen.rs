use crate::{CellStyle, CellWide};
use std::{collections::VecDeque, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C, align(8))]
pub(crate) struct Cell {
    pub scalar: [u32; 1],
    pub cluster: u32,
    pub style: u16,
    pub link: u16,
    pub wide: CellWide,
}
const _: () = {
    assert!(std::mem::size_of::<CellWide>() == 4 && CellWide::Narrow as u32 == 0);
    assert!(std::mem::size_of::<Cell>() == 16 && std::mem::align_of::<Cell>() == 8);
    assert!(std::mem::offset_of!(Cell, scalar) == 0 && std::mem::offset_of!(Cell, cluster) == 4);
    assert!(
        std::mem::offset_of!(Cell, style) == 8
            && std::mem::offset_of!(Cell, link) == 10
            && std::mem::offset_of!(Cell, wide) == 12
    );
};
impl Default for Cell {
    fn default() -> Self {
        Self {
            scalar: [0],
            cluster: 0,
            style: 0,
            link: 0,
            wide: CellWide::Narrow,
        }
    }
}
impl Cell {
    #[inline]
    pub fn write_ascii(cells: &mut [Self], bytes: &[u8], style: u16, link: u16) {
        debug_assert_eq!(cells.len(), bytes.len());
        let [s0, s1] = style.to_ne_bytes();
        let [l0, l1] = link.to_ne_bytes();
        let metadata = u64::from_ne_bytes([s0, s1, l0, l1, 0, 0, 0, 0]);
        // SAFETY: Cell's asserted C layout consists of two aligned u64 words.
        // The first stores a Unicode scalar and zero cluster ID; the second
        // stores style/link IDs and the valid Narrow (zero) discriminant.
        // This exclusive view neither escapes nor reads uninitialized padding.
        let words = unsafe {
            std::slice::from_raw_parts_mut(cells.as_mut_ptr().cast::<[u64; 2]>(), cells.len())
        };
        for (cell, byte) in words.iter_mut().zip(bytes) {
            let cp = u32::from(*byte).to_ne_bytes();
            *cell = [
                u64::from_ne_bytes([cp[0], cp[1], cp[2], cp[3], 0, 0, 0, 0]),
                metadata,
            ];
        }
    }
    pub fn blank(style: u16) -> Self {
        Self {
            style,
            ..Self::default()
        }
    }
    pub fn has_text(&self) -> bool {
        self.scalar[0] != 0 || self.cluster != 0
    }
    pub fn meaningful(&self) -> bool {
        self.has_text() || self.style != 0 || self.link != 0
    }
}
pub(crate) static EMPTY_CELL: std::sync::LazyLock<Cell> = std::sync::LazyLock::new(Cell::default);
// Cold rows use run-length encoding; reads expand lazily and edits detach the cache.
#[derive(Clone)]
pub(crate) enum Cells {
    Dense(Vec<Cell>),
    Encoded {
        runs: Vec<(u16, Cell)>,
        decoded: std::sync::OnceLock<Vec<Cell>>,
    },
}
impl Cells {
    fn new(cells: Vec<Cell>) -> Self {
        Self::Dense(cells)
    }
    fn expand(runs: &[(u16, Cell)]) -> Vec<Cell> {
        let length = runs.iter().map(|(n, _)| *n as usize).sum();
        let mut cells = Vec::with_capacity(length);
        for (n, cell) in runs {
            cells.extend(std::iter::repeat_n(*cell, *n as usize));
        }
        cells
    }
    #[cfg(test)]
    pub(crate) fn storage_bytes(&self) -> usize {
        match self {
            Self::Dense(cells) => cells.capacity() * std::mem::size_of::<Cell>(),
            Self::Encoded { runs, decoded } => {
                runs.capacity() * std::mem::size_of::<(u16, Cell)>()
                    + decoded
                        .get()
                        .map_or(0, |cells| cells.capacity() * std::mem::size_of::<Cell>())
            }
        }
    }
    fn budget_bytes(&self) -> usize {
        match self {
            Self::Dense(cells) => cells.capacity() * std::mem::size_of::<Cell>(),
            // Reserve lazy expansion so reads do not exceed the history budget.
            Self::Encoded { runs, decoded } => {
                runs.capacity() * std::mem::size_of::<(u16, Cell)>()
                    + decoded.get().map_or_else(
                        || {
                            runs.iter().map(|(n, _)| *n as usize).sum::<usize>()
                                * std::mem::size_of::<Cell>()
                        },
                        |cells| cells.capacity() * std::mem::size_of::<Cell>(),
                    )
            }
        }
    }
    pub fn compress(&mut self) {
        let cells = match self {
            Self::Dense(cells) => cells,
            Self::Encoded { decoded, .. } => {
                decoded.take();
                return;
            }
        };
        let mut runs: Vec<(u16, Cell)> = Vec::new();
        for cell in cells.iter() {
            if let Some((count, previous)) = runs.last_mut() {
                if previous == cell && *count < u16::MAX {
                    *count += 1;
                    continue;
                }
            }
            runs.push((1, *cell));
        }
        if runs.len() * std::mem::size_of::<(u16, Cell)>()
            >= cells.len() * std::mem::size_of::<Cell>()
        {
            return;
        }
        runs.shrink_to_fit();
        *self = Self::Encoded {
            runs,
            decoded: std::sync::OnceLock::new(),
        };
    }
}
impl std::ops::Deref for Cells {
    type Target = Vec<Cell>;
    #[inline]
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Dense(cells) => cells,
            Self::Encoded { runs, decoded } => decoded.get_or_init(|| Self::expand(runs)),
        }
    }
}
impl Cells {
    #[cold]
    #[inline(never)]
    fn make_dense(&mut self) {
        if let Self::Encoded { runs, decoded } = self {
            let cells = decoded.take().unwrap_or_else(|| Self::expand(runs));
            *self = Self::Dense(cells);
        }
    }
}
impl std::ops::DerefMut for Cells {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        if matches!(self, Self::Encoded { .. }) {
            self.make_dense();
        }
        match self {
            Self::Dense(cells) => cells,
            Self::Encoded { .. } => unreachable!("decoded above"),
        }
    }
}
impl<'a> IntoIterator for &'a Cells {
    type Item = &'a Cell;
    type IntoIter = std::slice::Iter<'a, Cell>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<'a> IntoIterator for &'a mut Cells {
    type Item = &'a mut Cell;
    type IntoIter = std::slice::IterMut<'a, Cell>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}
#[derive(Clone)]
pub(crate) struct Row {
    pub cells: Cells,
    pub graphemes: Vec<Option<Arc<Vec<u32>>>>,
    pub wrapped: bool,
    pub continuation: bool,
    pub version: u64,
}
impl Row {
    pub fn new(cols: usize, version: u64) -> Self {
        Self {
            cells: Cells::new(vec![Cell::default(); cols]),
            graphemes: Vec::new(),
            wrapped: false,
            continuation: false,
            version,
        }
    }
    pub fn cell(&self, x: usize) -> &Cell {
        self.cells.get(x).unwrap_or(&EMPTY_CELL)
    }
    pub fn text<'a>(&'a self, cell: &'a Cell) -> &'a [u32] {
        if cell.cluster == 0 {
            if cell.scalar[0] == 0 {
                &[]
            } else {
                &cell.scalar
            }
        } else {
            self.graphemes
                .get(cell.cluster as usize - 1)
                .and_then(|p| p.as_ref())
                .map_or(&[], |p| p.as_slice())
        }
    }
    pub fn cluster(&self, cell: &Cell) -> Option<Arc<Vec<u32>>> {
        cell.cluster
            .checked_sub(1)
            .and_then(|id| self.graphemes.get(id as usize))
            .and_then(Clone::clone)
    }
    pub fn push_text(&self, cell: &Cell, out: &mut String) {
        let text = self.text(cell);
        if text.is_empty() && cell.wide == CellWide::Narrow {
            out.push(' ');
        } else {
            out.extend(text.iter().filter_map(|cp| char::from_u32(*cp)));
        }
    }
    pub fn string(&self, cell: &Cell) -> String {
        let mut out = String::new();
        self.push_text(cell, &mut out);
        out
    }
    fn collect_graphemes(&mut self) {
        if self.graphemes.is_empty() {
            return;
        }
        let mut used = vec![false; self.graphemes.len()];
        for c in &self.cells {
            if c.cluster > 0 {
                if let Some(v) = used.get_mut(c.cluster as usize - 1) {
                    *v = true;
                }
            }
        }
        for (i, p) in self.graphemes.iter_mut().enumerate() {
            if !used[i] {
                *p = None;
            }
        }
    }
    fn store_cluster(&mut self, cluster: Arc<Vec<u32>>) -> u32 {
        if self.graphemes.len() >= self.cells.len().max(1) {
            self.collect_graphemes();
        }
        if let Some(i) = self.graphemes.iter().position(Option::is_none) {
            self.graphemes[i] = Some(cluster);
            (i + 1) as u32
        } else {
            self.graphemes.push(Some(cluster));
            self.graphemes.len() as u32
        }
    }
    pub fn import(&mut self, x: usize, mut cell: Cell, cluster: Option<Arc<Vec<u32>>>) {
        cell.cluster = cluster.map_or(0, |value| self.store_cluster(value));
        self.cells[x] = cell;
    }
    pub fn append(&mut self, x: usize, cp: u32) {
        if self.cells[x].cluster == 0 {
            let value = Arc::new(vec![self.cells[x].scalar[0], cp]);
            self.cells[x].cluster = self.store_cluster(value);
        } else {
            let id = self.cells[x].cluster as usize - 1;
            if let Some(value) = self.graphemes.get_mut(id).and_then(Option::as_mut) {
                Arc::make_mut(value).push(cp);
            }
        }
    }
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.cells.budget_bytes()
            + self.graphemes.capacity() * std::mem::size_of::<Option<Arc<Vec<u32>>>>()
            + self
                .graphemes
                .iter()
                .flatten()
                .map(|g| g.capacity() * 4 + std::mem::size_of::<Vec<u32>>() + 16)
                .sum::<usize>()
    }
    pub fn compact(&mut self) {
        if !self.wrapped {
            let mut end = self
                .cells
                .iter()
                .rposition(Cell::meaningful)
                .map_or(0, |i| i + 1);
            if end > 0 && self.cells[end - 1].wide == CellWide::Wide {
                end = (end + 1).min(self.cells.len());
            }
            self.cells.truncate(end);
            self.cells.shrink_to_fit();
        }
        self.collect_graphemes();
        let end = self
            .graphemes
            .iter()
            .rposition(Option::is_some)
            .map_or(0, |i| i + 1);
        self.graphemes.truncate(end);
        self.graphemes.shrink_to_fit();
    }
    pub fn repair_wide(&mut self) {
        for x in 0..self.cells.len() {
            let invalid = match self.cells[x].wide {
                CellWide::Wide => {
                    x + 1 >= self.cells.len() || self.cells[x + 1].wide != CellWide::SpacerTail
                }
                CellWide::SpacerTail => x == 0 || self.cells[x - 1].wide != CellWide::Wide,
                _ => false,
            };
            if invalid {
                self.cells[x] = Cell::default();
            }
        }
    }
}
#[derive(Clone, Default)]
pub(crate) struct SavedCursor {
    pub x: usize,
    pub y: usize,
    pub style: CellStyle,
    pub origin: bool,
    pub wrap_pending: bool,
}
pub(crate) struct Scroll {
    pub top: i64,
    pub bottom: i64,
    pub delta: i64,
}
pub(crate) struct Screen {
    pub scrolls: Vec<Scroll>,
    pub lines: VecDeque<Arc<Row>>,
    pub cols: usize,
    pub rows: usize,
    pub x: usize,
    pub y: usize,
    pub wrap_pending: bool,
    pub top: usize,
    pub bottom: usize,
    pub offset: usize,
    pub budget: usize,
    pub history_bytes: usize,
    pub saved: SavedCursor,
    pub revision: u64,
    pub layout_revision: u64,
    pub dropped: u64,
}
impl Screen {
    pub fn new(cols: usize, rows: usize, budget: usize) -> Self {
        Self {
            scrolls: Vec::new(),
            lines: (0..rows)
                .map(|i| Arc::new(Row::new(cols, i as u64 + 1)))
                .collect(),
            cols,
            rows,
            x: 0,
            y: 0,
            wrap_pending: false,
            top: 0,
            bottom: rows - 1,
            offset: 0,
            budget,
            history_bytes: 0,
            saved: SavedCursor::default(),
            revision: rows as u64 + 1,
            layout_revision: 1,
            dropped: 0,
        }
    }
    #[inline]
    pub fn history(&self) -> usize {
        self.lines.len().saturating_sub(self.rows)
    }
    pub fn viewport_top(&self) -> usize {
        self.history().saturating_sub(self.offset)
    }
    #[inline]
    pub fn row_mut(&mut self, index: usize) -> &mut Row {
        self.revision = self.revision.wrapping_add(1);
        let row = Arc::make_mut(&mut self.lines[index]);
        row.version = self.revision;
        row
    }
    #[inline]
    pub fn live_row_mut(&mut self, y: usize) -> &mut Row {
        let index = self.history() + y;
        self.row_mut(index)
    }
    pub fn blank(&mut self, style: u16) -> Arc<Row> {
        self.revision = self.revision.wrapping_add(1);
        let mut row = Row::new(self.cols, self.revision);
        if style != 0 {
            row.cells.fill(Cell::blank(style));
        }
        Arc::new(row)
    }
    pub fn trim_history(&mut self) {
        let limit = if self.budget == 0 {
            0
        } else {
            self.budget.max(1_048_576)
        };
        while self.history() > 0 && self.history_bytes > limit {
            if let Some(row) = self.lines.pop_front() {
                self.history_bytes = self.history_bytes.saturating_sub(row.bytes());
                self.dropped += 1;
            }
        }
        self.offset = self.offset.min(self.history());
    }
    pub fn scroll_up(&mut self, count: usize, style: u16) {
        self.scroll_up_inner(count, style, true);
    }
    pub fn delete_lines(&mut self, count: usize, style: u16) {
        self.scroll_up_inner(count, style, false);
    }
    fn scroll_up_inner(&mut self, count: usize, style: u16, keep_history: bool) {
        let count = count.min(self.bottom - self.top + 1);
        if self.top != 0 || self.bottom != self.rows - 1 {
            let base = self.dropped as i64 + self.history() as i64;
            self.scrolls.push(Scroll {
                top: base + self.top as i64,
                bottom: base + self.bottom as i64,
                delta: -(count as i64),
            });
        }
        for _ in 0..count {
            let blank = self.blank(style);
            if keep_history && self.top == 0 && self.bottom == self.rows - 1 {
                if self.offset > 0 {
                    self.offset = self.offset.saturating_add(1);
                }
                let index = self.history();
                Arc::make_mut(&mut self.lines[index]).compact();
                self.history_bytes = self.history_bytes.saturating_add(self.lines[index].bytes());
                self.lines.push_back(blank);
                self.trim_history();
            } else {
                let base = self.history();
                self.lines.remove(base + self.top);
                self.lines.insert(base + self.bottom, blank);
            }
        }
        self.layout_revision = self.layout_revision.wrapping_add(1);
    }
    pub fn scroll_down(&mut self, count: usize, style: u16) {
        let global = self.dropped as i64 + self.history() as i64;
        self.scrolls.push(Scroll {
            top: global + self.top as i64,
            bottom: global + self.bottom as i64,
            delta: count.min(self.bottom - self.top + 1) as i64,
        });
        let base = self.history();
        for _ in 0..count.min(self.bottom - self.top + 1) {
            let blank = self.blank(style);
            self.lines.remove(base + self.bottom);
            self.lines.insert(base + self.top, blank);
        }
        self.layout_revision = self.layout_revision.wrapping_add(1);
    }
    pub fn newline(&mut self, style: u16) {
        self.wrap_pending = false;
        if self.y == self.bottom {
            self.scroll_up(1, style);
        } else {
            self.y = (self.y + 1).min(self.rows - 1);
        }
    }
    pub fn clear_history(&mut self) {
        let count = self.history();
        self.lines.drain(..count);
        self.dropped += count as u64;
        self.offset = 0;
        self.history_bytes = 0;
        self.layout_revision = self.layout_revision.wrapping_add(1);
    }
    pub fn clear(&mut self, style: u16) {
        for y in 0..self.rows {
            let index = self.history() + y;
            let blank = self.blank(style);
            self.lines[index] = blank;
        }
        self.layout_revision = self.layout_revision.wrapping_add(1);
    }
    pub fn erase(&mut self, y: usize, start: usize, end: usize, style: u16) {
        if start >= end || y >= self.rows {
            return;
        }
        let end = end.min(self.cols);
        let row = self.live_row_mut(y);
        let start = if start > 0 && row.cells[start].wide == CellWide::SpacerTail {
            start - 1
        } else {
            start
        };
        let end = if end < row.cells.len() && row.cells[end - 1].wide == CellWide::Wide {
            end + 1
        } else {
            end
        };
        row.cells[start..end].fill(Cell::blank(style));
        row.wrapped = false;
    }
    pub fn put(&mut self, mut cell: Cell, width: usize, autowrap: bool, insert: bool, style: u16) {
        if width > self.cols {
            return;
        }
        if self.wrap_pending && autowrap {
            self.live_row_mut(self.y).wrapped = true;
            self.newline(style);
            self.x = 0;
            self.live_row_mut(self.y).continuation = true;
        }
        self.wrap_pending = false;
        if width == 2 && self.x + 1 >= self.cols {
            if autowrap {
                let x = self.x;
                let row = self.live_row_mut(self.y);
                row.cells[x] = Cell {
                    wide: CellWide::SpacerHead,
                    ..Cell::default()
                };
                row.wrapped = true;
                self.newline(style);
                self.x = 0;
                self.live_row_mut(self.y).continuation = true;
            } else {
                return;
            }
        }
        let x = self.x;
        let cols = self.cols;
        let row = self.live_row_mut(self.y);
        if insert {
            let n = width.min(cols - x);
            row.cells[x..].rotate_right(n);
            row.cells[x..x + n].fill(Cell::blank(style));
        }
        if row.cells[x].wide == CellWide::SpacerTail && x > 0 {
            row.cells[x - 1] = Cell::blank(style);
        }
        if row.cells[x].wide == CellWide::Wide && x + 1 < cols {
            row.cells[x + 1] = Cell::blank(style);
        }
        if width == 2 && x + 2 < cols && row.cells[x + 1].wide == CellWide::Wide {
            row.cells[x + 2] = Cell::blank(style);
        }
        cell.wide = if width == 2 {
            CellWide::Wide
        } else {
            CellWide::Narrow
        };
        row.cells[x] = cell;
        if width == 2 {
            row.cells[x + 1] = Cell {
                wide: CellWide::SpacerTail,
                ..Cell::default()
            };
        }
        self.x = x + width;
        if self.x >= cols {
            self.x = cols - 1;
            self.wrap_pending = true;
        }
    }
    pub fn resize(
        &mut self,
        cols: usize,
        rows: usize,
        reflow: bool,
        tracked: &mut [(usize, usize)],
    ) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        let old_dropped = self.dropped;
        let old_history = self.history();
        let old_cursor = old_history + self.y;
        let old_view = self.viewport_top();
        let mut cursor = (old_cursor, self.x);
        let old_cols = self.cols;
        if cols != old_cols && reflow {
            let old = std::mem::take(&mut self.lines);
            let mut output = VecDeque::new();
            let mut group = Vec::new();
            let mut cursor_offset = None;
            let mut by_row: std::collections::HashMap<usize, Vec<(usize, usize)>> =
                std::collections::HashMap::new();
            for (id, &(row, col)) in tracked.iter().enumerate() {
                by_row.entry(row).or_default().push((id, col));
            }
            let mut group_points = Vec::new();
            for (index, row) in old.iter().enumerate() {
                if index == old_cursor {
                    cursor_offset = Some(group.len() + self.x);
                }
                let start_offset = group.len();
                if let Some(points) = by_row.get(&index) {
                    for &(id, col) in points {
                        group_points.push((id, start_offset + col));
                    }
                }
                let mut end = row.cells.len();
                if !row.wrapped {
                    while end > 0
                        && !row.cells[end - 1].meaningful()
                        && !(index == old_cursor && end <= self.x)
                    {
                        end -= 1;
                    }
                }
                if let Some(points) = by_row.get(&index) {
                    if let Some(col) = points.iter().map(|p| p.1 + 1).max() {
                        end = end.max(col).min(old_cols);
                    }
                }
                for col in 0..end {
                    let cell = row.cell(col);
                    if cell.wide != CellWide::SpacerHead {
                        group.push((*cell, row.cluster(cell)));
                    }
                }
                if row.wrapped && index + 1 < old.len() {
                    continue;
                }
                let mut outrow = Row::new(cols, 0);
                let mut x = 0;
                let mut continued = false;
                let mut i = 0;
                while i < group.len() {
                    let (cell, cluster) = &group[i];
                    if cell.wide == CellWide::SpacerTail {
                        i += 1;
                        continue;
                    }
                    if cols == 1 && cell.wide == CellWide::Wide {
                        if cursor_offset == Some(i) {
                            cursor = (output.len(), x);
                        }
                        for &(id, offset) in &group_points {
                            if offset >= i && offset < i + 2 {
                                tracked[id] = (output.len(), x);
                            }
                        }
                        i += 2;
                        continue;
                    }
                    let width = if cell.wide == CellWide::Wide {
                        2.min(cols)
                    } else {
                        1
                    };
                    if x + width > cols {
                        if x < cols {
                            outrow.cells[x] = Cell {
                                wide: CellWide::SpacerHead,
                                ..Cell::default()
                            };
                        }
                        outrow.wrapped = true;
                        outrow.continuation = continued;
                        output.push_back(Arc::new(outrow));
                        continued = true;
                        outrow = Row::new(cols, 0);
                        x = 0;
                    }
                    if cursor_offset == Some(i) {
                        cursor = (output.len(), x);
                    }
                    for &(id, offset) in &group_points {
                        if offset >= i && offset < i + width {
                            tracked[id] = (output.len(), (x + offset - i).min(cols - 1));
                        }
                    }
                    outrow.import(x, *cell, cluster.clone());
                    if width == 2 {
                        outrow.cells[x + 1] = Cell {
                            wide: CellWide::SpacerTail,
                            ..Cell::default()
                        };
                    }
                    x += width;
                    i += width;
                    if x == cols && i < group.len() {
                        outrow.wrapped = true;
                        outrow.continuation = continued;
                        output.push_back(Arc::new(outrow));
                        continued = true;
                        outrow = Row::new(cols, 0);
                        x = 0;
                    }
                }
                if cursor_offset.is_some_and(|off| off >= group.len()) {
                    cursor = (output.len(), x.min(cols - 1));
                }
                for &(id, offset) in &group_points {
                    if offset >= group.len() {
                        tracked[id] = (output.len(), x.min(cols - 1));
                    }
                }
                group_points.clear();
                outrow.continuation = continued;
                output.push_back(Arc::new(outrow));
                group.clear();
                cursor_offset = None;
            }
            self.lines = output;
        } else if cols != old_cols {
            for index in 0..self.lines.len() {
                let row = self.row_mut(index);
                row.cells.resize(cols, Cell::default());
                row.repair_wide();
                row.wrapped = false;
            }
        }
        // Empty rows below the cursor should not push visible content into
        // history when only the window's height shrinks.
        while self.lines.len() > rows
            && self.lines.len() > cursor.0 + 1
            && self
                .lines
                .back()
                .is_some_and(|r| r.cells.iter().all(|c| !c.meaningful()))
        {
            self.lines.pop_back();
        }
        self.cols = cols;
        self.rows = rows;
        while self.lines.len() < rows {
            let blank = self.blank(0);
            self.lines.push_back(blank);
        }
        let history = self.history();
        self.y = cursor.0.saturating_sub(history).min(rows - 1);
        self.x = cursor.1.min(cols - 1);
        self.wrap_pending = false;
        self.top = 0;
        self.bottom = rows - 1;
        self.offset = if self.offset == 0 {
            0
        } else {
            history.saturating_sub(old_view.min(history))
        };
        let history = self.history();
        self.history_bytes = 0;
        for index in 0..self.lines.len() {
            let row = self.row_mut(index);
            if index < history {
                row.compact();
            } else {
                row.cells.resize(cols, Cell::default());
            }
            if index < history {
                self.history_bytes = self.history_bytes.saturating_add(self.lines[index].bytes());
            }
        }
        self.trim_history();
        let removed = self.dropped.saturating_sub(old_dropped) as usize;
        for point in tracked {
            point.0 = point.0.checked_sub(removed).unwrap_or(usize::MAX);
            point.1 = point.1.min(cols - 1);
        }
        self.layout_revision = self.layout_revision.wrapping_add(1);
    }
}
