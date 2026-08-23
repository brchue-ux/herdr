"""Tests for the read-only Herdr MCP connector."""

from __future__ import annotations

import base64
import io
import json
import os
import socket
import struct
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

from scripts import herdr_mcp


def _fake_png(width: int, height: int) -> bytes:
    """A minimal-but-valid PNG header: enough for `png_dimensions` to read."""
    return (
        herdr_mcp.PNG_SIGNATURE
        + struct.pack(">I", 13)
        + b"IHDR"
        + struct.pack(">II", width, height)
        + b"\x08\x02\x00\x00\x00"
        + b"\x00\x00\x00\x00"
    )


SNAPSHOT = {
    "version": "0.8.2",
    "protocol": 7,
    "focused_workspace_id": "ws-1",
    "workspaces": [
        {
            "workspace_id": "ws-1",
            "number": 1,
            "label": "herdr",
            "focused": True,
            "pane_count": 2,
            "tab_count": 1,
            "active_tab_id": "tab-1",
            "agent_status": "working",
            "tokens": {"owner": "firstmate"},
            "worktree": {
                "repo_key": "k",
                "repo_name": "herdr",
                "repo_root": "/repos/herdr",
                "checkout_path": "/repos/herdr-wt/task",
                "is_linked_worktree": True,
            },
        },
        {
            "workspace_id": "ws-2",
            "number": 2,
            "label": "notes",
            "focused": False,
            "pane_count": 1,
            "tab_count": 1,
            "active_tab_id": "tab-2",
            "agent_status": "idle",
        },
    ],
    "tabs": [
        {
            "tab_id": "tab-1",
            "workspace_id": "ws-1",
            "number": 1,
            "label": "main",
            "focused": True,
            "pane_count": 2,
            "agent_status": "working",
        },
        {
            "tab_id": "tab-2",
            "workspace_id": "ws-2",
            "number": 1,
            "label": "main",
            "focused": False,
            "pane_count": 1,
            "agent_status": "idle",
        },
    ],
    "panes": [
        {
            "pane_id": "pane-a",
            "terminal_id": "t-a",
            "workspace_id": "ws-1",
            "tab_id": "tab-1",
            "focused": True,
            "cwd": "/repos/herdr-wt/task",
            "label": "claude",
            "display_agent": "claude",
            "agent_status": "working",
            "unread": False,
            "owner": "ws-1",
            "tokens": {"summary": "building the connector"},
            "revision": 12,
        },
        {
            "pane_id": "pane-b",
            "terminal_id": "t-b",
            "workspace_id": "ws-1",
            "tab_id": "tab-1",
            "focused": False,
            "cwd": "/repos/herdr-wt/task",
            "agent_status": "idle",
            "unread": True,
            "revision": 3,
        },
        {
            "pane_id": "pane-c",
            "terminal_id": "t-c",
            "workspace_id": "ws-2",
            "tab_id": "tab-2",
            "focused": False,
            "agent_status": "unknown",
            "unread": False,
            "revision": 1,
        },
    ],
    "agents": [
        {
            "terminal_id": "t-a",
            "pane_id": "pane-a",
            "workspace_id": "ws-1",
            "tab_id": "tab-1",
            "agent_status": "working",
            "relation": "second_mate",
            "state_age_ms": 95_000,
            "focused": True,
            "state_change_seq": 4,
            "revision": 12,
        }
    ],
    "layouts": [],
    "machine_register": {
        "reading": True,
        "quantities": [
            {"name": "cpu", "value": 0.42, "history_samples": 60},
            {"name": "mem", "value": 0.31, "history_samples": 60},
        ],
    },
}


