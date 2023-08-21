crate::define_interface!(async, await);

#[cfg(test)]
mod tests {
    use super::*;

    crate::define_interface_tests!(tokio::test, async, await);

    #[tokio::test]
    async fn message_buf_allocs_should_not_overlap() {
        let interface = Box::leak(Box::new(Interface::new()));
        let buf_1 = tokio::task::spawn(async { interface.push().await });
        let buf_2 = tokio::task::spawn(async { interface.push().await });
        let buf_3 = tokio::task::spawn(async { interface.push().await });
        let buf_4 = tokio::task::spawn(async { interface.push().await });

        let buf_1 = buf_1.await.unwrap();
        let buf_2 = buf_2.await.unwrap();
        let buf_3 = buf_3.await.unwrap();
        let buf_4 = buf_4.await.unwrap();

        assert!(!has_duplicates(&[buf_1, buf_2, buf_3, buf_4]));
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn concurrent_get_after_creation_should_resolve() {
        let interface = Box::leak(Box::new(Interface::new()));

        let id = interface.push().await;
        // We test this properly elsewhere
        assert_eq!(id, 0, "spurious failure: interface push did not start at 0");
        let msg = tokio::task::spawn(async {
            // For lifetime simplicity, we don't bother moving `id` but not `interface`
            interface.get_raw(0).await
        });
        assert!(!msg.is_finished());
        interface.send(42, id).await.unwrap();
        // We haven't terminated it yet
        assert!(!msg.is_finished());
        interface.terminate_message(id).await.unwrap();

        let msg = msg.await.unwrap();
        assert_eq!(msg, [42]);
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn concurrent_get_before_creation_should_resolve() {
        let interface = Box::leak(Box::new(Interface::new()));

        let msg = tokio::task::spawn(async {
            // Note that this message does not even exist yet
            interface.get_raw(0).await
        });
        assert!(!msg.is_finished());
        // We test this properly elsewhere
        assert_eq!(
            interface.push().await,
            0,
            "spurious failure: interface push did not start at 0"
        );

        // We can terminate before creation (zero-sized)
        interface.send(42, 0).await.unwrap();
        // We haven't terminated it yet
        assert!(!msg.is_finished());
        interface.terminate_message(0).await.unwrap();

        let msg = msg.await.unwrap();
        assert_eq!(msg, [42]);
    }
}
