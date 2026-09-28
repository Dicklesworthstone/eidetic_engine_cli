//! Read-only recovery of unfinished goals, independent of the memory snapshot.
//!
//! A frame remains recorded work, not an adopted task or an executable plan.
//! Never replay its suggested commands, actor, source, or stored contract.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use super::{ResumeRedactionPosture, parse_ts, public_resume_text, shell_quote_cli_arg};
use crate::core::task_frame::{
    NON_EXECUTING_CONTRACT, TASK_FRAME_ID_PREFIX, TASK_FRAME_SCHEMA_V1, TASK_SUBGOAL_ID_PREFIX,
    TaskFrameRecord, TaskFrameStatus, TaskSubgoal, read_task_frames_for_resume,
};

const FRAME_CAP: usize = 8;
const SUBGOAL_CAP: usize = 8;
const BLOCKER_CAP: usize = 4;
const TEXT_CHAR_CAP: usize = 240;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeTaskStatus {
    Empty,
    Available,
    Partial,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeTaskBlockers {
    pub total: usize,
    pub truncated: bool,
    pub items: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeTaskSubgoal {
    pub id: String,
    pub selection_reason: &'static str,
    pub parent_id: Option<String>,
    pub title: String,
    pub status: TaskFrameStatus,
    pub updated_at: String,
    pub blockers: ResumeTaskBlockers,
    pub text_truncated: bool,
    pub redaction: ResumeRedactionPosture,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeTaskFrame {
    pub id: String,
    pub selection_reason: &'static str,
    pub root_goal: String,
    pub status: TaskFrameStatus,
    pub current_focus: Option<String>,
    pub updated_at: String,
    pub blockers: ResumeTaskBlockers,
    pub active_subgoals_total: usize,
    pub subgoals_truncated: bool,
    /// Flat, bounded view. A parent may exist outside this response page.
    pub subgoals: Vec<ResumeTaskSubgoal>,
    pub text_truncated: bool,
    pub redaction: ResumeRedactionPosture,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeTaskFrames {
    pub schema: &'static str,
    pub source: &'static str,
    pub status: ResumeTaskStatus,
    /// File state is NOT covered by the database's read transaction.
    pub snapshot_scope: &'static str,
    pub non_executing_contract: &'static str,
    /// None means the store could not be read, not that it was empty.
    pub active_total: Option<usize>,
    /// Unfinished records withheld for invalid identity, scope or structure.
    pub excluded_total: Option<usize>,
    pub truncated: bool,
    /// Multiple frames must not be implicitly treated as one active goal.
    pub selection_required: bool,
    pub frames: Vec<ResumeTaskFrame>,
    /// Binary-owned diagnostic only; never include a parser's private input.
    pub degraded_code: Option<&'static str>,
}

impl Default for ResumeTaskFrames {
    fn default() -> Self {
        Self {
            schema: "ee.resume.task_frames.v1",
            source: ".ee/task_frames.json",
            status: ResumeTaskStatus::Empty,
            snapshot_scope: "independent_task_frame_file",
            non_executing_contract: NON_EXECUTING_CONTRACT,
            active_total: Some(0),
            excluded_total: Some(0),
            truncated: false,
            selection_required: false,
            frames: Vec::new(),
            degraded_code: None,
        }
    }
}

impl ResumeTaskFrames {
    /// Only reconstruct read-only commands from validated IDs and the caller's
    /// addressed workspace. No command from the store reaches this surface.
    pub(super) fn next_commands(&self, workspace: &Path) -> Vec<String> {
        let workspace = shell_quote_cli_arg(&workspace.to_string_lossy());
        let mut commands = Vec::new();
        if self.degraded_code.is_some() {
            commands.push(format!(
                "ee task-frame show --workspace {workspace} --json  # inspect incomplete task recovery"
            ));
        }
        for frame in self
            .frames
            .iter()
            .take(2usize.saturating_sub(commands.len()))
        {
            commands.push(format!(
                "ee task-frame show {} --workspace {workspace} --json  # recorded unfinished work; not automatically adopted",
                frame.id
            ));
        }
        commands
    }
}

pub(super) fn load(workspace: &Path) -> ResumeTaskFrames {
    // Do not canonicalize before the existing reader's no-symlink checks.
    let frames = match read_task_frames_for_resume(workspace) {
        Ok(frames) => frames,
        Err(_) => {
            return ResumeTaskFrames {
                status: ResumeTaskStatus::Unavailable,
                active_total: None,
                excluded_total: None,
                degraded_code: Some("resume_task_frames_unavailable"),
                ..ResumeTaskFrames::default()
            };
        }
    };
    // Resolve only caller-supplied paths. Never stat an untrusted frame path.
    let mut roots = BTreeSet::from([workspace.to_string_lossy().into_owned()]);
    if let Ok(canonical) = workspace.canonicalize() {
        roots.insert(canonical.to_string_lossy().into_owned());
    }
    project(frames, &roots)
}

fn valid_id(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix).is_some_and(|payload| {
        payload.len() == 26
            && payload
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn unfinished(status: TaskFrameStatus, closed_at: &Option<String>) -> bool {
    status.is_active_candidate() && closed_at.is_none()
}

fn valid_frame(frame: &TaskFrameRecord, roots: &BTreeSet<String>) -> bool {
    if frame.schema != TASK_FRAME_SCHEMA_V1
        || !valid_id(&frame.id, TASK_FRAME_ID_PREFIX)
        || !roots.contains(&frame.workspace_root)
        || frame.root_goal.trim().is_empty()
        || parse_ts(&frame.created_at).is_none()
        || parse_ts(&frame.updated_at).is_none()
        || frame.closed_at.is_some()
    {
        return false;
    }
    let mut parents: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    for subgoal in &frame.subgoals {
        if !valid_id(&subgoal.id, TASK_SUBGOAL_ID_PREFIX)
            || subgoal.title.trim().is_empty()
            || parse_ts(&subgoal.created_at).is_none()
            || parse_ts(&subgoal.updated_at).is_none()
            || (subgoal.status.is_active_candidate() && subgoal.closed_at.is_some())
            || parents
                .insert(subgoal.id.as_str(), subgoal.parent_id.as_deref())
                .is_some()
        {
            return false;
        }
    }
    // Iterative, memoized traversal: reject orphan/cyclic goal stacks without
    // recursion or quadratic work on a long chain in the bounded source file.
    let mut complete = BTreeSet::new();
    for start in parents.keys().copied() {
        let mut path = BTreeSet::new();
        let mut next = Some(start);
        while let Some(id) = next {
            if complete.contains(id) {
                break;
            }
            if !path.insert(id) {
                return false;
            }
            let Some(parent) = parents.get(id) else {
                return false;
            };
            next = *parent;
        }
        complete.extend(path);
    }
    true
}

fn priority(status: TaskFrameStatus) -> u8 {
    match status {
        TaskFrameStatus::Active => 0,
        TaskFrameStatus::Blocked => 1,
        _ => 2,
    }
}

fn project(frames: Vec<TaskFrameRecord>, roots: &BTreeSet<String>) -> ResumeTaskFrames {
    // Reject every occurrence of an ambiguous ID, including collisions with
    // completed/draft rows; do not make array order decide which task is real.
    let mut identities = BTreeMap::new();
    for frame in &frames {
        *identities.entry(frame.id.clone()).or_insert(0usize) += 1;
    }
    let mut admitted = Vec::new();
    let mut excluded = 0;
    for frame in frames {
        if !frame.status.is_active_candidate() {
            continue;
        }
        if identities.get(&frame.id) != Some(&1) || !valid_frame(&frame, roots) {
            excluded += 1;
            continue;
        }
        admitted.push(frame);
    }
    admitted.sort_by(|left, right| {
        priority(left.status)
            .cmp(&priority(right.status))
            .then_with(|| parse_ts(&right.updated_at).cmp(&parse_ts(&left.updated_at)))
            .then_with(|| left.id.cmp(&right.id))
    });
    let total = admitted.len();
    ResumeTaskFrames {
        status: if excluded > 0 {
            ResumeTaskStatus::Partial
        } else if total == 0 {
            ResumeTaskStatus::Empty
        } else {
            ResumeTaskStatus::Available
        },
        active_total: Some(total),
        excluded_total: Some(excluded),
        truncated: total > FRAME_CAP,
        selection_required: total > 1,
        frames: admitted
            .into_iter()
            .take(FRAME_CAP)
            .map(project_frame)
            .collect(),
        degraded_code: (excluded > 0).then_some("resume_task_frames_excluded"),
        ..ResumeTaskFrames::default()
    }
}

fn text(raw: &str, field: &str, reasons: &mut Vec<String>, truncated: &mut bool) -> String {
    // Screen the entire field BEFORE truncation, otherwise a secret straddling
    // the output boundary could evade the canonical public-egress detector.
    let safe = public_resume_text(raw, field, reasons);
    if safe.chars().count() > TEXT_CHAR_CAP {
        *truncated = true;
        safe.chars().take(TEXT_CHAR_CAP).collect()
    } else {
        safe
    }
}

fn blockers(
    raw: &[String],
    reasons: &mut Vec<String>,
    text_truncated: &mut bool,
) -> ResumeTaskBlockers {
    ResumeTaskBlockers {
        total: raw.len(),
        truncated: raw.len() > BLOCKER_CAP,
        items: raw
            .iter()
            .take(BLOCKER_CAP)
            .map(|value| text(value, "task.blocker", reasons, text_truncated))
            .collect(),
    }
}

fn redaction(mut reasons: Vec<String>) -> ResumeRedactionPosture {
    reasons.sort();
    reasons.dedup();
    ResumeRedactionPosture {
        applied: !reasons.is_empty(),
        reasons,
    }
}

fn project_subgoal(subgoal: TaskSubgoal) -> ResumeTaskSubgoal {
    let mut reasons = Vec::new();
    let mut truncated = false;
    ResumeTaskSubgoal {
        id: subgoal.id,
        selection_reason: "unfinished_subgoal",
        parent_id: subgoal.parent_id,
        title: text(&subgoal.title, "task.subgoal", &mut reasons, &mut truncated),
        status: subgoal.status,
        updated_at: subgoal.updated_at,
        blockers: blockers(&subgoal.blockers, &mut reasons, &mut truncated),
        text_truncated: truncated,
        redaction: redaction(reasons),
    }
}

fn project_frame(frame: TaskFrameRecord) -> ResumeTaskFrame {
    let mut reasons = Vec::new();
    let mut truncated = false;
    let mut subgoals: Vec<_> = frame
        .subgoals
        .into_iter()
        .filter(|row| unfinished(row.status, &row.closed_at))
        .collect();
    subgoals.sort_by(|left, right| {
        priority(left.status)
            .cmp(&priority(right.status))
            .then_with(|| parse_ts(&right.updated_at).cmp(&parse_ts(&left.updated_at)))
            .then_with(|| left.id.cmp(&right.id))
    });
    ResumeTaskFrame {
        id: frame.id,
        selection_reason: "unfinished_task_frame",
        root_goal: text(&frame.root_goal, "task.goal", &mut reasons, &mut truncated),
        status: frame.status,
        current_focus: frame
            .current_focus
            .as_deref()
            .map(|value| text(value, "task.focus", &mut reasons, &mut truncated)),
        updated_at: frame.updated_at,
        blockers: blockers(&frame.blockers, &mut reasons, &mut truncated),
        active_subgoals_total: subgoals.len(),
        subgoals_truncated: subgoals.len() > SUBGOAL_CAP,
        subgoals: subgoals
            .into_iter()
            .take(SUBGOAL_CAP)
            .map(project_subgoal)
            .collect(),
        text_truncated: truncated,
        redaction: redaction(reasons),
    }
}

#[cfg(test)]
#[path = "resume_tasks_tests.rs"]
mod tests;
