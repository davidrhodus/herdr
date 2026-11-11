//! Kitty graphics storage and placement geometry, owned entirely by Rust.
use crate::*;
use base64::Engine as _;
use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    io::Read,
    sync::Arc,
};
const LIMIT: usize = 64 * 1024 * 1024;
#[cfg(test)]
thread_local! {pub(crate) static PNG_DECODE_CALLS:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
#[derive(Clone)]
pub(crate) struct Image {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub format: KittyImageFormat,
    pub data: Arc<[u8]>,
    pub source: Option<Arc<OwnedExport>>,
    pub fingerprint: u64,
    pub generation: u64,
}
impl Image {
    fn len(&self) -> usize {
        self.source
            .as_ref()
            .map_or(self.data.len(), |source| source.len())
    }
}
#[derive(Clone)]
struct Placement {
    id: u32,
    image: u32,
    alt: bool,
    col: i32,
    row: i64,
    columns: u32,
    rows: u32,
    z: i32,
    x_offset: u32,
    y_offset: u32,
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
    virtual_: bool,
}
#[derive(Default)]
pub(crate) struct Graphics {
    pub enabled: bool,
    pub forwarding: bool,
    pub png_forwarding: bool,
    pub generation: u64,
    pub images: HashMap<u32, Image>,
    placements: Vec<Placement>,
    chunk: Option<(HashMap<u8, i64>, Vec<u8>)>,
    next_id: u32,
}
fn value(params: &HashMap<u8, i64>, key: u8, default: i64) -> i64 {
    params.get(&key).copied().unwrap_or(default)
}
fn number(params: &HashMap<u8, i64>, key: u8, default: u32) -> u32 {
    value(params, key, default as i64).clamp(0, u32::MAX as i64) as u32
}
impl Graphics {
    pub(crate) fn clear_screen(&mut self, screen: &crate::screen::Screen, cell_height: u32) {
        let before = (self.placements.len(), self.images.len());
        let live = screen.dropped as i64 + screen.history() as i64;
        let bottom = live + screen.rows as i64;
        self.placements.retain(|p| {
            let rows = if p.rows > 0 {
                p.rows
            } else {
                p.source_height.div_ceil(cell_height.max(1)).max(1)
            };
            p.virtual_ || p.row >= bottom || p.row.saturating_add(rows as i64) <= live
        });
        self.images
            .retain(|id, _| self.placements.iter().any(|p| p.image == *id));
        if before != (self.placements.len(), self.images.len()) {
            self.changed();
        }
    }

    pub(crate) fn scroll(&mut self, scrolls: &[crate::screen::Scroll]) {
        for s in scrolls {
            self.placements.retain_mut(|p| {
                if p.virtual_ || p.row < s.top || p.row > s.bottom {
                    return true;
                }
                p.row += s.delta;
                p.row >= s.top && p.row <= s.bottom
            });
        }
    }

    pub(crate) fn anchors(&self, screen: &crate::screen::Screen) -> Vec<(usize, (usize, usize))> {
        self.placements
            .iter()
            .enumerate()
            .filter_map(|(id, p)| {
                let row = p.row.checked_sub(screen.dropped as i64)?;
                if !p.virtual_ && row >= 0 && (row as usize) < screen.lines.len() {
                    Some((id, (row as usize, p.col.max(0) as usize)))
                } else {
                    None
                }
            })
            .collect()
    }
    pub(crate) fn remap(
        &mut self,
        anchors: &[(usize, (usize, usize))],
        points: &[(usize, usize)],
        screen: &crate::screen::Screen,
    ) {
        for ((id, _), &(row, col)) in anchors.iter().zip(points) {
            if let Some(p) = self.placements.get_mut(*id) {
                p.row = if row == usize::MAX {
                    -4_294_967_296
                } else {
                    screen.dropped as i64 + row as i64
                };
                p.col = col as i32;
            }
        }
    }
    pub fn reset(&mut self) {
        self.images.clear();
        self.placements.clear();
        self.chunk = None;
        self.generation = self.generation.wrapping_add(1);
    }
    fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1).max(1);
    }
    pub fn command(&mut self, terminal: &mut Terminal, payload: &[u8]) {
        if !self.enabled || payload.first() != Some(&b'G') {
            return;
        }
        let split = payload
            .iter()
            .position(|b| *b == b';')
            .unwrap_or(payload.len());
        let mut params = HashMap::new();
        for part in payload[1..split].split(|b| *b == b',') {
            if part.len() < 3 || part[1] != b'=' {
                continue;
            }
            let raw = &part[2..];
            let val = if raw.len() == 1 && raw[0].is_ascii_alphabetic() {
                raw[0] as i64
            } else {
                let Ok(v) = std::str::from_utf8(raw).unwrap_or("").parse::<i64>() else {
                    return;
                };
                v
            };
            params.insert(part[0], val);
        }
        let encoded = payload.get(split + 1..).unwrap_or_default();
        let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
            return;
        };
        let mut data = decoded;
        if let Some((mut inherited, mut prior)) = self.chunk.take() {
            for (k, v) in params {
                inherited.insert(k, v);
            }
            params = inherited;
            if prior.len().saturating_add(data.len()) > LIMIT {
                return;
            }
            prior.extend_from_slice(&data);
            data = prior;
        }
        if value(&params, b'm', 0) == 1 {
            if data.len() <= LIMIT {
                self.chunk = Some((params, data));
            }
            return;
        }
        let action = value(&params, b'a', b't' as i64) as u8;
        let quiet = number(&params, b'q', 0);
        let mut id = number(&params, b'i', 0);
        if action == b'd' {
            self.delete(&params, terminal);
            return;
        }
        if matches!(action, b'a' | b'f') {
            if let Some(image) = self.images.get_mut(&id) {
                if image.format == KittyImageFormat::Png {
                    if let Some((w, h, bytes)) = decode_png(&image.data) {
                        image.width = w;
                        image.height = h;
                        image.format = KittyImageFormat::Rgba;
                        image.data = bytes.into();
                        image.fingerprint = fingerprint(&image.data, w, h, image.format);
                        image.generation = self.generation.wrapping_add(1);
                        self.changed();
                    }
                }
            }
            return;
        }
        if action == b'p' {
            self.place(terminal, id, &params);
            return;
        }
        if !matches!(action, b't' | b'T' | b'q') {
            return;
        }
        if id == 0 {
            self.next_id = self.next_id.wrapping_add(1).max(1);
            id = self.next_id;
        }
        if matches!(action, b't' | b'T') {
            self.placements.retain(|p| p.image != id);
        }
        let format = number(&params, b'f', 32);
        let mut width = number(&params, b's', 0);
        let mut height = number(&params, b'v', 0);
        let mut source = None;
        let medium = value(&params, b't', b'd' as i64) as u8;
        if matches!(medium, b'f' | b't' | b's') {
            let path = String::from_utf8_lossy(&data);
            match crate::native_source::load(
                &path,
                medium,
                number(&params, b'O', 0) as u64,
                params
                    .get(&b'S')
                    .copied()
                    .and_then(|v| usize::try_from(v).ok()),
                self.forwarding && format == 32 && value(&params, b'o', 0) == 0,
            ) {
                Ok((bytes, retained)) => {
                    data = bytes;
                    source = retained;
                }
                Err(_) => {
                    self.respond(
                        terminal,
                        id,
                        quiet,
                        action,
                        "ENOENT:cannot read image source",
                    );
                    return;
                }
            }
        }
        if value(&params, b'o', 0) == b'z' as i64 {
            let mut decompressed = Vec::new();
            if flate2::read::ZlibDecoder::new(&data[..])
                .take((LIMIT + 1) as u64)
                .read_to_end(&mut decompressed)
                .is_err()
                || decompressed.len() > LIMIT
            {
                self.respond(
                    terminal,
                    id,
                    quiet,
                    action,
                    "EINVAL:invalid compressed data",
                );
                return;
            }
            data = decompressed;
        }
        let mut kind = if format == 24 {
            KittyImageFormat::Rgb
        } else {
            KittyImageFormat::Rgba
        };
        let valid = if format == 100 {
            if self.png_forwarding && quiet == 2 && action != b'q' {
                if let Some((w, h)) = png_dimensions(&data) {
                    width = w;
                    height = h;
                    kind = KittyImageFormat::Png;
                    true
                } else {
                    false
                }
            } else if let Some((w, h, bytes)) = decode_png(&data) {
                width = w;
                height = h;
                data = bytes;
                true
            } else {
                false
            }
        } else {
            matches!(format, 24 | 32)
                && width > 0
                && height > 0
                && u64::from(width)
                    .checked_mul(u64::from(height))
                    .and_then(|n| n.checked_mul(if format == 24 { 3 } else { 4 }))
                    == Some(source.as_ref().map_or(data.len(), |source| source.len()) as u64)
        };
        if !valid || data.len() > LIMIT {
            self.respond(terminal, id, quiet, action, "EINVAL:invalid image data");
            return;
        }
        if action == b'q' {
            self.respond(terminal, id, quiet, action, "OK");
            return;
        }
        self.changed();
        self.placements.retain(|p| p.image != id);
        let total: usize = self
            .images
            .iter()
            .filter(|(i, _)| **i != id)
            .map(|(_, image)| image.len())
            .sum();
        if total.saturating_add(source.as_ref().map_or(data.len(), |source| source.len())) > LIMIT {
            self.respond(terminal, id, quiet, action, "ENOSPC:image storage limit");
            return;
        }
        let fp = source.as_ref().map_or_else(
            || fingerprint(&data, width, height, kind),
            |s| s.fingerprint(),
        );
        if self.images.len() >= 1024 && !self.images.contains_key(&id) {
            return;
        }
        self.images.insert(
            id,
            Image {
                id,
                width,
                height,
                format: kind,
                data: data.into(),
                source,
                fingerprint: fp,
                generation: self.generation,
            },
        );
        if action == b'T' {
            self.place(terminal, id, &params);
        }
        self.respond(terminal, id, quiet, action, "OK");
    }
    fn respond(&self, terminal: &mut Terminal, id: u32, quiet: u32, action: u8, result: &str) {
        if action == b'q' || (quiet == 0 || (quiet == 1 && result != "OK")) {
            terminal.reply(format!("\x1b_Gi={id};{result}\x1b\\").as_bytes());
        }
    }
    fn place(&mut self, terminal: &mut Terminal, id: u32, params: &HashMap<u8, i64>) {
        let Some(image) = self.images.get(&id) else {
            return;
        };
        let placement_id = number(params, b'p', 0);
        let virtual_ = number(params, b'U', 0) == 1;
        if virtual_ && self.placements.iter().any(|p| p.image == id && !p.virtual_) {
            return;
        }
        if self.placements.len() >= 4096
            && !self
                .placements
                .iter()
                .any(|p| p.image == id && p.id == placement_id && p.alt == terminal.alt)
        {
            return;
        }
        let columns = number(params, b'c', 0);
        let rows = number(params, b'r', 0);
        let s = terminal.screen();
        let placement = Placement {
            id: placement_id,
            image: id,
            alt: terminal.alt,
            col: s.x as i32,
            row: s.dropped as i64 + s.history() as i64 + s.y as i64,
            columns,
            rows,
            z: value(params, b'z', 0) as i32,
            x_offset: number(params, b'X', 0),
            y_offset: number(params, b'Y', 0),
            source_x: number(params, b'x', 0),
            source_y: number(params, b'y', 0),
            source_width: number(params, b'w', image.width),
            source_height: number(params, b'h', image.height),
            virtual_,
        };
        let cursor_rows = if rows == 0 {
            image.height.div_ceil(terminal.cell_height.max(1))
        } else {
            rows
        };
        self.placements
            .retain(|p| !(p.image == id && p.id == placement_id && p.alt == terminal.alt));
        self.placements.push(placement);
        let image_width = image.width;
        self.changed();
        if number(params, b'C', 0) == 0 && !virtual_ {
            let style = terminal.erase_style();
            for _ in 1..cursor_rows.min(terminal.screen().rows as u32) {
                terminal.screen_mut().newline(style);
            }
            let width = if columns == 0 {
                image_width.div_ceil(terminal.cell_width.max(1))
            } else {
                columns
            };
            let s = terminal.screen_mut();
            s.x = (s.x + width as usize).min(s.cols - 1);
        }
    }
    fn delete(&mut self, params: &HashMap<u8, i64>, terminal: &Terminal) {
        let mode = value(params, b'd', b'a' as i64) as u8;
        let id = number(params, b'i', 0);
        let pid = number(params, b'p', 0);
        let before = (self.placements.len(), self.images.len());
        match mode {
            b'a' | b'A' => {
                let live = terminal.screen().dropped as i64 + terminal.screen().history() as i64;
                let bottom = live + terminal.screen().rows as i64;
                let removed: Vec<_> = self
                    .placements
                    .iter()
                    .filter(|p| p.alt == terminal.alt && p.row >= live && p.row < bottom)
                    .map(|p| p.image)
                    .collect();
                self.placements
                    .retain(|p| !(p.alt == terminal.alt && p.row >= live && p.row < bottom));
                if mode == b'A' {
                    for image in removed {
                        if !self.placements.iter().any(|p| p.image == image) {
                            self.images.remove(&image);
                        }
                    }
                }
            }
            b'i' | b'I' => {
                self.placements
                    .retain(|p| !(p.image == id && (pid == 0 || p.id == pid)));
                if mode == b'I' {
                    self.images.remove(&id);
                }
            }
            b'p' | b'P' => self.placements.retain(|p| p.id != pid),
            _ => {}
        }
        if before != (self.placements.len(), self.images.len()) {
            self.changed();
        }
    }
    pub fn placements<F: FnMut(KittyImageDescriptor) -> bool>(
        &self,
        terminal: &Terminal,
        mut needs_data: F,
    ) -> Vec<KittyImagePlacement> {
        let mut out = Vec::new();
        let s = terminal.screen();
        let top = s.dropped as i64 + s.viewport_top() as i64;
        let cw = terminal.cell_width.max(1);
        let ch = terminal.cell_height.max(1);
        for p in self.placements.iter().filter(|p| p.alt == terminal.alt) {
            let Some(image) = self.images.get(&p.image) else {
                continue;
            };
            if p.virtual_ {
                self.virtual_placements(terminal, p, image, &mut needs_data, &mut out);
                continue;
            }
            let columns = if p.columns == 0 {
                p.source_width.div_ceil(cw)
            } else {
                p.columns
            }
            .max(1);
            let rows = if p.rows == 0 {
                p.source_height.div_ceil(ch)
            } else {
                p.rows
            }
            .max(1);
            let row = p.row - top;
            let visible_top = row.max(0);
            let visible_bottom = (row + rows as i64).min(s.rows as i64);
            let left = p.col.max(0);
            let right = (p.col as i64 + columns as i64).min(s.cols as i64) as i32;
            if visible_top >= visible_bottom || left >= right {
                continue;
            }
            let dy = (visible_top - row) as u32;
            let dx = (left - p.col) as u32;
            let visible_rows = (visible_bottom - visible_top) as u32;
            let visible_cols = (right - left) as u32;
            let render = KittyPlacementRenderInfo {
                pixel_width: if p.columns == 0 {
                    p.source_width.min(visible_cols * cw)
                } else {
                    visible_cols * cw
                },
                pixel_height: if p.rows == 0 {
                    p.source_height.min(visible_rows * ch)
                } else {
                    visible_rows * ch
                },
                grid_cols: visible_cols,
                grid_rows: visible_rows,
                viewport_col: left,
                viewport_row: visible_top as i32,
                source_x: p.source_x + scale(dx, p.source_width, columns),
                source_y: p.source_y + scale(dy, p.source_height, rows),
                source_width: scale(visible_cols, p.source_width, columns).max(1),
                source_height: scale(visible_rows, p.source_height, rows).max(1),
            };
            out.push(make_placement(
                image,
                p.id,
                p.z,
                p.x_offset,
                p.y_offset,
                render,
                &mut needs_data,
            ));
        }
        out
    }
    fn virtual_placements<F: FnMut(KittyImageDescriptor) -> bool>(
        &self,
        terminal: &Terminal,
        p: &Placement,
        image: &Image,
        needs_data: &mut F,
        out: &mut Vec<KittyImagePlacement>,
    ) {
        let s = terminal.screen();
        let cw = terminal.cell_width.max(1);
        let ch = terminal.cell_height.max(1);
        let columns = p.columns.max(1);
        let rows = p.rows.max(1);
        for (y, row) in s
            .lines
            .iter()
            .skip(s.viewport_top())
            .take(s.rows)
            .enumerate()
        {
            let mut previous_row = 0;
            let mut previous_col = 0;
            for (x, cell) in row.cells.iter().enumerate() {
                if cell.scalar[0] != KITTY_UNICODE_PLACEHOLDER {
                    continue;
                }
                let image_id = color_id(terminal.cell_style(cell).fg_color);
                if image_id != p.image & 0xffffff {
                    continue;
                }
                let requested = color_id(terminal.cell_style(cell).underline_color);
                if requested != 0 && requested != p.id {
                    continue;
                }
                let marks: Vec<u32> = row
                    .text(cell)
                    .iter()
                    .skip(1)
                    .filter_map(|cp| diacritic(*cp))
                    .collect();
                let source_row = marks.first().copied().unwrap_or(previous_row);
                let source_col = marks.get(1).copied().unwrap_or(previous_col);
                previous_row = source_row;
                previous_col = source_col + 1;
                if source_row >= rows || source_col >= columns {
                    continue;
                }
                let render = KittyPlacementRenderInfo {
                    pixel_width: cw,
                    pixel_height: ch,
                    grid_cols: 1,
                    grid_rows: 1,
                    viewport_col: x as i32,
                    viewport_row: y as i32,
                    source_x: scale(source_col, image.width, columns),
                    source_y: scale(source_row, image.height, rows),
                    source_width: scale(1, image.width, columns).max(1),
                    source_height: scale(1, image.height, rows).max(1),
                };
                if let Some(last) = out.last_mut().filter(|last| {
                    last.image_id == image.id
                        && last.render.viewport_row == y as i32
                        && last.render.viewport_col + last.render.grid_cols as i32 == x as i32
                        && last.render.source_y == render.source_y
                        && last.render.source_x + last.render.source_width >= render.source_x
                }) {
                    last.render.grid_cols += 1;
                    last.render.pixel_width += cw;
                    last.render.source_width = (render.source_x + render.source_width
                        - last.render.source_x)
                        .max(last.render.source_width)
                        .min(image.width - last.render.source_x);
                } else {
                    out.push(make_placement(
                        image,
                        if p.id == 0 {
                            0x80000000 | ((y as u32) << 16) | x as u32
                        } else {
                            p.id
                        },
                        p.z,
                        0,
                        0,
                        render,
                        needs_data,
                    ));
                }
            }
        }
    }
}
fn make_placement<F: FnMut(KittyImageDescriptor) -> bool>(
    image: &Image,
    id: u32,
    z: i32,
    x_offset: u32,
    y_offset: u32,
    render: KittyPlacementRenderInfo,
    needs_data: &mut F,
) -> KittyImagePlacement {
    let descriptor = KittyImageDescriptor {
        image_id: image.id,
        placement_id: id,
        image_width: image.width,
        image_height: image.height,
        format: image.format,
        data_len: image.len(),
        data_fingerprint: image.fingerprint,
        source_file: image.source.is_some(),
    };
    let data = if needs_data(descriptor) {
        image.source.as_ref().map_or_else(
            || image.data.to_vec(),
            |source| source.copy_rgba().unwrap_or_default(),
        )
    } else {
        Vec::new()
    };
    KittyImagePlacement {
        image_id: image.id,
        placement_id: id,
        z,
        x_offset,
        y_offset,
        image_width: image.width,
        image_height: image.height,
        format: image.format,
        data_len: image.len(),
        data_fingerprint: image.fingerprint,
        data,
        source_file: image.source.clone(),
        render,
    }
}
fn color_id(color: Option<CellColor>) -> u32 {
    match color {
        Some(CellColor::Rgb(c)) => (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b),
        Some(CellColor::Palette(i)) => u32::from(i),
        None => 0,
    }
}
fn scale(value: u32, source: u32, dest: u32) -> u32 {
    ((u64::from(value) * u64::from(source)) / u64::from(dest.max(1))).min(u32::MAX as u64) as u32
}
pub(crate) fn fingerprint(data: &[u8], width: u32, height: u32, format: KittyImageFormat) -> u64 {
    let mut h = DefaultHasher::new();
    width.hash(&mut h);
    height.hash(&mut h);
    format.hash(&mut h);
    data.hash(&mut h);
    h.finish()
}
fn png_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if !data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut cursor = 8;
    let mut dimensions = None;
    let mut idat = false;
    let mut end = false;
    while cursor + 12 <= data.len() {
        let length = u32::from_be_bytes(data[cursor..cursor + 4].try_into().ok()?) as usize;
        let tail = cursor.checked_add(length)?.checked_add(12)?;
        if tail > data.len() {
            return None;
        }
        let chunk = &data[cursor + 4..tail - 4];
        let expected = u32::from_be_bytes(data[tail - 4..tail].try_into().ok()?);
        if crc32fast::hash(chunk) != expected {
            return None;
        }
        match &chunk[..4] {
            b"IHDR" if length == 13 => {
                let width = u32::from_be_bytes(chunk[4..8].try_into().ok()?);
                let height = u32::from_be_bytes(chunk[8..12].try_into().ok()?);
                if width == 0
                    || height == 0
                    || u64::from(width)
                        .checked_mul(u64::from(height))
                        .and_then(|n| n.checked_mul(4))
                        .is_none_or(|n| n > LIMIT as u64)
                {
                    return None;
                }
                dimensions = Some((width, height));
            }
            b"IDAT" => idat = true,
            b"IEND" => {
                end = true;
                break;
            }
            _ => {}
        }
        cursor = tail;
    }
    if idat && end {
        dimensions
    } else {
        None
    }
}
fn decode_png(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    #[cfg(test)]
    PNG_DECODE_CALLS.set(PNG_DECODE_CALLS.get() + 1);
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    decoder.set_limits(png::Limits { bytes: LIMIT });
    let mut reader = decoder.read_info().ok()?;
    if reader.output_buffer_size() > LIMIT {
        return None;
    }
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).ok()?;
    let bytes = &buffer[..info.buffer_size()];
    let mut rgba = Vec::new();
    match info.color_type {
        png::ColorType::Rgba => rgba.extend_from_slice(bytes),
        png::ColorType::Rgb => {
            for pixel in bytes.chunks_exact(3) {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
            }
        }
        png::ColorType::Grayscale => {
            for p in bytes {
                rgba.extend_from_slice(&[*p, *p, *p, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for p in bytes.chunks_exact(2) {
                rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
        }
        _ => return None,
    };
    Some((info.width, info.height, rgba))
}
include!("kitty_diacritics.rs");
fn diacritic(cp: u32) -> Option<u32> {
    DIACRITICS.binary_search(&cp).ok().map(|i| i as u32)
}
impl Terminal {
    pub fn enable_kitty_graphics(&mut self) -> Result<(), Error> {
        if !self.graphics.enabled {
            self.graphics.forwarding = cfg!(target_os = "linux");
            self.inactive_graphics.forwarding = cfg!(target_os = "linux");
        }
        self.graphics.enabled = true;
        self.inactive_graphics.enabled = true;
        Ok(())
    }
    pub fn set_kitty_source_forwarding(&mut self, enabled: bool) -> Result<(), Error> {
        self.graphics.forwarding = enabled;
        self.inactive_graphics.forwarding = enabled;
        self.graphics.png_forwarding = enabled;
        self.inactive_graphics.png_forwarding = enabled;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn set_kitty_png_forwarding(&mut self, enabled: bool) -> Result<(), Error> {
        self.graphics.png_forwarding = enabled;
        self.inactive_graphics.png_forwarding = enabled;
        Ok(())
    }
    pub fn kitty_graphics_generation(&self) -> Result<u64, Error> {
        Ok(self.graphics.generation)
    }
    pub fn kitty_graphics_may_have_placements(&self) -> Result<bool, Error> {
        Ok(self.graphics.enabled
            && self.kitty_empty_generation.get() != Some(self.graphics.generation))
    }
    pub fn kitty_image_fingerprints(&self, ids: &[u32]) -> Result<Vec<Option<u64>>, Error> {
        let cache = self.kitty_fingerprints.lock().map_err(|_| Error(-2))?;
        Ok(ids
            .iter()
            .map(|id| {
                cache
                    .get(id)
                    .filter(|entry| {
                        self.graphics.images.get(id).is_some_and(|image| {
                            image.generation == entry.generation
                                && image.fingerprint == entry.fingerprint
                        })
                    })
                    .map(|entry| entry.fingerprint)
            })
            .collect())
    }
    pub fn kitty_image_placements(&self) -> Result<Vec<KittyImagePlacement>, Error> {
        self.kitty_image_placements_with_data_filter(|_| true)
    }
    pub fn kitty_image_placements_with_data_filter<F: FnMut(KittyImageDescriptor) -> bool>(
        &self,
        needs_data: F,
    ) -> Result<Vec<KittyImagePlacement>, Error> {
        let placements = self.graphics.placements(self, needs_data);
        let mut cache = self.kitty_fingerprints.lock().map_err(|_| Error(-2))?;
        cache.retain(|id, _| self.graphics.images.contains_key(id));
        for p in &placements {
            if let Some(image) = self.graphics.images.get(&p.image_id) {
                cache.insert(
                    p.image_id,
                    FingerprintEntry {
                        generation: image.generation,
                        fingerprint: image.fingerprint,
                    },
                );
            }
        }
        if self.graphics.placements.is_empty() {
            self.kitty_empty_generation
                .set(Some(self.graphics.generation));
        }
        Ok(placements)
    }
}
