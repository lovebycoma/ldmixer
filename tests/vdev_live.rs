//! Live test against the running PipeWire with an Audient interface present.
//! Run with: cargo test --test vdev_live -- --ignored

use std::process::Command;
use std::time::Duration;

use ld_mixer::vdev::{Manager, Spec};

fn pactl_has(kind: &str, name: &str) -> bool {
    let out = Command::new("pactl").args(["list", "short", kind]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).lines().any(|l| l.split('\t').nth(1) == Some(name))
}

#[test]
#[ignore]
fn creates_and_removes_devices() {
    let mic = Spec {
        node_name: "ld_mixer_test_mic_1".into(),
        description: "lD test Mic 1".into(),
        playback: false,
        channels: vec![0],
    };
    let daw = Spec {
        node_name: "ld_mixer_test_daw_3_4".into(),
        description: "lD test DAW 3+4".into(),
        playback: true,
        channels: vec![2, 3],
    };
    let mut m = Manager::default();
    assert!(m.reconcile(&[mic.clone(), daw.clone()]));
    std::thread::sleep(Duration::from_millis(1200));
    let st = m.status();
    assert!(st.iter().all(|s| s.running && s.error.is_none()), "{st:?}");
    assert!(pactl_has("sources", "ld_mixer_test_mic_1"));
    assert!(pactl_has("sinks", "ld_mixer_test_daw_3_4"));

    // Dropping a spec removes only that device.
    m.reconcile(&[daw.clone()]);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!pactl_has("sources", "ld_mixer_test_mic_1"));
    assert!(pactl_has("sinks", "ld_mixer_test_daw_3_4"));

    m.clear();
    std::thread::sleep(Duration::from_millis(500));
    assert!(!pactl_has("sinks", "ld_mixer_test_daw_3_4"));
}
