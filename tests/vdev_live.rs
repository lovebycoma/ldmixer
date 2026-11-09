//! Live test against the running PipeWire with an Audient interface present.
//! Run with: cargo test --test vdev_live -- --ignored

use std::process::Command;
use std::time::Duration;

use ldmixer::vdev::{Manager, Spec};

/// Ports linked to `port` according to `pw-link -l`.
fn links_of(port: &str) -> Vec<String> {
    let out = Command::new("pw-link").arg("-l").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let mut found = Vec::new();
    let mut current = false;
    for line in text.lines() {
        if !line.starts_with(' ') {
            current = line.trim() == port;
        } else if current {
            if let Some(p) = line.trim().strip_prefix("|-> ") {
                found.push(p.to_string());
            }
        }
    }
    found
}

fn pactl_has(kind: &str, name: &str) -> bool {
    let out = Command::new("pactl").args(["list", "short", kind]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).lines().any(|l| l.split('\t').nth(1) == Some(name))
}

#[test]
#[ignore]
fn creates_and_removes_devices() {
    let mic = Spec {
        node_name: "ldmixer_test_mic_1".into(),
        description: "lD test Mic 1".into(),
        playback: false,
        channels: vec![0],
    };
    let daw = Spec {
        node_name: "ldmixer_test_daw_3_4".into(),
        description: "lD test DAW 3+4".into(),
        playback: true,
        channels: vec![2, 3],
    };
    let mut m = Manager::default();
    assert!(m.reconcile(&[mic.clone(), daw.clone()]));
    std::thread::sleep(Duration::from_millis(1200));
    let st = m.status();
    assert!(st.iter().all(|s| s.running && s.error.is_none()), "{st:?}");
    assert!(pactl_has("sources", "ldmixer_test_mic_1"));
    assert!(pactl_has("sinks", "ldmixer_test_daw_3_4"));

    // The device side must reach the interface itself, never another node
    // (EasyEffects, the default sink, ...).
    for (port, aux) in [("output_AUX2", "playback_AUX2"), ("output_AUX3", "playback_AUX3")] {
        let links = links_of(&format!("output.ldmixer_test_daw_3_4:{port}"));
        assert!(
            links.len() == 1 && links[0].starts_with("alsa_output.usb-Audient") && links[0].ends_with(aux),
            "{port} linked to {links:?}"
        );
    }
    let links = links_of("alsa_input.usb-Audient_Audient_iD24-00.multichannel-input:capture_AUX0");
    assert!(links.iter().any(|l| l == "input.ldmixer_test_mic_1:input_AUX0"), "mic capture links {links:?}");

    // Dropping a spec removes only that device.
    m.reconcile(&[daw.clone()]);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!pactl_has("sources", "ldmixer_test_mic_1"));
    assert!(pactl_has("sinks", "ldmixer_test_daw_3_4"));

    m.clear();
    std::thread::sleep(Duration::from_millis(500));
    assert!(!pactl_has("sinks", "ldmixer_test_daw_3_4"));
}
