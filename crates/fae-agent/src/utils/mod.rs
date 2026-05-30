use std::path::{Component, Path, PathBuf};

pub fn path_clean(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let mut result = PathBuf::new();

    for comp in path.components() {
        match comp {
            Component::CurDir => {
                // 跳过 "."
            }
            Component::ParentDir => {
                // 如果前面有正常目录，就回退一层
                // 如果没有，就保留 ".."
                if !result.pop() {
                    result.push("..");
                }
            }
            Component::RootDir => {
                result.push(comp.as_os_str());
            }
            Component::Prefix(prefix) => {
                result.push(prefix.as_os_str());
            }
            Component::Normal(c) => {
                result.push(c);
            }
        }
    }

    if result.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        result
    }
}
