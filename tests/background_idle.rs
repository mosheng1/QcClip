#![allow(dead_code, unused_imports)]

#[path = "../src/platform/input.rs"]
mod input;
#[path = "../src/services/updates/schedule.rs"]
mod update_schedule;

use gpui::{Bounds, Point, Size};
use input::PointerRegionWatch;
use std::time::Duration;
use update_schedule::next_check_delay;

#[test]
fn hidden_edge_only_wakes_when_entering_or_leaving_trigger_region() {
    let region = Bounds {
        origin: Point { x: -2.0, y: 200.0 },
        size: Size {
            width: 2.0,
            height: 520.0,
        },
    };
    let mut watch = PointerRegionWatch::new(
        Some(region),
        Some(Point {
            x: -500.0,
            y: 300.0,
        }),
    );
    for x in -500..-2 {
        assert!(!watch.should_notify(Point {
            x: x as f32,
            y: 300.0
        }));
    }
    assert!(watch.should_notify(Point { x: -2.0, y: 300.0 }));
    assert!(!watch.should_notify(Point { x: -1.0, y: 400.0 }));
    assert!(watch.should_notify(Point { x: 0.0, y: 400.0 }));
    assert!(!watch.should_notify(Point { x: 100.0, y: 400.0 }));
    assert!(watch.should_notify(Point { x: -1.0, y: 719.0 }));
    assert!(watch.should_notify(Point { x: -1.0, y: 720.0 }));
}

#[test]
fn visible_window_keeps_receiving_pointer_changes() {
    let mut watch = PointerRegionWatch::new(None, None);
    assert!(watch.should_notify(Point { x: 10.0, y: 20.0 }));
    assert!(watch.should_notify(Point { x: 11.0, y: 20.0 }));
}

#[test]
fn edge_watch_preserves_initial_inside_state() {
    let region = Bounds {
        origin: Point { x: 0.0, y: 0.0 },
        size: Size {
            width: 2.0,
            height: 100.0,
        },
    };
    let mut watch = PointerRegionWatch::new(Some(region), Some(Point { x: 1.0, y: 50.0 }));
    assert!(!watch.should_notify(Point { x: 1.0, y: 51.0 }));
    assert!(watch.should_notify(Point { x: 2.0, y: 51.0 }));
    assert!(watch.should_notify(Point { x: 1.0, y: 51.0 }));
}

#[test]
fn updates_sleep_until_due_without_minute_polling() {
    assert_eq!(
        next_check_delay(Some(86400), 1000, 0, 1060),
        Some(Duration::from_secs(86340))
    );
    assert_eq!(
        next_check_delay(Some(86400), 1000, 0, 87400),
        Some(Duration::ZERO)
    );
    assert_eq!(next_check_delay(None, 1000, 0, 1060), None);
}

#[test]
fn update_schedule_handles_failure_clock_rollback_and_first_check() {
    assert_eq!(
        next_check_delay(Some(86400), 0, 0, 1000),
        Some(Duration::ZERO)
    );
    assert_eq!(
        next_check_delay(Some(86400), 0, 4600, 1000),
        Some(Duration::from_secs(3600))
    );
    assert_eq!(
        next_check_delay(Some(86400), 2000, 0, 1000),
        Some(Duration::ZERO)
    );
    assert_eq!(
        next_check_delay(Some(86400), 2000, 4600, 1000),
        Some(Duration::from_secs(3600))
    );
}

#[test]
fn failed_update_check_retries_before_the_regular_interval() {
    // 即使刚成功检查过，手动检查失败后也应在一小时后重试。
    assert_eq!(
        next_check_delay(Some(86400), 1000, 4660, 1060),
        Some(Duration::from_secs(3600))
    );
    assert_eq!(
        next_check_delay(Some(86400), 1000, 4660, 4661),
        Some(Duration::ZERO)
    );
    // 持久化的期限遇到时间回拨时，不应长时间停查。
    assert_eq!(
        next_check_delay(Some(86400), 9000, 12000, 1000),
        Some(Duration::from_secs(3600))
    );
}
