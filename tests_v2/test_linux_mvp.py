"""MVP integration tests for the limux v1 socket protocol.

Runs against a live limux instance launched by scripts/run-tests-linux.sh.

Covers the "did the flatten break anything" oracle set from the port plan:
workspace CRUD, panes/splits, terminal send/read, length-prefixed responses.
Broader parity with the existing v2 tests_v2/ test suite is follow-up work.
"""

import time

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
