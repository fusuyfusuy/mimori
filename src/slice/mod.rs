use crate::model::SliceResult;
use crate::storage::get_or_sync_graph;
use anyhow::Result;
use std::path::Path;

pub fn execute_slice(target: &str, follow_local: bool, with_imports: bool) -> Result<SliceResult> {
    let root = if let Some((target_file, _)) = target.split_once(':') {
        let p = Path::new(target_file);
        if p.is_absolute() {
            p.parent().unwrap_or_else(|| Path::new(".")).to_path_buf()
        } else {
            Path::new(".").to_path_buf()
        }
    } else {
        Path::new(".").to_path_buf()
    };

    let graph = get_or_sync_graph(&root)?;
    graph.build_slice(target, follow_local, with_imports)
}
