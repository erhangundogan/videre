//! `videre gallery` starts `videre watch` beside it, so a library being
//! browsed keeps up without a second terminal, and never leaves that watch
//! behind: stopped on quit, and stopping itself if the gallery dies.

mod common;
use common::TestLibrary;

use std::net::{TcpListener, TcpStream};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A scanned library the gallery can open, holding only a short video: a
/// photo would give the watch face detection to do, which loads (and in a
/// private test cache, downloads) the face model, and a stage in flight
/// finishes before the watch looks at its parent again.
fn library() -> TestLibrary {
    let lib = TestLibrary::new();
    lib.copy_fixture("red_1s.mp4", "görüntü.mp4");
    lib.scan();
    lib
}

/// A child killed if the test fails before stopping it, so a failed test
/// never leaves a server or a watch running.
struct Reaped(Option<Child>);

impl Reaped {
    fn take(&mut self) -> Child {
        self.0.take().unwrap()
    }
}

impl Drop for Reaped {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn watch_running(lib: &TestLibrary) -> bool {
    videre_core::library_locks::command_locked(&lib.context(), "watch").unwrap_or(false)
}

/// Poll `cond` until it holds, or fail after `within` with `what`.
fn wait_until(within: Duration, what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !cond() {
        assert!(Instant::now() < deadline, "{what} (after {within:?})");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Start the gallery and wait until it accepts connections. Output is piped
/// so a test can read what the gallery said about the watch.
fn start_gallery(lib: &TestLibrary) -> (Reaped, u16) {
    let port = free_port();
    let child = lib
        .cmd()
        .args(["gallery", "--port", &port.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let gallery = Reaped(Some(child));
    wait_until(
        Duration::from_secs(20),
        "the gallery never listened",
        || TcpStream::connect(("127.0.0.1", port)).is_ok(),
    );
    (gallery, port)
}

/// Stop the gallery through `/api/quit` and return everything it printed.
fn quit(mut gallery: Reaped, port: u16) -> String {
    let mut child = gallery.take();
    common::stop_gallery(&mut child, port);
    let out = child.wait_with_output().unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn the_gallery_starts_a_watch_and_stops_it_on_quit() {
    let lib = library();
    let (gallery, port) = start_gallery(&lib);
    wait_until(Duration::from_secs(20), "no watch was started", || {
        watch_running(&lib)
    });
    let said = quit(gallery, port);
    assert!(said.contains("started videre watch"), "{said}");
    wait_until(
        Duration::from_secs(15),
        "the watch outlived the gallery",
        || !watch_running(&lib),
    );
}

#[test]
fn a_watch_already_running_is_used_and_left_running() {
    let lib = library();
    let mut own = Reaped(Some(
        lib.cmd()
            .args(["watch", "--silent"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    ));
    wait_until(
        Duration::from_secs(20),
        "the user's watch never started",
        || watch_running(&lib),
    );
    let (gallery, port) = start_gallery(&lib);
    let said = quit(gallery, port);
    assert!(
        said.contains("using the videre watch already running"),
        "{said}"
    );
    std::thread::sleep(Duration::from_millis(1500));
    let still = own.0.as_mut().unwrap().try_wait().unwrap().is_none();
    assert!(still, "the user's watch was stopped");
    assert!(watch_running(&lib));
}

#[test]
fn a_watch_stops_by_itself_when_the_gallery_is_killed() {
    let lib = library();
    let (mut gallery, _port) = start_gallery(&lib);
    wait_until(Duration::from_secs(20), "no watch was started", || {
        watch_running(&lib)
    });
    // SIGKILL: the gallery gets no chance to stop its watch.
    let mut child = gallery.take();
    child.kill().unwrap();
    child.wait().unwrap();
    wait_until(
        Duration::from_secs(5),
        "the watch outlived a killed gallery",
        || !watch_running(&lib),
    );
}

#[test]
fn no_watch_is_started_when_the_setting_is_off() {
    let lib = library();
    lib.no_gallery_watch();
    let (gallery, port) = start_gallery(&lib);
    std::thread::sleep(Duration::from_secs(2));
    assert!(!watch_running(&lib), "a watch started although turned off");
    let said = quit(gallery, port);
    assert!(!said.contains("videre watch"), "{said}");
}
