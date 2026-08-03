//! PipeWire application routing (Linux only).
//!
//! ZDL-Echo's TX/RX audio itself still goes through `cpal` (see `audio.rs`),
//! same as always. This module is a *separate* control-plane connection to
//! the PipeWire graph that only watches the registry for other applications'
//! audio streams (Firefox, Discord, a SIP softphone, ...) and creates/removes
//! `pw_link` objects between ZDL-Echo's own ALSA-shim ports and theirs. That
//! lets a specific application be picked as a TX target or RX source from
//! inside ZDL-Echo, without an external patchbay (qpwgraph/pw-link).
//!
//! Everything here lives on one dedicated OS thread and never crosses a
//! pipewire-rs object to another thread — those types are `!Send` (they wrap
//! `Rc`-managed C objects tied to the connection they were created on).
//! Communication in is via `pipewire::channel` (a self-pipe the mainloop can
//! poll); communication out reuses the existing `crossbeam_channel` back to
//! the UI, carrying only plain owned data (`AppMessage`).

use crate::types::{AppMessage, PwCommand, SoftwareApp};
use crossbeam_channel::Sender;
use pipewire as pw;
use pw::properties::properties;
use pw::types::ObjectType;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PortDir {
    In,
    Out,
}

struct TrackedNode {
    label: String,
    dir: PortDir,
}

struct PortInfo {
    node_id: u32,
    dir: PortDir,
}

#[derive(Default)]
struct State {
    /// Other applications' audio-stream nodes (never our own).
    nodes: HashMap<u32, TrackedNode>,
    ports: HashMap<u32, PortInfo>,
    /// Our own `alsa_playback.ZDL-Echo` node (Stream/Output/Audio) — the
    /// source side for injecting tones into another app.
    own_tx_node: Option<u32>,
    /// Our own `alsa_capture.ZDL-Echo` node (Stream/Input/Audio) — the
    /// destination side for decoding another app's audio.
    own_rx_node: Option<u32>,
    link_factory: Option<String>,
    tx_links: HashMap<String, Vec<pw::link::Link>>,
    rx_links: HashMap<String, Vec<pw::link::Link>>,
}

fn find_node(nodes: &HashMap<u32, TrackedNode>, label: &str, dir: PortDir) -> Option<u32> {
    nodes
        .iter()
        .find(|(_, n)| n.label == label && n.dir == dir)
        .map(|(&id, _)| id)
}

fn ports_of(ports: &HashMap<u32, PortInfo>, node_id: u32, dir: PortDir) -> Vec<u32> {
    let mut out: Vec<u32> = ports
        .iter()
        .filter(|(_, p)| p.node_id == node_id && p.dir == dir)
        .map(|(&id, _)| id)
        .collect();
    out.sort_unstable();
    out
}

/// Pair up source and destination ports. When one side has fewer ports than
/// the other, its ports are cycled so every destination port gets fed —
/// duplicating a mono source across a stereo destination is fine, but never
/// fan a longer source into a shorter destination (that would sum multiple
/// signals into one port and double the level), so the source side is only
/// ever cycled, never truncated onto a shared destination port.
fn pair_ports(src: &[u32], dst: &[u32]) -> Vec<(u32, u32)> {
    if src.is_empty() || dst.is_empty() {
        return Vec::new();
    }
    if src.len() >= dst.len() {
        src.iter().zip(dst.iter()).map(|(&s, &d)| (s, d)).collect()
    } else {
        src.iter().cycle().zip(dst.iter()).map(|(&s, &d)| (s, d)).collect()
    }
}

fn create_links(
    core: &pw::core::CoreRc,
    factory: &str,
    out_node: u32,
    out_ports: &[u32],
    in_node: u32,
    in_ports: &[u32],
) -> Vec<pw::link::Link> {
    pair_ports(out_ports, in_ports)
        .into_iter()
        .filter_map(|(op, ip)| {
            core.create_object::<pw::link::Link>(
                factory,
                &properties! {
                    "link.output.node" => out_node.to_string(),
                    "link.output.port" => op.to_string(),
                    "link.input.node" => in_node.to_string(),
                    "link.input.port" => ip.to_string(),
                },
            )
            .ok()
        })
        .collect()
}

