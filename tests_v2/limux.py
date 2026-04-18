#!/usr/bin/env python3
"""limux Python test client (v1 text protocol).

Talks to a running limux instance over its Unix socket (`LIMUX_SOCKET`).

Protocol:
  Request:  "cmd arg arg\\n"
  Response: single line   "OK [payload]\\n"     or
            single line   "ERROR message\\n"     or
            length-prefix  "OK+<byte_count>\\n<raw bytes>"   (no trailing newline)

See app/src/socket.rs for the authoritative list of commands.

This replaces the earlier JSON-RPC v2 helper (preserved in git history). The
v2 client assumed the macOS limux socket; limux does not speak v2.
"""

from __future__ import annotations

import os
import socket
import time
from typing import List, Optional, Tuple


class limuxError(Exception):
    """Raised when a command returns an ERROR line or the socket misbehaves."""


def _default_socket_path() -> str:
    override = os.environ.get("LIMUX_SOCKET")
    if override:
        return override
    raise limuxError(
        "LIMUX_SOCKET not set. Launch limux or point the env var at a running "
        "instance's socket before running tests."
    )


class limux:
    def __init__(self, socket_path: Optional[str] = None):
        self.socket_path = socket_path or _default_socket_path()
        self._sock: Optional[socket.socket] = None

    # --- lifecycle ------------------------------------------------------

    def connect(self, timeout_s: float = 5.0) -> None:
        deadline = time.time() + timeout_s
        last_err: Optional[Exception] = None
        while time.time() < deadline:
            try:
                s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                s.settimeout(timeout_s)
                s.connect(self.socket_path)
                self._sock = s
                return
            except OSError as e:
                last_err = e
                time.sleep(0.05)
        raise limuxError(f"could not connect to {self.socket_path}: {last_err}")

    def close(self) -> None:
        if self._sock is not None:
            try:
                self._sock.close()
            finally:
                self._sock = None

    def __enter__(self) -> "limux":
        if self._sock is None:
            self.connect()
        return self

    def __exit__(self, *_exc) -> None:
        self.close()

    # --- framing --------------------------------------------------------

    def _send_line(self, line: str) -> str:
        if self._sock is None:
            self.connect()
        assert self._sock is not None
        self._sock.sendall((line + "\n").encode("utf-8"))
        return self._recv_response()

    def _recv_response(self, timeout_s: float = 20.0) -> str:
        assert self._sock is not None
        self._sock.settimeout(timeout_s)
        buf = bytearray()
        while b"\n" not in buf:
            chunk = self._sock.recv(4096)
            if not chunk:
                raise limuxError("socket closed while reading header")
            buf.extend(chunk)

        newline = buf.index(b"\n")
        header = buf[:newline].decode("utf-8", errors="replace")
        remaining = bytes(buf[newline + 1 :])

        if header.startswith("OK+"):
            try:
                length = int(header[3:])
            except ValueError as e:
                raise limuxError(f"malformed length-prefix header: {header!r}") from e
            body = bytearray(remaining)
            while len(body) < length:
                chunk = self._sock.recv(min(4096, length - len(body)))
                if not chunk:
                    raise limuxError("socket closed while reading body")
                body.extend(chunk)
            return body[:length].decode("utf-8", errors="replace")

        if header.startswith("ERROR"):
            raise limuxError(header)
        return header

    def _expect_ok(self, response: str) -> str:
        """For single-line `OK [payload]` replies, return the payload (or '')."""
        if not response.startswith("OK"):
            raise limuxError(f"unexpected response: {response!r}")
        tail = response[2:]
        return tail[1:] if tail.startswith(" ") else tail

    # --- commands: system -----------------------------------------------

    def ping(self) -> bool:
        return self._send_line("ping") == "OK pong"

    def version(self) -> str:
        return self._expect_ok(self._send_line("version"))

    # --- commands: workspaces -------------------------------------------

    def new_workspace(self) -> None:
        self._expect_ok(self._send_line("new_workspace"))

    def workspace_count(self) -> int:
        return int(self._expect_ok(self._send_line("workspace_count")))

    def list_workspaces(self) -> List[dict]:
        """Returns a list of dicts: {id, title, panes, pinned, color}."""
        resp = self._send_line("list_workspaces")
        if resp == "OK":
            return []
        out: List[dict] = []
        for line in resp.splitlines():
            entry: dict = {"pinned": False, "color": None}
            for token in line.split(" "):
                if ":" not in token:
                    if token == "pinned":
                        entry["pinned"] = True
                    continue
                key, _, val = token.partition(":")
                if key == "id":
                    entry["id"] = int(val)
                elif key == "title":
                    entry["title"] = val.strip('"')
                elif key == "panes":
                    entry["panes"] = int(val)
                elif key.startswith("color="):
                    entry["color"] = key.split("=", 1)[1]
                elif key == "color" or token.startswith("color="):
                    entry["color"] = val or token.split("=", 1)[1]
            out.append(entry)
        return out

    def select_workspace(self, ws_id: int) -> None:
        self._expect_ok(self._send_line(f"select_workspace {ws_id}"))

    def close_workspace(self, ws_id: Optional[int] = None) -> None:
        cmd = "close_workspace" if ws_id is None else f"close_workspace {ws_id}"
        self._expect_ok(self._send_line(cmd))

    def current_workspace(self) -> Tuple[int, str]:
        payload = self._expect_ok(self._send_line("current_workspace"))
        # Payload format: "<id> <title words...>"
        head, _, rest = payload.partition(" ")
        return int(head), rest

    def rename_workspace(self, ws_id: int, title: str) -> None:
        self._expect_ok(self._send_line(f"rename_workspace {ws_id} {title}"))

    # --- commands: panes / splits ---------------------------------------

    def list_panes(self, ws_id: Optional[int] = None) -> List[int]:
        cmd = "list_panes" if ws_id is None else f"list_panes {ws_id}"
        payload = self._expect_ok(self._send_line(cmd))
        if not payload:
            return []
        return [int(x) for x in payload.split(",") if x]

    def focus_pane(self, pane_id: int) -> None:
        self._expect_ok(self._send_line(f"focus_pane {pane_id}"))

    def split_right(self) -> None:
        self._expect_ok(self._send_line("split_right"))

    def split_down(self) -> None:
        self._expect_ok(self._send_line("split_down"))

    # --- commands: surfaces / terminal ----------------------------------

    def list_surfaces(self) -> List[dict]:
        """Returns list of dicts for each surface/browser across all workspaces."""
        resp = self._send_line("list_surfaces")
        if resp == "OK":
            return []
        out: List[dict] = []
        for line in resp.splitlines():
            entry: dict = {}
            for token in line.split(" "):
                if ":" not in token:
                    if token in ("terminal", "browser"):
                        entry["kind"] = token
                    continue
                key, _, val = token.partition(":")
                if key in ("surface", "browser"):
                    entry["id"] = int(val)
                    entry["kind"] = key
                elif key == "workspace":
                    entry["workspace"] = int(val)
                elif key == "pane":
                    entry["pane"] = int(val)
                elif token.startswith("cwd="):
                    entry["cwd"] = token.split("=", 1)[1]
                elif token.startswith("url="):
                    entry["url"] = token.split("=", 1)[1]
            out.append(entry)
        return out

    def send(self, surface_id: int, text: str) -> None:
        self._expect_ok(self._send_line(f"send {surface_id} {text}"))

    def read_screen(self, surface_id: int) -> str:
        return self._expect_ok(self._send_line(f"read_screen {surface_id}"))
