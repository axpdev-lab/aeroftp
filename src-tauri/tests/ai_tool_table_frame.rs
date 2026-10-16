//! The tool table is built on whichever thread reads it first: in the MCP
//! server, a tokio worker serving a request. Built in one closure it needed a
//! 284 KiB frame in a debug build, growing with every tool added. Each entry
//! now has a frame of its own, so the whole table builds on a small stack
//! however many tools it holds.
//!
//! A regression aborts this test binary with "thread 'tool table on a
//! 128 KiB stack' has overflowed its stack"; it holds no other test.

use ftp_client_gui_lib::ai_core::tools::TOOL_DEFINITIONS;

const SMALL_STACK: usize = 128 * 1024;

#[test]
fn the_tool_table_builds_on_a_small_stack() {
    let tools = std::thread::Builder::new()
        .name("tool table on a 128 KiB stack".into())
        .stack_size(SMALL_STACK)
        .spawn(|| TOOL_DEFINITIONS.len())
        .expect("spawn the small-stack thread")
        .join()
        .expect("the table was built");
    assert!(tools > 100, "the whole table, {tools} tools");
}
