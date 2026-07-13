//! End-to-end tests for [`query_trace_stats_by_name`], which issues
//! `EVENT_TRACE_CONTROL_QUERY` and returns the session's runtime counters.
//!
//! Requires elevation (starting an ETW session needs admin), like the other
//! integration tests in this crate.

use std::process::Command;

use ferrisetw::provider::Provider;
use ferrisetw::trace::{query_trace_stats_by_name, UserTrace};

#[test]
fn query_stats_reports_running_session() {
    const NAME: &str = "ferrisetw-query-stats-test";

    // Clean up a session left behind by an interrupted run.
    let _ = Command::new("logman").args(["stop", "-ets", NAME]).output();

    // A session that is not running must error, not return zeros.
    assert!(
        query_trace_stats_by_name(NAME).is_err(),
        "querying a non-running session should fail"
    );

    // Start a real session. Microsoft-Windows-Wordpad is a convenient provider;
    // we never process events — we only need the session to exist.
    let provider = Provider::by_guid("54FFD262-99FE-4576-96E7-1ADB500370DC").build();
    let (trace, _handle) = UserTrace::new()
        .named(NAME.to_string())
        .enable(provider)
        .start()
        .expect("start ETW session (requires elevation)");

    let stats = query_trace_stats_by_name(NAME).expect("query a running session");

    // A live real-time session always has buffers allocated, which proves ETW
    // actually wrote the output fields into our properties buffer.
    assert!(
        stats.number_of_buffers > 0,
        "a running session should report allocated buffers, got {:?}",
        stats
    );
    // An idle session has dropped nothing.
    assert_eq!(
        stats.events_lost, 0,
        "an idle session should report no lost events, got {:?}",
        stats
    );

    trace.stop().expect("stop trace");

    // Once stopped, the session is gone and the query fails again.
    assert!(
        query_trace_stats_by_name(NAME).is_err(),
        "querying a stopped session should fail"
    );
}