class FakeHerdrServer:
    """A Unix socket speaking Herdr's newline-delimited JSON API."""

    def __init__(self, responder):
        self._dir = tempfile.TemporaryDirectory()
        self.path = Path(self._dir.name) / "herdr.sock"
        self.requests: list[dict] = []
        self._responder = responder
        self._listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._listener.bind(str(self.path))
        self._listener.listen(8)
        self._stop = False
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    def _serve(self):
        while not self._stop:
            try:
                conn, _ = self._listener.accept()
            except OSError:
                return
            with conn:
                data = b""
                while b"\n" not in data:
                    chunk = conn.recv(4096)
                    if not chunk:
                        break
                    data += chunk
                if not data:
                    continue
                request = json.loads(data.split(b"\n", 1)[0])
                self.requests.append(request)
                response = self._responder(request)
                conn.sendall((json.dumps(response) + "\n").encode("utf-8"))

    def close(self):
        self._stop = True
        self._listener.close()
        self._dir.cleanup()


def snapshot_responder(request):
    if request["method"] == "session.snapshot":
        return {"id": request["id"], "result": {"type": "session_snapshot", "snapshot": SNAPSHOT}}
    if request["method"] == "pane.get":
        pane = next(
            (p for p in SNAPSHOT["panes"] if p["pane_id"] == request["params"]["pane_id"]), None
        )
        if pane is None:
            return {"id": request["id"], "error": {"code": "pane_not_found", "message": "no pane"}}
        return {"id": request["id"], "result": {"type": "pane_info", "pane": pane}}
    if request["method"] == "pane.process_info":
        return {
            "id": request["id"],
            "result": {
                "type": "pane_process_info",
                "process_info": {
                    "pane_id": request["params"]["pane_id"],
                    "shell_pid": 4242,
                    "tty": "/dev/pts/9",
                    "foreground_processes": [{"pid": 4243, "name": "claude", "cmdline": "claude"}],
                },
            },
        }
    if request["method"] == "pane.read":
        return {
            "id": request["id"],
            "result": {
                "type": "pane_read",
                "read": {
                    "pane_id": request["params"]["pane_id"],
                    "workspace_id": "ws-1",
                    "tab_id": "tab-1",
                    "source": request["params"]["source"],
                    "format": "text",
                    "text": "hello from the pane\n",
                    "revision": 12,
                    "truncated": False,
                },
            },
        }
    if request["method"] == "workspace.list":
        return {
            "id": request["id"],
            "result": {"type": "workspace_list", "workspaces": SNAPSHOT["workspaces"]},
        }
    return {"id": request["id"], "error": {"code": "unknown_method", "message": request["method"]}}


class ReadOnlyPolicyTests(unittest.TestCase):
    def test_allowlist_excludes_every_mutating_method(self):
        mutating = [
            "pane.close",
            "pane.send_text",
            "pane.send_keys",
            "pane.send_input",
            "pane.focus",
            "pane.split",
            "workspace.close",
            "workspace.create",
            "workspace.focus",
            "tab.close",
            "agent.start",
            "agent.prompt",
            "agent.send_keys",
            "server.stop",
            "server.live_handoff",
            "server.reload_config",
            "worktree.remove",
            "plugin.action.invoke",
            "plugin.unlink",
            "layout.apply",
            "session.status.set",
        ]
        for method in mutating:
            with self.subTest(method=method):
                self.assertFalse(herdr_mcp.is_read_only_method(method))

    def test_client_refuses_a_mutating_method_without_connecting(self):
        client = herdr_mcp.HerdrClient(session="nonexistent-session-name")
        with mock.patch.object(herdr_mcp.HerdrClient, "_roundtrip") as roundtrip:
            with self.assertRaises(herdr_mcp.HerdrApiError) as ctx:
                client.call("pane.close", {"pane_id": "pane-a"})
            roundtrip.assert_not_called()
        self.assertIn("read-only allowlist", str(ctx.exception))

    def test_agent_read_is_not_reachable(self):
        # agent.read always enters the alt-screen harvest, which injects input.
        self.assertNotIn("agent.read", herdr_mcp.READ_ONLY_METHODS)

    def test_pane_read_is_not_reachable_through_the_raw_query_tool(self):
        self.assertNotIn("pane.read", herdr_mcp.READ_ONLY_METHODS)
        with self.assertRaises(herdr_mcp.HerdrApiError):
            herdr_mcp.tool_herdr_query(lambda _s: None, {"method": "pane.read"})

    def test_pane_read_rejects_scrollback_sources(self):
        for source in ("recent", "recent_unwrapped"):
            with self.subTest(source=source):
                with self.assertRaises(herdr_mcp.HerdrApiError) as ctx:
                    herdr_mcp.tool_herdr_pane_read(
                        lambda _s: None, {"pane_id": "pane-a", "source": source}
                    )
                self.assertIn("not read-only", str(ctx.exception))

    def test_pane_read_tool_schema_only_offers_safe_sources(self):
        tool = next(t for t in herdr_mcp._tool_definitions() if t["name"] == "herdr_pane_read")
        self.assertEqual(
            tool["inputSchema"]["properties"]["source"]["enum"], list(herdr_mcp.SAFE_READ_SOURCES)
        )


