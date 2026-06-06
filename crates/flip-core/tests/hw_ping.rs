//! Hardware round-trip: requires a Flipper with the flip-link FAP running.
//! Run with: FLIPPER_HW=1 cargo test -p flip-core --test hw_ping -- --nocapture

use flip_core::device::DeviceLink;
use flip_core::serial::{pick_agent_port, SerialTransport};
use std::time::Duration;

#[test]
fn ping_pong_over_usb() {
    if std::env::var("FLIPPER_HW").ok().as_deref() != Some("1") {
        eprintln!("skipping: set FLIPPER_HW=1 with the FAP running on-device");
        return;
    }
    let port = pick_agent_port().expect("agent port");
    eprintln!("using agent port: {port}");
    let transport = SerialTransport::open(&port).expect("open port");
    let mut link = DeviceLink::new(transport);

    let payload = b"flip-link-spike";
    let echoed = link
        .ping(payload, Duration::from_secs(2))
        .expect("pong round-trip");
    assert_eq!(&echoed, payload);
}
