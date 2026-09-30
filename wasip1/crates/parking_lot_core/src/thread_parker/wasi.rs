//! WASI（wasi-threads）の待ち。std の `thread::park` / `unpark` の上に載せる。
//!
//! wasm の本物の待ち（`memory.atomic.wait32`）を使う `wasm_atomic.rs` は nightly の
//! feature でしか入らず、それ以外の wasm は `wasm.rs`（競合した時点で panic）になる。
//! wasm32-wasip1-threads の std は park / unpark を futex（`memory.atomic.wait32` /
//! `notify`）で実装しているので、それを使えば stable のまま本物の待ちができる。
//! park は「先に unpark されていたら即座に戻る」札を持つので、順番の競合も起きない

use core::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, Thread};
use std::time::Instant;

pub struct ThreadParker {
    parked: AtomicBool,
    /// 眠る側のスレッド。`ThreadData` はスレッドごとに作られるので、作ったスレッドが持ち主
    thread: Thread,
}

impl super::ThreadParkerT for ThreadParker {
    type UnparkHandle = UnparkHandle;

    const IS_CHEAP_TO_CONSTRUCT: bool = true;

    #[inline]
    fn new() -> ThreadParker {
        ThreadParker {
            parked: AtomicBool::new(false),
            thread: thread::current(),
        }
    }

    #[inline]
    unsafe fn prepare_park(&self) {
        self.parked.store(true, Ordering::Relaxed);
    }

    #[inline]
    unsafe fn timed_out(&self) -> bool {
        self.parked.load(Ordering::Relaxed)
    }

    #[inline]
    unsafe fn park(&self) {
        while self.parked.load(Ordering::Acquire) {
            thread::park();
        }
    }

    #[inline]
    unsafe fn park_until(&self, timeout: Instant) -> bool {
        while self.parked.load(Ordering::Acquire) {
            let now = Instant::now();
            if now >= timeout {
                return false;
            }
            thread::park_timeout(timeout - now);
        }
        true
    }

    #[inline]
    unsafe fn unpark_lock(&self) -> UnparkHandle {
        self.parked.store(false, Ordering::Release);
        UnparkHandle(self.thread.clone())
    }
}

pub struct UnparkHandle(Thread);

impl super::UnparkHandleT for UnparkHandle {
    #[inline]
    unsafe fn unpark(self) {
        self.0.unpark();
    }
}

#[inline]
pub fn thread_yield() {
    thread::yield_now();
}
