// Diagnostic: does the near-heap-limit callback fire in our build/env?
// Mirrors rusty_v8's own heap_limits test.

use std::ffi::c_void;
use std::sync::Once;

static INIT: Once = Once::new();
fn init() {
    INIT.call_once(|| {
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

struct State {
    calls: u64,
}

extern "C" fn cb(data: *mut c_void, current: usize, _initial: usize) -> usize {
    let st = unsafe { &mut *(data as *mut State) };
    st.calls += 1;
    current * 2
}

#[test]
fn near_heap_limit_fires_transient() {
    init();
    let params = v8::CreateParams::default().heap_limits(0, 10 << 20);
    let isolate = &mut v8::Isolate::new(params);
    let mut state = State { calls: 0 };
    let ptr = &mut state as *mut _ as *mut c_void;
    isolate.add_near_heap_limit_callback(cb, ptr);

    let scope = &mut v8::HandleScope::new(isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);

    for _ in 0..1_000_000 {
        let code = v8::String::new(
            scope,
            r#""hello world".repeat(10).split("o").map((s)=>s.repeat(100).split("o"))"#,
        )
        .unwrap();
        let script = v8::Script::compile(scope, code, None).unwrap();
        let _ = script.run(scope);
        if state.calls > 0 {
            break;
        }
    }
    assert!(state.calls > 0, "callback should fire on transient allocation");
}

// NOTE: V8 does NOT reliably invoke the near-heap-limit callback for a single
// long-running script that monotonically RETAINS memory (a growing global
// array). Empirically it fires 0 times before OOM. falcon therefore enforces
// its memory ceiling with an RSS-delta watchdog thread (see engine.rs), which
// is verified end-to-end by the huge-alloc acceptance test. This callback
// remains wired as a secondary catch for transient blowups (test above).
