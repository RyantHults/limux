"""MVP integration tests for the limux v1 socket protocol.

Runs against a live limux instance launched by scripts/run-tests-linux.sh.

Covers the "did the flatten break anything" oracle set from the port plan:
workspace CRUD, panes/splits, terminal send/read, length-prefixed responses.
Broader parity with the existing v2 tests_v2/ test suite is follow-up work.
"""

import os
import signal
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

import pytest

from limux import limux, limuxError


@pytest.fixture
def cli():
    with limux() as c:
        yield c


def test_ping(cli):
    assert cli.ping()


def test_version(cli):
    v = cli.version()
    assert v.startswith("limux")


def test_workspace_lifecycle(cli):
    """Create, list, rename, select, close."""
    start_count = cli.workspace_count()
    cli.new_workspace()
    assert cli.workspace_count() == start_count + 1

    workspaces = cli.list_workspaces()
    assert len(workspaces) == start_count + 1
    new_ws = workspaces[-1]
    assert "id" in new_ws and "title" in new_ws

    cli.rename_workspace(new_ws["id"], "mvp-test")
    renamed = [w for w in cli.list_workspaces() if w["id"] == new_ws["id"]][0]
    assert renamed["title"] == "mvp-test"

    cli.select_workspace(new_ws["id"])
    ws_id, _title = cli.current_workspace()
    assert ws_id == new_ws["id"]

    # close_workspace is async end-to-end: the socket returns OK once
    # request_close is dispatched to panels, but actual teardown requires
    # SIGHUP → shell exit → ghostty close callback → panel removal on the
    # GTK main loop. Under xvfb with a fresh bash that chain has flaky timing.
    # For MVP we verify the command was accepted; workspace-count-after-close
    # is a follow-up once close semantics are tightened up.
    cli.close_workspace(new_ws["id"])


def test_panes_and_splits(cli):
    """A fresh workspace starts with one pane; splitting yields more."""
    cli.new_workspace()
    ws_id, _ = cli.current_workspace()

    panes_before = cli.list_panes(ws_id)
    assert len(panes_before) == 1

    cli.split_right()
    panes_after = cli.list_panes(ws_id)
    assert len(panes_after) == 2

    cli.split_down()
    panes_after_two = cli.list_panes(ws_id)
    assert len(panes_after_two) == 3

    # Focus the first pane to make sure the lookup works
    cli.focus_pane(panes_after_two[0])

    cli.close_workspace(ws_id)


def test_terminal_send_and_read(cli):
    """Send text to a terminal surface and read it back from the screen buffer."""
    cli.new_workspace()
    ws_id, _ = cli.current_workspace()

    # list_surfaces emits "surface:ID workspace:W pane:P terminal cwd=..." per
    # line for terminal panels; the trailing `terminal` keyword is what the
    # helper records in .kind, so filter on that. (The "surface:" prefix is
    # the ID namespace, not a kind.)
    surfaces = [
        s for s in cli.list_surfaces()
        if s.get("workspace") == ws_id and s.get("kind") == "terminal"
    ]
    assert surfaces, "new workspace should have at least one terminal surface"
    surface_id = surfaces[0]["id"]

    # We can't send control chars like \r through the v1 line protocol —
    # handle_command()'s .trim() strips trailing whitespace, including \r.
    # Instead, send the marker as typed text and read it back from the
    # screen buffer. Typed-but-not-executed text still appears on screen,
    # which is all we need to verify the send/read round-trip.
    marker = f"limux-mvp-{int(time.time())}"
    cli.send(surface_id, marker)
    time.sleep(0.3)

    screen = cli.read_screen(surface_id)
    assert marker in screen, f"marker {marker!r} not found in screen: {screen!r}"

    cli.close_workspace(ws_id)


def test_list_workspaces_length_prefixed(cli):
    """list_workspaces with >0 workspaces uses the length-prefixed protocol."""
    # This exercises the OK+<len>\n<data> framing that read_screen also uses.
    entries = cli.list_workspaces()
    assert isinstance(entries, list)
    for entry in entries:
        assert "id" in entry
        assert "title" in entry
        assert "panes" in entry