fn send_snapshot(st: &State, tx_ui: &Sender<AppMessage>) {
    let mut agg: BTreeMap<String, (bool, bool)> = BTreeMap::new();
    for n in st.nodes.values() {
        let e = agg.entry(n.label.clone()).or_insert((false, false));
        match n.dir {
            // has a capture/mic-input stream -> we can inject TX into it
            PortDir::In => e.0 = true,
            // has a playback/speaker stream -> we can decode RX from it
            PortDir::Out => e.1 = true,
        }
    }
    let apps = agg
        .into_iter()
        .map(|(label, (can_tx, can_rx))| {
            let linked_tx = st.tx_links.contains_key(&label);
            let linked_rx = st.rx_links.contains_key(&label);
            SoftwareApp { label, can_tx, can_rx, linked_tx, linked_rx }
        })
        .collect();
    let _ = tx_ui.send(AppMessage::SoftwareApps(apps));
}

/// A node is "ours" if the ALSA/PipeWire shim named it after our own binary.
/// Case-insensitive because the installed package renames the binary to
/// lowercase `zdl-echo` while a `cargo build` produces `ZDL-Echo`.
fn is_own_node(node_name: &str) -> bool {
    node_name.to_ascii_lowercase().contains("zdl-echo")
}

fn on_global(
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
    state: &Rc<RefCell<State>>,
    tx_ui: &Sender<AppMessage>,
) {
    let Some(props) = global.props else { return };
    match global.type_ {
        ObjectType::Node => {
            let dir = match props.get("media.class") {
                Some("Stream/Output/Audio") => PortDir::Out,
                Some("Stream/Input/Audio") => PortDir::In,
                _ => return,
            };
            let node_name = props.get("node.name").unwrap_or("");
            if is_own_node(node_name) {
                let mut st = state.borrow_mut();
                match dir {
                    PortDir::Out => st.own_tx_node = Some(global.id),
                    PortDir::In => st.own_rx_node = Some(global.id),
                }
                send_snapshot(&st, tx_ui);
                return;
            }
            // Skip nodes with no application identity (WirePlumber's
            // role-based loopback sinks, etc.) — they aren't "apps" a user
            // would recognize or want to pick from a list.
            let Some(label) = props
                .get("application.process.binary")
                .or_else(|| props.get("application.name"))
                .map(str::to_string)
            else {
                return;
            };
            let mut st = state.borrow_mut();
            st.nodes.insert(global.id, TrackedNode { label, dir });
            send_snapshot(&st, tx_ui);
        }
        ObjectType::Port => {
            let Some(node_id) = props.get("node.id").and_then(|s| s.parse::<u32>().ok()) else {
                return;
            };
            let dir = match props.get("port.direction") {
                Some("out") => PortDir::Out,
                Some("in") => PortDir::In,
                _ => return,
            };
            state.borrow_mut().ports.insert(global.id, PortInfo { node_id, dir });
        }
        ObjectType::Factory => {
            if props.get("factory.type.name") == Some(ObjectType::Link.to_str())
                && let Some(name) = props.get("factory.name")
            {
                state.borrow_mut().link_factory = Some(name.to_string());
            }
        }
        _ => {}
    }
}

fn on_global_remove(id: u32, state: &Rc<RefCell<State>>, tx_ui: &Sender<AppMessage>) {
    let mut st = state.borrow_mut();
    st.ports.remove(&id);
    // Links die with either endpoint server-side; drop our stale proxies too
    // instead of leaving the UI showing a "connected" app that's gone.
    if st.own_tx_node == Some(id) {
        st.own_tx_node = None;
        st.tx_links.clear();
    }
    if st.own_rx_node == Some(id) {
        st.own_rx_node = None;
        st.rx_links.clear();
    }
    if let Some(n) = st.nodes.remove(&id) {
        match n.dir {
            PortDir::In => {
                st.tx_links.remove(&n.label);
            }
            PortDir::Out => {
                st.rx_links.remove(&n.label);
            }
        }
    }
    send_snapshot(&st, tx_ui);
}

