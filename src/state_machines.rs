//! The app's statum machines as graphs (statum's `introspection`): each
//! transition's legal targets, asserted, and docs/state-machines.md written
//! from them, to compare with the TLA+ models in verify/. `cargo xtask
//! state-machines` writes it; `cargo xtask check` fails while it's stale.
use statum::StableGraphMetadata;
use std::path::Path;

/// Where the graphs go, between this crate's markers.
const DOC: &str = "docs/state-machines.md";
const BEGIN: &str = "<!-- kindil: begin -->\n";
const END: &str = "<!-- kindil: end -->\n";

/// Each transition, `From.method -> To | Other`, in a stable order.
fn edges(graph: &StableGraphMetadata) -> Vec<String> {
    let mut edges: Vec<String> = graph
        .transitions
        .iter()
        .map(|t| {
            format!(
                "{}.{} -> {}",
                t.from_state,
                t.method_name,
                t.to_states.join(" | ")
            )
        })
        .collect();
    edges.sort();
    edges
}

/// A machine's section: its states, a Mermaid diagram and its edges. The
/// registry's order is the link order: sorted, so the file is stable.
fn section(title: &str, about: &str, graph: &StableGraphMetadata) -> String {
    let mut sorted = graph.clone();
    sorted
        .transitions
        .sort_by(|a, b| (&a.from_state, &a.method_name).cmp(&(&b.from_state, &b.method_name)));
    let states: Vec<&str> = graph.states.iter().map(|s| s.rust_name.as_str()).collect();
    format!(
        "## {title}\n\n{about}\n\n`{}`: {}.\n\n```mermaid\n{}```\n\n```text\n{}\n```\n\n",
        graph.machine.rust_type_path,
        states.join(", "),
        sorted.to_mermaid_state_diagram(),
        edges(graph).join("\n")
    )
}

fn machines() -> Vec<(&'static str, &'static str, StableGraphMetadata)> {
    vec![
        (
            "kindil install",
            "src/install.rs: a release installs itself, and rolls back unless it comes up and holds.",
            crate::install::graph(),
        ),
        (
            "Over-the-air update",
            "src/update.rs: kindil's side of an update; the launcher runs the trial (below).",
            crate::update::graph(),
        ),
        (
            "USB drive mode",
            "src/drive.rs: the user storage to a computer and back.",
            crate::drive::graph(),
        ),
    ]
}

#[test]
fn each_transition_has_exactly_its_legal_targets() {
    let want = [
        &[
            "BackedUp.stop -> Stopped",
            "Checked.back_up -> BackedUp",
            "Failed.roll_back -> RolledBack",
            "Placed.start -> Started",
            "Started.come_up -> Up | Failed",
            "Stopped.place -> Placed | Failed",
            "Up.hold -> Kept | Failed",
        ][..],
        &[
            "Available.download -> Verified",
            "Available.verify -> Verified",
            "Staged.roll_back -> RolledBack",
            "Trying.commit -> Kept",
            "Trying.roll_back -> RolledBack",
            "Verified.stage -> Staged",
        ],
        &[
            "Checked.mount -> Remounted",
            "Exported.release -> Released",
            "Free.load_gadget -> GadgetIn",
            "GadgetIn.export -> Exported",
            "Mounted.unmount -> Unmounted",
            "Released.check -> Checked",
            "Unmounted.verify -> Free",
        ],
    ];
    for ((title, _, graph), want) in machines().iter().zip(want) {
        assert_eq!(edges(graph), want, "{title}");
    }
}

/// Nothing is kept but through its trial or hold, and every failure after
/// the point of no return has a way back.
#[test]
fn kept_only_after_its_trial_and_rollback_always_reachable() {
    let into = |graph: &StableGraphMetadata, state: &str| -> Vec<String> {
        graph
            .transitions
            .iter()
            .filter(|t| t.to_states.iter().any(|s| s == state))
            .map(|t| format!("{}.{}", t.from_state, t.method_name))
            .collect()
    };
    assert_eq!(into(&crate::install::graph(), "Kept"), ["Up.hold"]);
    assert_eq!(into(&crate::update::graph(), "Kept"), ["Trying.commit"]);
    let mut back = into(&crate::update::graph(), "RolledBack");
    back.sort();
    assert_eq!(back, ["Staged.roll_back", "Trying.roll_back"]);
    // The LUN is written on one edge, from the state seen unmounted.
    assert_eq!(
        into(&crate::drive::graph(), "Exported"),
        ["GadgetIn.export"]
    );
}

#[test]
fn docs_state_machines_is_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(DOC);
    let mut block = String::new();
    for (title, about, graph) in machines() {
        block.push_str(&section(title, about, &graph));
    }
    let doc = std::fs::read_to_string(&path).unwrap_or_default();
    let ours = |doc: &str| -> Option<(usize, usize)> {
        let start = doc.find(BEGIN)? + BEGIN.len();
        Some((start, start + doc[start..].find(END)?))
    };
    let Some((start, end)) = ours(&doc) else {
        panic!("{DOC} lacks {BEGIN:?} and {END:?}");
    };
    if std::env::var_os("KINDIL_WRITE_STATE_MACHINES").is_some() {
        let written = format!("{}{block}{}", &doc[..start], &doc[end..]);
        std::fs::write(&path, written).unwrap();
    } else {
        assert!(
            doc[start..end] == block,
            "{DOC} is out of date: cargo xtask state-machines"
        );
    }
}