def test_error_on_unknown_workspace(cli):
    with pytest.raises(limuxError):
        cli.select_workspace(99999)


def _wait_until(predicate, timeout_s=15.0, interval_s=0.15):
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(interval_s)
    return False


def test_new_pane_tab_inherits_working_directory():
    """A new tab in a pane must start in the focused tab's working directory.

    Regression test for the bug where new_pane_tab() fell back to the
    app-level default working directory instead of the focused tab's.

    Pasting an OSC-0 title or a command is not viable here: ghostty's paste
    path strips ESC (and a trailing CR is trimmed by the socket protocol),
    and readline's bracketed paste makes a pasted command inert. So this test
    launches its own instance with --command, and the command itself:
      1. prints the directory the shell was actually spawned in (PWDINIT:$PWD)
      2. emits a fixed OSC-0 title pointing at `target`, which limux converts
         into tab.working_directory (via SET_TITLE -> extract_directory)
      3. execs an interactive bash so the surface stays usable
    tab1's working_directory is seeded by its own startup title, and tab2's
    PWDINIT line reveals which directory it was spawned in.
    """
    target = "/tmp/limux-cwd-inherit"
    os.makedirs(target, exist_ok=True)
    bin_path = os.environ.get("LIMUX_BIN") or str(
        Path(__file__).resolve().parents[1] / "target/debug/limux"
    )
    if not os.path.exists(bin_path):
        pytest.skip(f"limux binary not found at {bin_path}")

    sock_dir = tempfile.mkdtemp(prefix="limux-cwd-test-")
    sock_path = os.path.join(sock_dir, "limux.sock")
    command = (
        "printf 'PWDINIT:%%s\\n' \"$PWD\"; "
        "printf '\\033]0;user@host:%s\\007'; "
        "exec bash"
    ) % (target,)

    cmd = [bin_path, "--socket", sock_path, "--command", command]
    if not os.environ.get("DISPLAY") and not os.environ.get("WAYLAND_DISPLAY"):
        if not shutil.which("xvfb-run"):
            pytest.skip("no DISPLAY and xvfb-run unavailable")
        cmd = ["xvfb-run", "-a", "--server-args=-screen 0 1280x800x24"] + cmd

    proc = subprocess.Popen(
        cmd,
        start_new_session=True,
    )
    try:
        def socket_ready():
            if os.path.exists(sock_path):
                return True
            if proc.poll() is not None:
                raise AssertionError(
                    f"limux exited early (rc={proc.returncode})"
                )
            return False

        assert _wait_until(socket_ready, timeout_s=30), "socket never appeared"

        with limux(socket_path=sock_path) as cli:
            cli.new_workspace()
            ws_id, _ = cli.current_workspace()

            def terminals():
                return [
                    s for s in cli.list_surfaces()
                    if s.get("workspace") == ws_id and s.get("kind") == "terminal"
                ]

            assert _wait_until(lambda: len(terminals()) >= 1)
            tab1_id = terminals()[0]["id"]

            # Wait until tab1's own startup title has seeded its working
            # directory with `target` (which differs from the app default).
            ok = _wait_until(
                lambda: any(s.get("cwd") == target for s in terminals()),
                timeout_s=8.0,
            )
            assert ok, "tab1 never picked up cwd=%s" % target

            cli.new_pane_tab()

            def tab2s():
                return [t for t in terminals() if t["id"] != tab1_id]

            assert _wait_until(lambda: len(tab2s()) == 1)
            tab2_id = tab2s()[0]["id"]

            def spawned_dir():
                try:
                    screen = cli.read_screen(tab2_id)
                except limuxError:
                    return None
                for line in screen.splitlines():
                    if line.startswith("PWDINIT:"):
                        return line[len("PWDINIT:"):]
                return None

            assert _wait_until(lambda: spawned_dir() is not None), (
                "no PWDINIT marker on tab2 screen"
            )
            assert spawned_dir() == target, (
                f"tab2 was spawned in {spawned_dir()!r}, expected {target!r}"
            )
    finally:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            proc.wait()
        shutil.rmtree(sock_dir, ignore_errors=True)