class SocketResolutionTests(unittest.TestCase):
    def test_default_session_socket_matches_herdr_layout(self):
        with mock.patch.dict(os.environ, {"HOME": "/home/x"}, clear=True):
            self.assertEqual(
                herdr_mcp.api_socket_path(None), Path("/home/x/.config/herdr/herdr.sock")
            )

    def test_named_session_socket_matches_herdr_layout(self):
        with mock.patch.dict(os.environ, {"HOME": "/home/x"}, clear=True):
            self.assertEqual(
                herdr_mcp.api_socket_path("lab-1"),
                Path("/home/x/.config/herdr/sessions/lab-1/herdr.sock"),
            )

    def test_xdg_config_home_wins(self):
        with mock.patch.dict(os.environ, {"XDG_CONFIG_HOME": "/cfg", "HOME": "/home/x"}, clear=True):
            self.assertEqual(herdr_mcp.api_socket_path(None), Path("/cfg/herdr/herdr.sock"))

    def test_socket_override_wins_over_session(self):
        with mock.patch.dict(os.environ, {"HERDR_MCP_SOCKET": "/run/h.sock"}, clear=True):
            self.assertEqual(herdr_mcp.api_socket_path("lab-1"), Path("/run/h.sock"))

    def test_invalid_session_names_are_rejected(self):
        for name in ("", ".", "..", "a/b", "a b", "x" * 65):
            with self.subTest(name=name):
                with self.assertRaises(herdr_mcp.HerdrApiError):
                    herdr_mcp.validate_session_name(name)

    def test_session_listing_reports_stopped_sessions(self):
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "herdr" / "sessions" / "lab-1").mkdir(parents=True)
            (Path(tmp) / "herdr" / "sessions" / "bad name").mkdir(parents=True)
            with mock.patch.dict(os.environ, {"XDG_CONFIG_HOME": tmp}, clear=True):
                sessions = herdr_mcp.list_sessions()
        names = [entry["name"] for entry in sessions]
        self.assertEqual(names, ["default", "lab-1"])
        self.assertTrue(sessions[0]["default"])
        self.assertFalse(any(entry["running"] for entry in sessions))


