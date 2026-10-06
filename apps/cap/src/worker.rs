// SPDX-License-Identifier: MIT OR Apache-2.0
//! Image work must yield both the iced and headless command executors.
pub(crate) async fn run<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    std::thread::Builder::new()
        .name("cap-image".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .map_err(|e| e.to_string())?;
    rx.await.map_err(|_| "image worker stopped")?
}
#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn blocked_image_work_yields_the_command_executor() {
        let (release, held) = std::sync::mpsc::channel();
        let mut job = Box::pin(super::run(move || {
            held.recv().unwrap();
            Ok(42)
        }));
        assert!(iced::futures::poll!(&mut job).is_pending());
        release.send(()).unwrap();
        assert_eq!(job.await.unwrap(), 42);
    }
}
