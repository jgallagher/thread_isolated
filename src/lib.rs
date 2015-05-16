#![feature(alloc)]
#![feature(core)]
#![cfg_attr(test, feature(std_misc))]

use std::boxed::FnBox;
use std::cell::{Ref, RefMut, RefCell};
use std::marker::PhantomData;
use std::mem;
use std::sync::{Arc, Weak};
use std::sync::mpsc::sync_channel;

use self::debug::ThreadDebugger;

pub trait IsolationRunner: Sync + 'static {
    fn run_on_owning_thread(&self, f: Box<FnBox() + Send>);
}

pub enum OwningThread {}
pub enum NonOwningThread {}

pub struct ThreadIsolated<T, ThreadKind> {
    inner: Arc<Inner<T>>,
    debug: ThreadDebugger,
    _marker: PhantomData<ThreadKind>,
}

struct Inner<T> {
    item: RefCell<T>,
    runner: Box<IsolationRunner>,
}

impl<T> ThreadIsolated<T, OwningThread> {
    pub unsafe fn new<R: IsolationRunner>(item: T, runner: R) -> ThreadIsolated<T, OwningThread> {
        ThreadIsolated{
            inner: Arc::new(Inner{
                item: RefCell::new(item),
                runner: Box::new(runner),
            }),
            debug: ThreadDebugger::new(),
            _marker: PhantomData,
        }
    }

    pub fn borrow(&self) -> Ref<T> {
        self.debug.assert_on_originating_thread();
        self.inner.item.borrow()
    }

    pub fn borrow_mut(&self) -> RefMut<T> {
        self.debug.assert_on_originating_thread();
        self.inner.item.borrow_mut()
    }

    pub fn clone_for_non_owning_thread(&self) -> ThreadIsolated<T, NonOwningThread> {
        ThreadIsolated{
            inner: self.inner.clone(),
            debug: self.debug,
            _marker: PhantomData,
        }
    }

    pub fn downgrade_for_non_owning_thread(&self) -> ThreadIsolatedWeak<T> {
        ThreadIsolatedWeak{
            inner: self.inner.downgrade(),
            debug: self.debug,
        }
    }
}

impl<T> ThreadIsolated<T, NonOwningThread> {
    pub unsafe fn as_owning_thread(self) -> ThreadIsolated<T, OwningThread> {
        self.debug.assert_on_originating_thread();
        ThreadIsolated{
            inner: self.inner,
            debug: self.debug,
            _marker: PhantomData,
        }
    }

    pub fn with<U, F>(&self, f: F) -> U where
        F: FnOnce(&RefCell<T>) -> U + Send,
        U: Send,
    {
        self.debug.assert_not_on_originating_thread();

        let (tx, rx) = sync_channel(1);

        let closure: Box<FnBox() + Send> = Box::new(move|| {
            self.debug.assert_on_originating_thread();
            let u = f(&self.inner.item);
            tx.send(u).unwrap();
        });

        // `closure` contains references, but IsolationRunner expects an FnBox
        // that is 'static. We're using a channel to block until after `f` has been
        // called, so any references it contains will be alive as long as they need
        // to be. The borrow checker can't know that (the channels are purely a
        // runtime thing), so we'll use mem::transmute to force `closure` to *look*
        // like a 'static FnBox.
        //
        // This is safe because the runner can only call closure once, and we won't
        // return until after that call finishes.
        let closure: Box<FnBox() + Send + 'static> = unsafe { mem::transmute(closure) };
        self.inner.runner.run_on_owning_thread(closure);

        // Memory safety of the above closure relies on this recv() blocking until
        // the closure has run. It's not safe for us to just panic if recv() fails;
        // we need to tear down the whole process.
        match rx.recv() {
            Ok(u) => u,
            Err(err) => {
                println!("recv() failed: {}", err);
                unsafe { std::intrinsics::abort(); }
            }
        }
    }
}

impl<T> Clone for ThreadIsolated<T, NonOwningThread> {
    fn clone(&self) -> ThreadIsolated<T, NonOwningThread> {
        ThreadIsolated{
            inner: self.inner.clone(),
            debug: self.debug,
            _marker: PhantomData,
        }
    }
}

unsafe impl<T> Send for ThreadIsolated<T, NonOwningThread> {}
unsafe impl<T> Sync for ThreadIsolated<T, NonOwningThread> {}

pub struct ThreadIsolatedWeak<T> {
    inner: Weak<Inner<T>>,
    debug: ThreadDebugger,
}

impl<T> ThreadIsolatedWeak<T> {
    pub fn upgrade(&self) -> Option<ThreadIsolated<T, NonOwningThread>> {
        self.inner.upgrade().map(|inner| {
            ThreadIsolated{
                inner: inner,
                debug: self.debug,
                _marker: PhantomData,
            }
        })
    }
}

unsafe impl<T> Send for ThreadIsolatedWeak<T> {}
unsafe impl<T> Sync for ThreadIsolatedWeak<T> {}

#[cfg(debug_assertions)]
mod debug {
    use std::sync::atomic::{AtomicUsize, Ordering, ATOMIC_USIZE_INIT};

    static THREAD_ID_COUNTER: AtomicUsize = ATOMIC_USIZE_INIT;

    fn next_thread_id() -> usize {
        THREAD_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
    }

