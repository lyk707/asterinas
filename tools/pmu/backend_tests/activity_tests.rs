// SPDX-License-Identifier: MPL-2.0

use crate::activity;

#[test]
fn event_activity_tracks_record_lifetime_across_threads_and_rollback() {
    assert!(!activity::has_events());
    let owner = activity::EventActivity::new();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(|| {
                for _ in 0..1_000 {
                    let _record = activity::EventActivity::new();
                    assert!(activity::has_events());
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert!(activity::has_events());
    let result: core::result::Result<(), ()> = (|| {
        let _failed_record = activity::EventActivity::new();
        Err(())?;
        Ok(())
    })();
    assert!(result.is_err() && activity::has_events());
    drop(owner);
    assert!(!activity::has_events());
}
