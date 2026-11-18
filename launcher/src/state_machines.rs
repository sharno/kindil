//! The launcher's statum machines as graphs: each transition's legal
//! targets, asserted, and their part of docs/state-machines.md (see
//! src/state_machines.rs in the app, whose helpers these repeat: the
//! launcher shares no code with it). Only the tests build statum's
//! `introspection`; the launcher itself has no registry.
use statum::StableGraphMetadata;
use std::path::Path;

const DOC: &str = "../docs/state-machines.md";
const BEGIN: &str = "<!-- launcher: begin -->\n";
const END: &str = "<!-- launcher: end -->\n";

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
    let [session, trial] = crate::session::graphs();
    vec![
        (
            "Launcher session",
            "launcher/src/session.rs: Amazon's UI steps aside while kindil runs, and always comes back.",
            session,
        ),
        (
            "Update trial (launcher)",
            "launcher/src/session.rs: a staged update's one trial run; kindil keeps it (above).",
            trial,
        ),
    ]
}

#[test]
fn each_transition_has_exactly_its_legal_targets() {
    let want = [
        &[
            "Aside.restore -> Restored",
            "Ready.step_aside -> Aside",
            "Starting.pick -> Ready",
        ][..],
        &["Staged.begin -> Trying | Idle", "Trying.roll_back -> Idle"],
    ];
    for ((title, _, graph), want) in machines().iter().zip(want) {
        assert_eq!(edges(graph), want, "{title}");
    }
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