class WireTests(unittest.TestCase):
    def setUp(self):
        self.server = FakeHerdrServer(snapshot_responder)
        self.addCleanup(self.server.close)
        patcher = mock.patch.dict(os.environ, {"HERDR_MCP_SOCKET": str(self.server.path)})
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_round_trip_sends_one_json_line_and_returns_the_result(self):
        client = herdr_mcp.HerdrClient()
        result = client.call("workspace.list")
        self.assertEqual(len(result["workspaces"]), 2)
        self.assertEqual(self.server.requests[-1]["method"], "workspace.list")
        self.assertEqual(self.server.requests[-1]["params"], {})
        self.assertIn("id", self.server.requests[-1])

    def test_api_error_becomes_a_python_error(self):
        client = herdr_mcp.HerdrClient()
        with self.assertRaises(herdr_mcp.HerdrApiError) as ctx:
            client.call("pane.get", {"pane_id": "missing"})
        self.assertIn("pane_not_found", str(ctx.exception))

    def test_missing_socket_reports_actionable_guidance(self):
        with mock.patch.dict(os.environ, {"HERDR_MCP_SOCKET": "/nonexistent/herdr.sock"}):
            with self.assertRaises(herdr_mcp.HerdrApiError) as ctx:
                herdr_mcp.HerdrClient().call("ping")
        self.assertIn("herdr_sessions", str(ctx.exception))

    def test_overview_tool_renders_the_live_snapshot(self):
        text = herdr_mcp.tool_herdr_overview(lambda s: herdr_mcp.HerdrClient(s), {})
        self.assertIn("workspace 1 'herdr'", text)
        self.assertIn("pane <pane-a>", text)
        self.assertIn("agent=claude", text)
        self.assertIn("status=working", text)
        self.assertIn("linked worktree: /repos/herdr-wt/task", text)

    def test_pane_tool_merges_pane_and_process_info(self):
        text = herdr_mcp.tool_herdr_pane(
            lambda s: herdr_mcp.HerdrClient(s), {"pane_id": "pane-a"}
        )
        self.assertIn("pane <pane-a>", text)
        self.assertIn("shell_pid: 4242", text)
        self.assertIn("pid 4243: claude", text)

    def test_pane_read_defaults_to_the_visible_screen(self):
        text = herdr_mcp.tool_herdr_pane_read(
            lambda s: herdr_mcp.HerdrClient(s), {"pane_id": "pane-a"}
        )
        self.assertEqual(text.strip(), "hello from the pane")
        self.assertEqual(self.server.requests[-1]["params"]["source"], "visible")


class RenderTests(unittest.TestCase):
    def test_overview_groups_panes_under_their_tab_and_workspace(self):
        text = herdr_mcp.render_overview(SNAPSHOT)
        self.assertIn("2 workspace(s), 2 tab(s), 3 pane(s)", text)
        self.assertLess(text.index("workspace 1"), text.index("pane <pane-a>"))
        self.assertLess(text.index("pane <pane-b>"), text.index("workspace 2"))
        self.assertIn("relation=second_mate", text)
        self.assertIn("held_for=95s", text)
        self.assertIn("machine: cpu=42%, mem=31%", text)

    def test_overview_filters_to_one_workspace(self):
        by_label = herdr_mcp.render_overview(SNAPSHOT, "notes")
        self.assertIn("pane <pane-c>", by_label)
        self.assertNotIn("pane <pane-a>", by_label)
        self.assertIn("pane <pane-a>", herdr_mcp.render_overview(SNAPSHOT, "ws-1"))
        self.assertIn("pane <pane-c>", herdr_mcp.render_overview(SNAPSHOT, "2"))

    def test_overview_reports_an_unknown_workspace_filter(self):
        self.assertIn("no workspace matching", herdr_mcp.render_overview(SNAPSHOT, "nope"))

    def test_overview_survives_an_empty_session(self):
        text = herdr_mcp.render_overview({"version": "0.8.2", "protocol": 7})
        self.assertIn("0 workspace(s), 0 tab(s), 0 pane(s)", text)

    def test_home_paths_are_abbreviated(self):
        with mock.patch.dict(os.environ, {"HOME": "/home/x"}):
            self.assertEqual(herdr_mcp._home_relative("/home/x/repos/a"), "~/repos/a")
            self.assertEqual(herdr_mcp._home_relative("/home/xyz/a"), "/home/xyz/a")


