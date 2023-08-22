use std::sync::mpsc::{channel, Receiver, Sender};

crate::define_interface!();

#[cfg(test)]
mod tests {
    use super::*;

    crate::define_interface_tests!(test);

    #[test]
    fn message_buf_allocs_should_not_overlap() {
        let interface = Box::leak(Box::new(Interface::new()));
        let buf_1 = std::thread::spawn(|| interface.push());
        let buf_2 = std::thread::spawn(|| interface.push());
        let buf_3 = std::thread::spawn(|| interface.push());
        let buf_4 = std::thread::spawn(|| interface.push());

        let buf_1 = buf_1.join().unwrap();
        let buf_2 = buf_2.join().unwrap();
        let buf_3 = buf_3.join().unwrap();
        let buf_4 = buf_4.join().unwrap();

        assert!(!has_duplicates(&[buf_1, buf_2, buf_3, buf_4]));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn concurrent_get_after_creation_should_resolve() {
        let interface = Box::leak(Box::new(Interface::new()));

        let id = interface.push();
        // We test this properly elsewhere
        assert_eq!(id, 0, "spurious failure: interface push did not start at 0");
        let msg = std::thread::spawn(|| {
            // For lifetime simplicity, we don't bother moving `id` but not `interface`
            interface.get_raw(0)
        });
        assert!(!msg.is_finished());
        interface.send(42, id).unwrap();
        // We haven't terminated it yet
        assert!(!msg.is_finished());
        interface.terminate_message(id).unwrap();

        let msg = msg.join().unwrap();
        assert_eq!(msg[0], [42]);
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn concurrent_get_before_creation_should_resolve() {
        let interface = Box::leak(Box::new(Interface::new()));

        let msg = std::thread::spawn(|| {
            // Note that this message does not even exist yet
            interface.get_raw(0)
        });
        assert!(!msg.is_finished());
        // We test this properly elsewhere
        assert_eq!(
            interface.push(),
            0,
            "spurious failure: interface push did not start at 0"
        );

        // We can terminate before creation (zero-sized)
        interface.send(42, 0).unwrap();
        // We haven't terminated it yet
        assert!(!msg.is_finished());
        interface.terminate_message(0).unwrap();

        let msg = msg.join().unwrap();
        assert_eq!(msg[0], [42]);
    }
}