fn handle_command(cmd: PwCommand, core: &pw::core::CoreRc, state: &Rc<RefCell<State>>, tx_ui: &Sender<AppMessage>) {
    let mut st = state.borrow_mut();
    match cmd {
        PwCommand::LinkTx(label) => {
            let Some(own_tx) = st.own_tx_node else {
                let _ = tx_ui.send(AppMessage::AudioError(
                    "software TX not ready: ZDL-Echo's own output stream hasn't registered with PipeWire yet".into(),
                ));
                return;
            };
            let Some(target) = find_node(&st.nodes, &label, PortDir::In) else {
                let _ = tx_ui.send(AppMessage::AudioError(format!("{label}: no input stream to inject into")));
                return;
            };
            let out_ports = ports_of(&st.ports, own_tx, PortDir::Out);
            let in_ports = ports_of(&st.ports, target, PortDir::In);
            if out_ports.is_empty() || in_ports.is_empty() {
                let _ = tx_ui.send(AppMessage::AudioError(format!("{label}: no ports available yet, try again")));
                return;
            }
            let factory = st.link_factory.clone().unwrap_or_else(|| "link-factory".to_string());
            let links = create_links(core, &factory, own_tx, &out_ports, target, &in_ports);
            if links.is_empty() {
                let _ = tx_ui.send(AppMessage::AudioError(format!("{label}: failed to create TX link")));
            } else {
                let _ = tx_ui.send(AppMessage::AudioStatus(format!("TX -> {label} (software)")));
                st.tx_links.insert(label, links);
            }
        }
        PwCommand::UnlinkTx(label) => {
            if let Some(links) = st.tx_links.remove(&label) {
                for l in links {
                    let _ = core.destroy_object(l);
                }
                let _ = tx_ui.send(AppMessage::AudioStatus(format!("TX -> {label} disconnected")));
            }
        }
        PwCommand::LinkRx(label) => {
            let Some(own_rx) = st.own_rx_node else {
                let _ = tx_ui.send(AppMessage::AudioError(
                    "software RX not ready: ZDL-Echo's own input stream hasn't registered with PipeWire yet".into(),
                ));
                return;
            };
            let Some(target) = find_node(&st.nodes, &label, PortDir::Out) else {
                let _ = tx_ui.send(AppMessage::AudioError(format!("{label}: no output stream to capture from")));
                return;
            };
            let out_ports = ports_of(&st.ports, target, PortDir::Out);
            let in_ports = ports_of(&st.ports, own_rx, PortDir::In);
            if out_ports.is_empty() || in_ports.is_empty() {
                let _ = tx_ui.send(AppMessage::AudioError(format!("{label}: no ports available yet, try again")));
                return;
            }
            let factory = st.link_factory.clone().unwrap_or_else(|| "link-factory".to_string());
            let links = create_links(core, &factory, target, &out_ports, own_rx, &in_ports);
            if links.is_empty() {
                let _ = tx_ui.send(AppMessage::AudioError(format!("{label}: failed to create RX link")));
            } else {
                let _ = tx_ui.send(AppMessage::AudioStatus(format!("RX <- {label} (software)")));
                st.rx_links.insert(label, links);
            }
        }
        PwCommand::UnlinkRx(label) => {
            if let Some(links) = st.rx_links.remove(&label) {
                for l in links {
                    let _ = core.destroy_object(l);
                }
                let _ = tx_ui.send(AppMessage::AudioStatus(format!("RX <- {label} disconnected")));
            }
        }
    }
    send_snapshot(&st, tx_ui);
}

/// Entry point for the dedicated PipeWire routing thread. Blocks for the
/// life of the process (or until the connection fails). Safe to run even
/// where PipeWire isn't available — it just reports one error and returns,
/// leaving hardware routing in `audio.rs` completely unaffected.
pub fn run(tx_ui: Sender<AppMessage>, pw_cmd_rx: pw::channel::Receiver<PwCommand>) {
    pw::init();

    let mainloop = match pw::main_loop::MainLoopRc::new(None) {
        Ok(m) => m,
        Err(e) => {
            let _ = tx_ui.send(AppMessage::AudioError(format!("pipewire mainloop: {e}")));
            return;
        }
    };
    let context = match pw::context::ContextRc::new(&mainloop, None) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx_ui.send(AppMessage::AudioError(format!("pipewire context: {e}")));
            return;
        }
    };
    let core = match context.connect_rc(None) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx_ui.send(AppMessage::AudioError(format!("pipewire connect: {e}")));
            return;
        }
    };
    let registry = match core.get_registry_rc() {
        Ok(r) => r,
        Err(e) => {
            let _ = tx_ui.send(AppMessage::AudioError(format!("pipewire registry: {e}")));
            return;
        }
    };

    let state = Rc::new(RefCell::new(State::default()));

    let state_add = Rc::clone(&state);
    let tx_ui_add = tx_ui.clone();
    let state_rm = Rc::clone(&state);
    let tx_ui_rm = tx_ui.clone();
    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| on_global(global, &state_add, &tx_ui_add))
        .global_remove(move |id| on_global_remove(id, &state_rm, &tx_ui_rm))
        .register();

    let core_cmds = core.clone();
    let state_cmds = Rc::clone(&state);
    let tx_ui_cmds = tx_ui.clone();
    let _cmd_receiver = pw_cmd_rx.attach(mainloop.loop_(), move |cmd| {
        handle_command(cmd, &core_cmds, &state_cmds, &tx_ui_cmds);
    });

    mainloop.run();
}