class DisplayResolutionTests(unittest.TestCase):
    def test_prefers_the_dedicated_env_var(self):
        with mock.patch.dict(os.environ, {"HERDR_MCP_DISPLAY": ":7", "DISPLAY": ":0"}):
            self.assertEqual(herdr_mcp.resolve_display(), ":7")

    def test_falls_back_to_display(self):
        with mock.patch.dict(os.environ, {"DISPLAY": ":0"}, clear=True):
            self.assertEqual(herdr_mcp.resolve_display(), ":0")

    def test_raises_with_no_display_configured(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            with self.assertRaises(herdr_mcp.HerdrApiError):
                herdr_mcp.resolve_display()


class PngDimensionsTests(unittest.TestCase):
    def test_reads_width_and_height_from_ihdr(self):
        self.assertEqual(herdr_mcp.png_dimensions(_fake_png(1600, 1000)), (1600, 1000))

    def test_rejects_non_png_bytes(self):
        with self.assertRaises(herdr_mcp.HerdrApiError):
            herdr_mcp.png_dimensions(b"not a png")


class PaneScreenshotTests(unittest.TestCase):
    """`herdr_pane_screenshot`'s Herdr-side logic: target resolution, the focus
    guard, and the cell-to-pixel crop math. The actual pixel capture
    (`capture_display_png` / `crop_png`) is mocked throughout -- it shells out
    to ImageMagick against a real X display, which this suite has neither."""

    RECTS = {
        "pane-a": {"x": 0, "y": 0, "width": 40, "height": 24},
        "pane-b": {"x": 40, "y": 0, "width": 40, "height": 24},
        "pane-c": {"x": 0, "y": 0, "width": 80, "height": 24},
    }

    def _responder(self, *, graphics_info_ok=True):
        def responder(request):
            method = request["method"]
            if method == "session.snapshot":
                return {
                    "id": request["id"],
                    "result": {"type": "session_snapshot", "snapshot": SNAPSHOT},
                }
            if method == "pane.layout":
                pane_id = request["params"]["pane_id"]
                pane = next(p for p in SNAPSHOT["panes"] if p["pane_id"] == pane_id)
                tab_id = pane["tab_id"]
                tab_panes = [p for p in SNAPSHOT["panes"] if p["tab_id"] == tab_id]
                layout = {
                    "workspace_id": pane["workspace_id"],
                    "tab_id": tab_id,
                    "zoomed": False,
                    "area": {"x": 0, "y": 0, "width": 80, "height": 24},
                    "focused_pane_id": tab_panes[0]["pane_id"],
                    "panes": [
                        {
                            "pane_id": p["pane_id"],
                            "focused": p["pane_id"] == tab_panes[0]["pane_id"],
                            "rect": self.RECTS[p["pane_id"]],
                        }
                        for p in tab_panes
                    ],
                    "splits": [],
                }
                return {"id": request["id"], "result": {"type": "pane_layout", "layout": layout}}
            if method == "pane.graphics.info":
                if not graphics_info_ok:
                    return {
                        "id": request["id"],
                        "error": {
                            "code": "cell_size_unavailable",
                            "message": "host cell size is unavailable",
                        },
                    }
                return {
                    "id": request["id"],
                    "result": {
                        "type": "pane_graphics_info",
                        "cell_width_px": 10,
                        "cell_height_px": 20,
                    },
                }
            return {"id": request["id"], "error": {"code": "unknown_method", "message": method}}

        return responder

    def _serve(self, *, graphics_info_ok=True):
        server = FakeHerdrServer(self._responder(graphics_info_ok=graphics_info_ok))
        self.addCleanup(server.close)
        patcher = mock.patch.dict(os.environ, {"HERDR_MCP_SOCKET": str(server.path)})
        patcher.start()
        self.addCleanup(patcher.stop)
        return server

    @staticmethod
    def _client_factory(session):
        return herdr_mcp.HerdrClient(session)

    def test_requires_exactly_one_target(self):
        self._serve()
        for args in ({}, {"pane_id": "pane-a", "tab_id": "tab-1"}):
            with self.subTest(args=args):
                with self.assertRaises(herdr_mcp.HerdrApiError) as ctx:
                    herdr_mcp.tool_herdr_pane_screenshot(self._client_factory, args)
                self.assertIn("exactly one of", str(ctx.exception))

    def test_refuses_a_target_not_currently_shown(self):
        # pane-c lives in ws-2/tab-2, both focused=False in SNAPSHOT.
        self._serve()
        with self.assertRaises(herdr_mcp.HerdrApiError) as ctx:
            herdr_mcp.tool_herdr_pane_screenshot(self._client_factory, {"pane_id": "pane-c"})
        self.assertIn("not the workspace/tab currently shown", str(ctx.exception))

    def test_whole_window_capture_needs_no_cell_geometry(self):
        self._serve()
        fake_png = _fake_png(800, 600)
        with (
            mock.patch.object(herdr_mcp, "resolve_display", return_value=":99"),
            mock.patch.object(herdr_mcp, "capture_display_png", return_value=fake_png) as capture,
            mock.patch.object(herdr_mcp, "crop_png") as crop,
        ):
            content = herdr_mcp.tool_herdr_pane_screenshot(self._client_factory, {"tab_id": "tab-1"})
        capture.assert_called_once_with(":99")
        crop.assert_not_called()
        image = next(b for b in content if b["type"] == "image")
        self.assertEqual(image["mimeType"], "image/png")
        self.assertEqual(base64.b64decode(image["data"]), fake_png)
        text = next(b for b in content if b["type"] == "text")
        self.assertIn("800x600px", text["text"])
        self.assertIn("whole window", text["text"])

    def test_pane_capture_crops_to_the_pane_rect_in_pixels(self):
        self._serve()
        fake_png = _fake_png(800, 600)
        cropped_png = _fake_png(400, 480)
        with (
            mock.patch.object(herdr_mcp, "resolve_display", return_value=":99"),
            mock.patch.object(herdr_mcp, "capture_display_png", return_value=fake_png),
            mock.patch.object(herdr_mcp, "crop_png", return_value=cropped_png) as crop,
        ):
            content = herdr_mcp.tool_herdr_pane_screenshot(
                self._client_factory, {"pane_id": "pane-a"}
            )
        # pane-a's rect is 0,0,40x24 cells at 10x20px/cell.
        crop.assert_called_once_with(fake_png, (0, 0, 400, 480))
        image = next(b for b in content if b["type"] == "image")
        self.assertEqual(base64.b64decode(image["data"]), cropped_png)
        text = next(b for b in content if b["type"] == "text")
        self.assertIn("400x480px", text["text"])
        self.assertIn("pane pane-a", text["text"])

    def test_missing_cell_size_refuses_the_crop_with_a_clear_reason(self):
        self._serve(graphics_info_ok=False)
        with self.assertRaises(herdr_mcp.HerdrApiError) as ctx:
            herdr_mcp.tool_herdr_pane_screenshot(self._client_factory, {"pane_id": "pane-a"})
        self.assertIn("experimental.kitty_graphics", str(ctx.exception))

    def test_tool_schema_exposes_all_three_addressing_modes(self):
        tool = next(
            t for t in herdr_mcp._tool_definitions() if t["name"] == "herdr_pane_screenshot"
        )
        props = tool["inputSchema"]["properties"]
        self.assertIn("pane_id", props)
        self.assertIn("workspace_id", props)
        self.assertIn("tab_id", props)

    def test_not_reachable_through_herdr_query(self):
        # It is a composite Python tool, like herdr_overview/herdr_pane, not a
        # Herdr API method -- so it was never a candidate for READ_ONLY_METHODS.
        self.assertNotIn("herdr_pane_screenshot", herdr_mcp.READ_ONLY_METHODS)


class McpProtocolTests(unittest.TestCase):
    def setUp(self):
        self.server = herdr_mcp.McpServer(client_factory=lambda _s: None)

    def test_initialize_echoes_a_supported_protocol_version(self):
        response = self.server.handle(
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}}
        )
        self.assertEqual(response["result"]["protocolVersion"], "2024-11-05")
        self.assertEqual(response["result"]["serverInfo"]["name"], "herdr")
        self.assertIn("tools", response["result"]["capabilities"])

    def test_initialize_falls_back_for_an_unknown_protocol_version(self):
        response = self.server.handle(
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "1999-01-01"}}
        )
        self.assertEqual(
            response["result"]["protocolVersion"], herdr_mcp.DEFAULT_PROTOCOL_VERSION
        )

    def test_notifications_get_no_response(self):
        self.assertIsNone(
            self.server.handle({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )

    def test_tools_list_exposes_every_tool_with_a_schema(self):
        response = self.server.handle({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        tools = response["result"]["tools"]
        self.assertEqual({tool["name"] for tool in tools}, set(herdr_mcp.TOOLS))
        for tool in tools:
            self.assertEqual(tool["inputSchema"]["type"], "object")
            self.assertTrue(tool["description"])

    def test_query_tool_schema_lists_only_allowlisted_methods(self):
        tool = next(t for t in herdr_mcp._tool_definitions() if t["name"] == "herdr_query")
        self.assertEqual(
            tool["inputSchema"]["properties"]["method"]["enum"], sorted(herdr_mcp.READ_ONLY_METHODS)
        )

    def test_unknown_tool_is_a_tool_error_not_a_protocol_error(self):
        response = self.server.handle(
            {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "herdr_nuke"}}
        )
        self.assertTrue(response["result"]["isError"])
        self.assertIn("unknown tool", response["result"]["content"][0]["text"])

    def test_unknown_rpc_method_is_a_protocol_error(self):
        response = self.server.handle({"jsonrpc": "2.0", "id": 4, "method": "nope/nope"})
        self.assertEqual(response["error"]["code"], -32601)

    def test_serve_reads_lines_and_writes_one_response_per_request(self):
        stdin = io.StringIO(
            json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
            + "\n"
            + json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"})
            + "\n"
            + json.dumps({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
            + "\n"
        )
        stdout = io.StringIO()
        self.server.serve(stdin, stdout)
        lines = [line for line in stdout.getvalue().splitlines() if line]
        self.assertEqual(len(lines), 2)
        self.assertEqual(json.loads(lines[0])["id"], 1)
        self.assertEqual(json.loads(lines[1])["id"], 2)

    def test_malformed_input_does_not_kill_the_server(self):
        stdin = io.StringIO(
            "{not json\n" + json.dumps({"jsonrpc": "2.0", "id": 9, "method": "ping"}) + "\n"
        )
        stdout = io.StringIO()
        self.server.serve(stdin, stdout)
        lines = [json.loads(line) for line in stdout.getvalue().splitlines() if line]
        self.assertEqual(lines[0]["error"]["code"], -32700)
        self.assertEqual(lines[1]["id"], 9)

    def test_a_refused_call_is_reported_to_the_model_as_a_tool_error(self):
        response = self.server.handle(
            {
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {"name": "herdr_query", "arguments": {"method": "pane.close"}},
            }
        )
        self.assertTrue(response["result"]["isError"])
        self.assertIn("read-only allowlist", response["result"]["content"][0]["text"])


class EndToEndStdioTests(unittest.TestCase):
    """Drive the server the way an MCP client does, over a real Herdr socket."""

    def test_full_handshake_and_overview_over_stdio(self):
        server_socket = FakeHerdrServer(snapshot_responder)
        self.addCleanup(server_socket.close)
        with mock.patch.dict(os.environ, {"HERDR_MCP_SOCKET": str(server_socket.path)}):
            stdin = io.StringIO(
                "\n".join(
                    json.dumps(message)
                    for message in (
                        {
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "initialize",
                            "params": {"protocolVersion": "2025-06-18"},
                        },
                        {"jsonrpc": "2.0", "method": "notifications/initialized"},
                        {
                            "jsonrpc": "2.0",
                            "id": 2,
                            "method": "tools/call",
                            "params": {"name": "herdr_overview", "arguments": {}},
                        },
                    )
                )
                + "\n"
            )
            stdout = io.StringIO()
            herdr_mcp.McpServer().serve(stdin, stdout)

        responses = [json.loads(line) for line in stdout.getvalue().splitlines() if line]
        self.assertEqual(len(responses), 2)
        overview = responses[1]["result"]
        self.assertFalse(overview["isError"])
        self.assertIn("pane <pane-a>", overview["content"][0]["text"])
        self.assertEqual([r["method"] for r in server_socket.requests], ["session.snapshot"])


if __name__ == "__main__":
    unittest.main()
