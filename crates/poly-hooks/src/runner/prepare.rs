//! Pre-execution planning: resolve each hook's matched file set and its
//! file-based skip decision, and group hooks into priority bands.

use std::path::{Path, PathBuf};

use crate::filter::{FileTagCache, HookFileFilter};
use crate::model::{Hook, HookRunRequest, SkipReason, StageSpec};
use crate::stage::RunInputMode;

/// A hook's resolved file set and skip decision, computed before execution.
pub(super) struct Prepared {
    /// Files this hook will receive (already filtered by name and tag).
    pub(super) matched: Vec<PathBuf>,
    /// Set when the hook is skipped before it is ever executed.
    pub(super) skip: Option<SkipReason>,
}

pub(super) fn prepare(request: &HookRunRequest, spec: &StageSpec) -> Vec<Prepared> {
    let all_paths: Vec<&Path> = request.files.iter().map(AsRef::as_ref).collect();
    let tag_cache = FileTagCache::from_paths(all_paths.iter().copied());
    spec.hooks
        .iter()
        .map(|hook| prepare_one(request, hook, &all_paths, &tag_cache))
        .collect()
}

fn prepare_one(request: &HookRunRequest, hook: &Hook, all_paths: &[&Path], tag_cache: &FileTagCache<'_>) -> Prepared {
    match RunInputMode::from(hook.stage) {
        RunInputMode::NoFiles => Prepared {
            matched: Vec::new(),
            skip: None,
        },
        RunInputMode::MessageFile => Prepared {
            matched: request.message_file.iter().cloned().collect(),
            skip: None,
        },
        RunInputMode::Files => {
            let filter = HookFileFilter::new(
                hook.files.as_ref(),
                hook.exclude.as_ref(),
                hook.types.as_ref(),
                hook.types_or.as_ref(),
                hook.exclude_types.as_ref(),
            );
            let has_tag_filter = hook.types.is_some() || hook.types_or.is_some() || hook.exclude_types.is_some();
            let matched: Vec<PathBuf> = all_paths
                .iter()
                .filter(|path| {
                    filter.matches_filename(path) && (!has_tag_filter || filter.matches_tags(tag_cache.tags_for(path)))
                })
                .map(|path| path.to_path_buf())
                .collect();
            let skip = if matched.is_empty() && !hook.always_run {
                Some(SkipReason::NoFiles)
            } else {
                None
            };
            Prepared { matched, skip }
        }
    }
}

/// Group hook positions by `priority` (ascending), preserving original order
/// within a group.
pub(super) fn group_by_priority(hooks: &[Hook]) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..hooks.len()).collect();
    order.sort_by_key(|&pos| hooks[pos].priority);

    let mut groups: Vec<Vec<usize>> = Vec::new();
    for pos in order {
        match groups.last_mut() {
            Some(group) if hooks[group[0]].priority == hooks[pos].priority => group.push(pos),
            _ => groups.push(vec![pos]),
        }
    }
    groups
}