    thread_local!(static THREAD_ID: usize = next_thread_id());

    fn our_thread_id() -> usize {
        THREAD_ID.with(|t| *t)
    }

    #[derive(Copy,Clone)]
    pub struct ThreadDebugger {
        thread_id: usize,
    }

    impl ThreadDebugger {
        pub fn new() -> ThreadDebugger {
            ThreadDebugger{
                thread_id: our_thread_id(),
            }
        }

        pub fn assert_on_originating_thread(&self) {
            assert!(self.thread_id == our_thread_id(), "item accessed off of owning thread");
        }

        pub fn assert_not_on_originating_thread(&self) {
            assert!(self.thread_id != our_thread_id(),
                    "item unexpectedly accessed on owning thread");
        }
    }
}

#[cfg(not(debug_assertions))]
mod debug {
    #[derive(Copy,Clone)]
    pub struct ThreadDebugger;

    impl ThreadDebugger {
        pub fn new() -> ThreadDebugger { ThreadDebugger }
        pub fn assert_on_originating_thread(&self) {}
        pub fn assert_not_on_originating_thread(&self) {}
    }
}

#[cfg(test)]
mod test {
    use std::thread;
    use std::sync::{Arc, Barrier, Mutex};
    use std::sync::mpsc::{channel, Sender};
    use std::boxed::FnBox;

    use super::{IsolationRunner, ThreadIsolated};

    #[test]
    fn test_normal_use() {
        // Create an isolation runner that uses channels to send boxed functions
        // back to the main thread for it to run.
        struct Runner {
            tx: Mutex<Sender<Box<FnBox() + Send>>>,
        }

        impl IsolationRunner for Runner {
            fn run_on_owning_thread(&self, boxed: Box<FnBox() + Send>) {
                let guard = self.tx.lock().unwrap();
                guard.send(boxed).unwrap();
            }
        }

        let (tx, rx) = channel();
        let (tx_f, rx_f) = channel();
        let (tx_clone, rx_clone) = channel();
        let runner = Runner{ tx: Mutex::new(tx_f) };

        // start up the owning thread
        let handle = thread::spawn(move|| {
            // create our ThreadIsolated...
            let t = unsafe { ThreadIsolated::new(0i32, runner) };

            // ...and give a non-owning clone back to the test_normal_use thread.
            tx_clone.send(t.clone_for_non_owning_thread()).unwrap();

            loop {
                select! (
                    // If we receive a message on rx, confirm that t has been incremented
                    // the expected number of times, then exit.
                    _ = rx.recv() => {
                        assert_eq!(*t.borrow(), 20);
                        return
                    },

                    // If we receive a function on rx_f, run it.
                    f = rx_f.recv() => {
                        f.unwrap()();
                    }
                )
            }
        });

        // start up 10 worker threads, each of which will try to increment the counter
        // owned by the thread above twice.
        let barrier = Arc::new(Barrier::new(11));
        let non_owning_t = rx_clone.recv().unwrap();
        for _ in 0 .. 10 {
            let b = barrier.clone();
            let t = non_owning_t.clone();
            thread::spawn(move|| {
                let t2 = t.clone();
                t.with(|refcell| {
                    *refcell.borrow_mut() += 1;

                    // Inside `with`, we're on the owning thread, so we can (unsafely)
                    // convert a NonOwningThread handle to an OwningThread handle.
                    let owning_t = unsafe { t2.as_owning_thread() };
                    *owning_t.borrow_mut() += 1;
                });
                b.wait();
            });
        }

        // wait until all worker threads are finished
        barrier.wait();

        // send the owning thread the message to check that its counter is as expected
        tx.send(()).unwrap();

        assert!(handle.join().is_ok());
    }

    #[cfg(debug_assertions)]
    mod debug_tests {
        use std::thread;
        use std::boxed::FnBox;
        use ::{IsolationRunner, ThreadIsolated};

        struct UnsafeNoopRunner;

        impl IsolationRunner for UnsafeNoopRunner {
            fn run_on_owning_thread(&self, f: Box<FnBox() + Send>) {
                f()
            }
        }

        #[test]
        fn test_accessing_nonowning_value_from_owning_thread_panics() {
            let handle = thread::spawn(move|| {
                let t = unsafe { ThreadIsolated::new(0i32, UnsafeNoopRunner) };
                let t2 = t.clone_for_non_owning_thread();

                t2.with(|t| {
                    *t.borrow_mut() = 1;
                });
            });

            assert!(handle.join().is_err());
        }

        #[test]
        fn test_accessing_owning_value_from_nonowning_thread_panics() {
            let t = unsafe { ThreadIsolated::new(0i32, UnsafeNoopRunner) };
            let t2 = t.clone_for_non_owning_thread();

            let handle = thread::spawn(move|| {
                t2.with(|t| {
                    *t.borrow_mut() = 1;
                });
            });

            assert!(handle.join().is_err());
        }

        #[test]
        fn test_as_owning_thread_panics_if_called_off_owning_thread() {
            let t = unsafe { ThreadIsolated::new(0i32, UnsafeNoopRunner) };
            let t2 = t.clone_for_non_owning_thread();

            let handle = thread::spawn(move|| {
                let _ = unsafe { t2.as_owning_thread() };
            });

            assert!(handle.join().is_err());
        }
    }
}
