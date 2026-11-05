use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];
#[derive(Debug)]
pub(crate) struct FileEntry {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) is_dir: bool,
}
#[derive(Debug)]
pub(crate) struct FileBrowser {
    directory: PathBuf,
    entries: Vec<FileEntry>,
    cursor: usize,
    selected: HashSet<PathBuf>,
    error: Option<String>,
}
impl FileBrowser {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn new(directory: impl AsRef<Path>) -> Self {
        let directory = directory
            .as_ref()
            .canonicalize()
            .unwrap_or_else(|_| directory.as_ref().to_path_buf());
        let mut browser = Self {
            directory,
            entries: vec![],
            cursor: 0,
            selected: HashSet::new(),
            error: None,
        };
        browser.refresh();
        browser
    }
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }
    pub(crate) fn entries(&self) -> &[FileEntry] {
        &self.entries
    }
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }
    pub(crate) fn selected_count(&self) -> usize {
        self.selected.len()
    }
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub(crate) fn is_selected(&self, path: &Path) -> bool {
        self.selected.contains(path)
    }
    pub(crate) fn move_cursor(&mut self, offset: isize) {
        self.cursor = if self.entries.is_empty() {
            0
        } else {
            self.cursor
                .saturating_add_signed(offset)
                .min(self.entries.len() - 1)
        };
    }
    pub(crate) fn move_to_start(&mut self) {
        self.cursor = 0;
    }
    pub(crate) fn move_to_end(&mut self) {
        self.cursor = self.entries.len().saturating_sub(1);
    }
    pub(crate) fn activate(&mut self) {
        let Some(entry) = self.entries.get(self.cursor) else {
            return;
        };
        if entry.is_dir {
            self.directory = entry.path.clone();
            self.cursor = 0;
            self.refresh();
        } else {
            self.toggle_selected();
        }
    }
    pub(crate) fn toggle_selected(&mut self) {
        let Some(entry) = self.entries.get(self.cursor).filter(|e| !e.is_dir) else {
            return;
        };
        if !self.selected.remove(&entry.path) {
            self.selected.insert(entry.path.clone());
        }
    }
    pub(crate) fn go_to_parent(&mut self) {
        let Some(parent) = self.directory.parent() else {
            return;
        };
        self.directory = parent.to_path_buf();
        self.cursor = 0;
        self.refresh();
    }
    pub(crate) fn selected_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<_> = self.selected.iter().cloned().collect();
        paths.sort();
        paths
    }
    fn refresh(&mut self) {
        self.entries.clear();
        self.error = None;
        match fs::read_dir(&self.directory) {
            Ok(entries) => {
                self.entries = entries
                    .filter_map(Result::ok)
                    .filter_map(|entry| {
                        let path = entry.path();
                        let is_dir = path.is_dir();
                        (is_dir || is_image(&path)).then(|| FileEntry {
                            name: entry.file_name().to_string_lossy().into_owned(),
                            path,
                            is_dir,
                        })
                    })
                    .collect();
                self.entries.sort_by(|a, b| {
                    b.is_dir
                        .cmp(&a.is_dir)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                        .then_with(|| a.name.cmp(&b.name))
                });
                self.cursor = self.cursor.min(self.entries.len().saturating_sub(1));
            }
            Err(e) => {
                self.cursor = 0;
                self.error = Some(e.to_string());
            }
        }
    }
}
fn is_image(path: &Path) -> bool {
    path.is_file()
        && path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|candidate| e.eq_ignore_ascii_case(candidate))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("fae-browser-{}", wd_tools::uuid::v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }
    #[test]
    fn browser_lists_directories_and_images_only() {
        let root = TestDir::new();
        fs::create_dir(root.0.join("folder")).unwrap();
        fs::write(root.0.join("photo.PNG"), b"image").unwrap();
        fs::write(root.0.join("notes.txt"), b"text").unwrap();
        let browser = FileBrowser::new(&root.0);
        let entries = browser.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "folder");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].name, "photo.PNG");
    }
    #[test]
    fn selection_is_preserved_across_directories() {
        let root = TestDir::new();
        let nested = root.0.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(root.0.join("root.png"), b"image").unwrap();
        fs::write(nested.join("nested.jpg"), b"image").unwrap();
        let mut browser = FileBrowser::new(&root.0);
        browser.move_cursor(1);
        browser.toggle_selected();
        browser.move_to_start();
        browser.activate();
        browser.toggle_selected();
        let selected = browser.selected_paths();
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().any(|p| p.ends_with("root.png")));
        assert!(selected.iter().any(|p| p.ends_with("nested.jpg")));
    }
}
