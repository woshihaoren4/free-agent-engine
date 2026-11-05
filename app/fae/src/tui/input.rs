use unicode_width::UnicodeWidthStr;

#[derive(Debug, Default)]
pub(super) struct Composer {
    pub(super) text: String,
    pub(super) cursor: usize,
    pub(super) scroll_back: u16,
}

impl Composer {
    pub(super) fn insert(&mut self, value: &str) {
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
        self.follow_cursor();
    }
    pub(super) fn backspace(&mut self) {
        if let Some(previous) = self.text[..self.cursor].char_indices().next_back() {
            self.text.drain(previous.0..self.cursor);
            self.cursor = previous.0;
            self.follow_cursor();
        }
    }
    pub(super) fn delete(&mut self) {
        if let Some(next) = self.text[self.cursor..].chars().next() {
            self.text.drain(self.cursor..self.cursor.saturating_add(next.len_utf8()));
            self.follow_cursor();
        }
    }
    pub(super) fn move_left(&mut self) {
        if let Some(previous) = self.text[..self.cursor].char_indices().next_back() {
            self.cursor = previous.0;
            self.follow_cursor();
        }
    }
    pub(super) fn move_right(&mut self) {
        if let Some(next) = self.text[self.cursor..].chars().next() {
            self.cursor += next.len_utf8();
            self.follow_cursor();
        }
    }
    pub(super) fn move_home(&mut self) {
        self.cursor = self.text[..self.cursor].rfind('\n').map(|position| position + 1).unwrap_or(0);
        self.follow_cursor();
    }
    pub(super) fn move_end(&mut self) {
        self.cursor = self.text[self.cursor..].find('\n').map(|offset| self.cursor + offset).unwrap_or(self.text.len());
        self.follow_cursor();
    }
    pub(super) fn take(&mut self) -> Option<String> {
        let value = self.text.trim().to_string();
        self.text.clear();
        self.cursor = 0;
        self.follow_cursor();
        (!value.is_empty()).then_some(value)
    }
    pub(super) fn visual_cursor(&self, width: usize) -> (u16, u16) {
        let width = width.max(1);
        let (mut row, mut column) = (0usize, 0usize);
        for character in self.text[..self.cursor].chars() {
            if character == '\n' { row += 1; column = 0; continue; }
            let character_width = character.to_string().width().max(1);
            if column + character_width > width { row += 1; column = 0; }
            column += character_width;
            if column == width { row += 1; column = 0; }
        }
        (column as u16, row as u16)
    }
    pub(super) fn visual_lines(&self, width: usize) -> usize {
        let width = width.max(1);
        self.text.split('\n').map(|line| line.width().max(1).div_ceil(width)).sum()
    }
    pub(super) fn vertical_scroll(&self, width: usize, visible_height: u16) -> u16 {
        let follow_scroll = self.max_vertical_scroll(width, visible_height);
        follow_scroll.saturating_sub(self.scroll_back.min(follow_scroll))
    }
    pub(super) fn max_vertical_scroll(&self, width: usize, visible_height: u16) -> u16 {
        let (_, cursor_row) = self.visual_cursor(width);
        cursor_row.saturating_sub(visible_height.saturating_sub(1))
    }
    pub(super) fn scroll_up(&mut self, lines: u16, max_scroll: u16) {
        self.scroll_back = self.scroll_back.saturating_add(lines).min(max_scroll);
    }
    pub(super) fn scroll_down(&mut self, lines: u16) {
        self.scroll_back = self.scroll_back.saturating_sub(lines);
    }
    pub(super) fn follow_cursor(&mut self) { self.scroll_back = 0; }
}

pub(super) fn parse_steer(input: &str) -> Option<&str> {
    let content = input.strip_prefix("/steer")?;
    if !content.starts_with(char::is_whitespace) { return None; }
    let content = content.trim();
    (!content.is_empty()).then_some(content)
}
