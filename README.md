# ThreadIsolated

[![Build Status](https://api.travis-ci.org/jgallagher/thread_isolated.svg?branch=master)](https://travis-ci.org/jgallagher/thread_isolated)

[API Documentation](http://jgallagher.github.io/thread_isolated/thread_isolated/index.html)

ThreadIsolated is an experimental library for allowing non-`Send`, non-`Sync` types to be
shared among multiple threads by requiring that they only be accessed on the thread that
created and owns them. Other threads can indirectly access the isolated data by supplying a
closure that will be run on the owning thread.

The value of ThreadIsolated in a pure Rust application is probably pretty limited (maybe even
completely useless). The original motivation for the type is to use Rust on iOS, where it is
common to have instances that are only safe to be accessed on the iOS main thread.  Using
ThreadIsolated gives Rust a way to have types that are only accessed on the iOS main thread but
can still be used (albeit indirectly) from threads created in Rust.

In debug builds, `ThreadIsolated` will use thread-local storage and perform runtime checks that
all thread access obeys the expected rules. In release builds these checks are not performed.

For an example of ThreadIsolated in pure Rust code, see the `test_normal_use` function in the
`test` module of `src/lib.rs`.

## Author

John Gallagher, johnkgallagher@gmail.com

## License

ThreadIsolated is available under the MIT license. See the LICENSE file for more info.
